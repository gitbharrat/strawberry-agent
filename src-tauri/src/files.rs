//! File browser, viewer and editor support.
//!
//! Directory listings are one level deep on purpose: the tree expands lazily, so
//! opening a repository with a hundred thousand files costs the same as opening
//! an empty one.

use crate::error::{AppError, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Files above this size are not offered to the editor. Well past any real
/// source file, and small enough that reading one never stalls the UI.
const MAX_EDIT_BYTES: u64 = 4 * 1024 * 1024;
const SNIFF_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: i64,
    pub hidden: bool,
    /// Has children — lets the UI show a disclosure arrow without a second call.
    pub expandable: bool,
}

pub fn list_dir(path: &Path, show_hidden: bool) -> Result<Vec<FsEntry>> {
    let rd = std::fs::read_dir(path).map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    let mut out = Vec::new();

    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let hidden = name.starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        // Never worth walking into, and listing them buries the real tree.
        if matches!(name.as_str(), "node_modules" | "target" | ".git" | "dist" | ".next" | "__pycache__")
            && !show_hidden
        {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = meta.is_dir();
        out.push(FsEntry {
            name,
            path: entry.path().display().to_string(),
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            hidden,
            expandable: is_dir,
        });
    }

    // Directories first, then case-insensitive by name — the ordering people
    // expect from a file tree.
    out.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContents {
    pub path: String,
    pub name: String,
    pub content: String,
    pub language: String,
    pub size: u64,
    pub lines: usize,
    pub binary: bool,
    pub truncated: bool,
    pub read_only: bool,
    pub hash: String,
    pub modified: i64,
}

pub fn read_file(path: &Path) -> Result<FileContents> {
    let meta = std::fs::metadata(path).map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    if meta.is_dir() {
        return Err(AppError::msg(format!("{} is a directory", path.display())));
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let language = language_for(&name);
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let too_big = meta.len() > MAX_EDIT_BYTES;
    let bytes = if too_big {
        // Read only the head so the viewer can still show something useful.
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let mut buf = vec![0u8; MAX_EDIT_BYTES as usize];
        let n = f.read(&mut buf)?;
        buf.truncate(n);
        buf
    } else {
        std::fs::read(path)?
    };

    let binary = bytes.iter().take(SNIFF_BYTES).any(|b| *b == 0);
    let content = if binary { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() };

    Ok(FileContents {
        path: path.display().to_string(),
        name,
        lines: content.lines().count(),
        hash: crate::journal::content_hash(&bytes),
        content,
        language,
        size: meta.len(),
        binary,
        truncated: too_big,
        read_only: too_big || binary || meta.permissions().readonly(),
        modified,
    })
}

/// Saves an edit made in the app's own editor.
///
/// `expected_hash` guards against clobbering a change made outside the app
/// between opening the file and saving it.
pub async fn write_file(
    db: &crate::db::Db,
    path: &Path,
    content: &str,
    expected_hash: Option<&str>,
    chat_id: Option<&str>,
) -> Result<FileContents> {
    let previous = tokio::fs::read(path).await.ok();

    if let (Some(expected), Some(current)) = (expected_hash, previous.as_deref()) {
        let actual = crate::journal::content_hash(current);
        if actual != expected {
            return Err(AppError::msg(
                "This file changed on disk since it was opened. Reload it before saving.",
            ));
        }
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content.as_bytes()).await?;

    // Manual edits join the same journal as agent edits, so they show up in the
    // change list and can be reverted the same way.
    if let Some(chat) = chat_id {
        crate::journal::record(
            db,
            crate::journal::Provenance::manual(chat),
            path,
            previous.as_deref(),
            content.as_bytes(),
        )
        .await?;
    }

    read_file(path)
}

/// Deletes a path from the file browser. Files are journalled first (with a
/// snapshot) so the deletion shows in the chat's change list and can be
/// undone; the returned id is that change. Directories only go when empty
/// unless `recursive` — a recursive removal is not journalled and says so in
/// the UI before it happens.
pub async fn delete_path(
    db: &crate::db::Db,
    path: &Path,
    chat_id: Option<&str>,
    recursive: bool,
) -> Result<Option<String>> {
    let meta =
        tokio::fs::metadata(path).await.map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;

    if meta.is_dir() {
        if recursive {
            tokio::fs::remove_dir_all(path).await?;
        } else {
            tokio::fs::remove_dir(path).await.map_err(|e| {
                if e.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                    AppError::msg(format!("{} is not empty", path.display()))
                } else {
                    AppError::msg(format!("{}: {e}", path.display()))
                }
            })?;
        }
        return Ok(None);
    }

    let before = tokio::fs::read(path).await?;
    tokio::fs::remove_file(path).await?;
    match chat_id {
        Some(chat) => {
            let id =
                crate::journal::record_delete(db, crate::journal::Provenance::manual(chat), path, &before)
                    .await?;
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

/// Maps an extension to the highlighter's language id.
pub fn language_for(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let base = Path::new(&lower).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let by_name = match base.as_str() {
        "dockerfile" => Some("docker"),
        "makefile" => Some("makefile"),
        "cargo.lock" => Some("toml"),
        ".gitignore" | ".dockerignore" | ".npmignore" => Some("ignore"),
        _ => None,
    };
    if let Some(l) = by_name {
        return l.into();
    }

    let ext = Path::new(&lower).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let lang = match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" | "zsh" | "fish" => "bash",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "svg" => "xml",
        "md" | "markdown" | "mdx" => "markdown",
        "lua" => "lua",
        "zig" => "zig",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "graphql" | "gql" => "graphql",
        "ini" | "cfg" | "conf" | "env" => "ini",
        "diff" | "patch" => "diff",
        _ => "text",
    };
    lang.into()
}

/// Resolves a UI-supplied path, expanding `~`.
pub fn resolve(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(raw),
    }
}

pub fn home_dir() -> String {
    dirs::home_dir().unwrap_or_default().display().to_string()
}
