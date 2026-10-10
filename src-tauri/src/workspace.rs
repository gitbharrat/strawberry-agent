//! Working-folder status: what directory a chat operates in, and what git
//! knows about it.
//!
//! The branch is read straight from `.git/HEAD` rather than by shelling out —
//! it is one small file read, so the status strip can refresh freely without
//! spawning a process. Only the dirty count needs `git`, and that call is
//! deadline-bounded so a huge or networked repository cannot stall the UI.

use crate::error::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub cwd: Option<String>,
    pub exists: bool,
    pub is_repo: bool,
    /// Branch name, or a short commit id when the head is detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// Modified, added and untracked entries. `None` if git did not answer in time.
    pub dirty: Option<usize>,
    pub git_available: bool,
}

/// Walks up from `dir` looking for a `.git` entry, the way git itself does.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// Reads the current branch from `.git/HEAD`.
///
/// Handles the worktree and submodule case, where `.git` is a file pointing at
/// the real git directory rather than being one.
fn head_of(root: &Path) -> Option<(String, bool)> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let rel = text.strip_prefix("gitdir:")?.trim();
        let p = PathBuf::from(rel);
        if p.is_absolute() {
            p
        } else {
            root.join(p)
        }
    } else {
        dot_git
    };

    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Some((branch.to_string(), false)),
        // Detached head: HEAD holds the commit id directly.
        None => Some((head.chars().take(8).collect(), true)),
    }
}

async fn dirty_count(root: &Path) -> Option<usize> {
    let child = tokio::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();

    let out = tokio::time::timeout(GIT_TIMEOUT, child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).count())
}

pub async fn status(cwd: Option<String>) -> WorkspaceStatus {
    let git_available = which_git();
    let Some(cwd) = cwd else {
        return WorkspaceStatus {
            cwd: None,
            exists: false,
            is_repo: false,
            branch: None,
            detached: false,
            dirty: None,
            git_available,
        };
    };

    if cwd.starts_with("ssh://") {
        // Remote workspace: reachable-or-not is the tools' business; git
        // status over the wire is not worth a round trip per repaint.
        return WorkspaceStatus {
            cwd: Some(cwd),
            exists: true,
            is_repo: false,
            branch: None,
            detached: false,
            dirty: None,
            git_available,
        };
    }

    let path = crate::files::resolve(&cwd);
    let exists = path.is_dir();
    let root = if exists { repo_root(&path) } else { None };

    let (branch, detached) = root.as_deref().and_then(head_of).map_or((None, false), |(b, d)| (Some(b), d));
    let dirty = match (&root, git_available) {
        (Some(r), true) => dirty_count(r).await,
        _ => None,
    };

    WorkspaceStatus {
        cwd: Some(path.display().to_string()),
        exists,
        is_repo: root.is_some(),
        branch,
        detached,
        dirty,
        git_available,
    }
}

fn which_git() -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join("git");
                candidate.is_file() || candidate.with_extension("exe").is_file()
            })
        })
        .unwrap_or(false)
}

/// Initialises a repository in `dir`. Returns the resulting status.
pub async fn init(dir: &str) -> Result<WorkspaceStatus> {
    let path = crate::files::resolve(dir);
    let out = tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::process::Command::new("git")
            .arg("init")
            .current_dir(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| crate::error::AppError::Timeout(GIT_TIMEOUT.as_millis() as u64))?
    .map_err(|e| crate::error::AppError::msg(format!("could not run git: {e}")))?;

    if !out.status.success() {
        return Err(crate::error::AppError::msg(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(status(Some(path.display().to_string())).await)
}
