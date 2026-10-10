//! Architecture analysis.
//!
//! Builds and maintains a living map of the workspace: which files exist, how
//! they depend on each other, what every function does, and how healthy each
//! one looks. Two layers, deliberately separate:
//!
//!  * a **static scan** (free, instant) extracts the graph, functions and
//!    quality heuristics;
//!  * the **selected model** writes the semantic layer — short descriptions,
//!    inputs/outputs, and notes on math kernels and routing hubs.
//!
//! Everything persists under `.strawberry/` in the workspace:
//! `architecture.json` is the full document the UI renders, and
//! `architecture.md` is the compact summary injected into every chat's system
//! prompt so the agent always knows the terrain. After a run that edits files,
//! the map re-scans itself incrementally and each touched function gains a
//! history entry.

pub mod enrich;
pub mod scan;

use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const DIR: &str = ".strawberry";
pub const JSON_FILE: &str = "architecture.json";
pub const MD_FILE: &str = "architecture.md";

/// Ceiling for what the system prompt carries. Beyond this the map is
/// truncated at a line boundary — context is precious.
const PROMPT_BUDGET: usize = 7000;
/// Ceiling for the generated markdown file itself.
const MD_BUDGET: usize = 11_000;
/// Functions enriched in one background auto-update. Keeps the after-run cost
/// at a request or two.
const AUTO_ENRICH_CAP: usize = 48;

// ---------------------------------------------------------------------- types

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchDoc {
    pub version: u32,
    pub root: String,
    pub generated_at: i64,
    /// Model that wrote the semantic layer, when enrichment has run.
    pub model: Option<String>,
    pub enriched: bool,
    pub file_count: u32,
    pub fn_count: u32,
    pub files: Vec<ArchFile>,
    pub edges: Vec<ArchEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchFile {
    /// Workspace-relative path; doubles as the node id.
    pub id: String,
    pub lang: String,
    pub lines: u32,
    pub hash: String,
    pub quality: u8,
    pub lints: Vec<String>,
    /// Model-written one-liner for what this file is.
    pub role: Option<String>,
    /// Architectural kind: entry | orchestrator | ui | state | model | util |
    /// io | config | test. Seeded by graph-shape heuristics, refined by the
    /// model during enrichment.
    pub kind: Option<String>,
    pub imports: Vec<String>,
    /// Service channels this file provides: bound HTTP routes, subscribed
    /// topics and events. Matched by string against `consumes` elsewhere.
    pub provides: Vec<String>,
    /// Service channels this file consumes: called routes, published events.
    pub consumes: Vec<String>,
    /// Files that import or call into this one — the reverse of `imports`.
    pub used_by: Vec<String>,
    pub in_degree: u32,
    /// Undirected weighted degree: total coupling in both directions.
    pub degree: u32,
    /// Top slice of the codebase by degree.
    pub hub: bool,
    /// Worst function complexity in the file.
    pub max_cx: u32,
    pub functions: Vec<ArchFn>,
    pub log: Vec<FnLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchFn {
    pub name: String,
    pub line: u32,
    pub end_line: u32,
    pub params: String,
    pub exported: bool,
    pub complexity: u32,
    pub nesting: u32,
    pub quality: u8,
    pub lints: Vec<String>,
    pub calls: Vec<String>,
    /// Identifier fingerprint for cross-file similarity in the UI.
    pub tokens: Vec<String>,
    pub hash: String,
    /// Static signal ("math" | "routing") that seeds the model's tagging.
    pub hint: Option<String>,
    /// Referenced from several other files — a load-bearing function.
    pub core: bool,
    /// No reference to it anywhere in the scan — a dead-code candidate.
    pub dead: bool,
    /// Body changed since the description was written.
    pub stale: bool,
    pub desc: Option<String>,
    pub inputs: Option<String>,
    pub outputs: Option<String>,
    pub tag: Option<String>,
    pub note: Option<String>,
    /// Model-estimated time complexity over the dominant input, e.g. "O(n log n)".
    pub big_o: Option<String>,
    /// Model-estimated extra space, e.g. "O(1)".
    pub space: Option<String>,
    /// Precomputed lookalikes elsewhere in the codebase, best first.
    pub similar: Vec<SimilarRef>,
    pub log: Vec<FnLog>,
}

/// A cross-reference to a similar function: `target` is "path::name".
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SimilarRef {
    pub target: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FnLog {
    pub ts: i64,
    /// "indexed" | "added" | "modified" | "removed <name>"
    pub event: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchEdge {
    pub from: String,
    pub to: String,
    pub weight: u32,
    /// Channel labels when this edge (also) runs over a route or event —
    /// e.g. "route /api/alpha", "event order.created". Empty = imports only.
    #[serde(default)]
    pub via: Vec<String>,
}

/// Progress events streamed to the UI while a scan runs.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AnalysisEvent {
    #[serde(rename_all = "camelCase")]
    Phase { phase: String, message: String },
    #[serde(rename_all = "camelCase")]
    Progress { done: u32, total: u32 },
    #[serde(rename_all = "camelCase")]
    Notice { text: String },
}

/// What the UI asks for on open: where the map lives and what it holds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSnapshot {
    pub root: Option<String>,
    pub doc: Option<ArchDoc>,
}

// ----------------------------------------------------------------- lifecycle

/// The raw working directory for a chat: its own cwd, else the project's.
/// May be a local path or an `ssh://<host>/<dir>` remote workspace.
pub fn chat_cwd(state: &AppState, chat_id: &str) -> Option<String> {
    let chat = state.db.get_chat(chat_id).ok()?;
    chat.cwd.or_else(|| {
        chat.project_id.as_deref().and_then(|id| state.db.get_project(id).ok()).and_then(|p| p.root_path)
    })
}

/// The *local* workspace root for a chat. `None` for remote workspaces —
/// callers that can handle those go through `chat_cwd` instead.
pub fn chat_root(state: &AppState, chat_id: &str) -> Option<PathBuf> {
    let path = PathBuf::from(chat_cwd(state, chat_id)?);
    path.is_dir().then_some(path)
}

/// The directory holding `architecture.json`/`.md` for a workspace. Local
/// workspaces keep it inside the folder (`.strawberry/`); remote ones mirror
/// it under the app's data dir — keyed by host and path, so every chat bound
/// to the same remote folder shares one map — which keeps prompt assembly
/// synchronous and readable offline.
pub fn map_dir(cwd: &str) -> PathBuf {
    match crate::remote::parse(cwd) {
        Some(t) => crate::db::data_dir()
            .join("remote-arch")
            .join(format!("{}-{}", t.host_id, crate::journal::content_hash(t.root.as_bytes()))),
        None => crate::files::resolve(cwd).join(DIR),
    }
}

pub async fn load_doc(root: &Path) -> Option<ArchDoc> {
    load_doc_at(&root.join(DIR)).await
}

/// Loads the document from a map directory (see `map_dir`).
pub async fn load_doc_at(dir: &Path) -> Option<ArchDoc> {
    let raw = tokio::fs::read_to_string(dir.join(JSON_FILE)).await.ok()?;
    serde_json::from_str(&raw).ok()
}

/// Pulls the workspace's own copy of the map — written there by a scan,
/// possibly from another machine — and refreshes the local mirror.
/// Best-effort by design: any failure just means "no map yet".
pub async fn fetch_remote_doc(state: &AppState, cwd: &str) -> Option<ArchDoc> {
    let t = crate::remote::parse(cwd)?;
    let r = crate::remote::pooled(state, &t.host_id).await.ok()?;
    let path = format!("{}/{DIR}/{JSON_FILE}", t.root.trim_end_matches('/'));
    let bytes = r.read(&path, crate::remote::MAX_REMOTE_READ).await.ok()?;
    let doc: ArchDoc = serde_json::from_slice(&bytes).ok()?;
    let dir = map_dir(cwd);
    if tokio::fs::create_dir_all(&dir).await.is_ok() {
        let _ = tokio::fs::write(dir.join(JSON_FILE), &bytes).await;
        let _ = tokio::fs::write(dir.join(MD_FILE), render_md(&doc)).await;
    }
    Some(doc)
}

async fn persist(root: &Path, doc: &ArchDoc) -> Result<()> {
    let dir = root.join(DIR);
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join(JSON_FILE), serde_json::to_string(doc)?).await?;
    tokio::fs::write(dir.join(MD_FILE), render_md(doc)).await?;
    Ok(())
}

/// Persists a remote workspace's map: the local mirror is the copy the prompt
/// and the pane read, so it must succeed; the copy into the workspace's own
/// `.strawberry/` is for other machines and the remote's tooling, and a
/// failure there only costs sharing.
async fn persist_remote(
    r: &crate::remote::Remote,
    remote_root: &str,
    mirror: &Path,
    doc: &ArchDoc,
) -> Result<()> {
    let json = serde_json::to_string(doc)?;
    let md = render_md(doc);
    tokio::fs::create_dir_all(mirror).await?;
    tokio::fs::write(mirror.join(JSON_FILE), &json).await?;
    tokio::fs::write(mirror.join(MD_FILE), &md).await?;

    let base = format!("{}/{DIR}", remote_root.trim_end_matches('/'));
    for (name, body) in [(JSON_FILE, json.as_bytes()), (MD_FILE, md.as_bytes())] {
        if let Err(e) = r.write(&format!("{base}/{name}"), body).await {
            tracing::warn!("could not write {base}/{name}: {e}");
            break;
        }
    }
    Ok(())
}

/// Ceiling on the bytes a remote gather brings over in one scan. Per-file and
/// file-count caps mirror the local scanner's.
const REMOTE_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const REMOTE_GATHER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// Fetches every scannable source in the remote workspace in a single exec
/// round trip: one shell loop enumerates (honouring `.gitignore` via
/// `git ls-files` when the folder is a repo), filters, and streams each file
/// behind a per-scan boundary line.
async fn gather_remote_sources(
    r: &crate::remote::Remote,
    remote_root: &str,
    cancel: &CancellationToken,
) -> Result<Vec<scan::SourceFile>> {
    let boundary = format!("=====STRAWBERRY-{}=====", crate::db::new_id());
    let cmd = gather_cmd(&boundary);
    let exec = r.exec(remote_root, &cmd);
    let (out, code) = tokio::select! {
        _ = cancel.cancelled() => return Err(AppError::Cancelled),
        res = tokio::time::timeout(REMOTE_GATHER_TIMEOUT, exec) => {
            res.map_err(|_| AppError::Timeout(REMOTE_GATHER_TIMEOUT.as_millis() as u64))??
        }
    };
    let files = parse_gathered(&out, &boundary);
    if files.is_empty() && code != 0 {
        return Err(AppError::msg(format!(
            "listing sources on {} failed (exit {code}): {}",
            r.host.label,
            out.trim().chars().take(200).collect::<String>()
        )));
    }
    Ok(files)
}

/// The one-round-trip gather script. POSIX sh only — the remote login shell
/// may be dash. Filters mirror the local walker: source extensions, vendored
/// directories, generated files, per-file size, file count, and a total-bytes
/// budget so a monorepo cannot flood the connection.
fn gather_cmd(boundary: &str) -> String {
    let exts = scan::SOURCE_EXTS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join("|");
    let vend = scan::VENDORED_DIRS.iter().map(|d| format!("*/{d}/*")).collect::<Vec<_>>().join("|");
    let finds =
        scan::VENDORED_DIRS.iter().map(|d| format!("-not -path '*/{d}/*'")).collect::<Vec<_>>().join(" ");
    format!(
        r#"if git rev-parse --git-dir >/dev/null 2>&1; then git ls-files --cached --others --exclude-standard; else find . -type f -not -path '*/.git/*' {finds}; fi 2>/dev/null | sed 's|^\./||' | while IFS= read -r f; do
case "$f" in {exts}) ;; *) continue ;; esac
case "$f" in *.min.*|*.d.ts) continue ;; esac
case "/$f/" in {vend}|*/.strawberry/*|*/gen/*|*/.git/*) continue ;; esac
[ -f "$f" ] || continue
s=$(wc -c < "$f" 2>/dev/null) || continue
[ "$s" -le {max_file} ] || continue
n=$((n+1)); [ "$n" -le {max_files} ] || break
tot=$((tot+s)); [ "$tot" -le {max_total} ] || break
printf '\n%s %s\n' '{boundary}' "$f"
cat -- "$f"
done; printf '\n%s .\n' '{boundary}'"#,
        max_file = scan::MAX_FILE_BYTES,
        max_files = scan::MAX_FILES,
        max_total = REMOTE_TOTAL_BYTES,
    )
}

/// Splits the gathered stream back into files. The `\n` printed before every
/// boundary swallows exactly the separator it added, so texts round-trip
/// byte-identically whether or not a file ends in a newline.
fn parse_gathered(out: &str, boundary: &str) -> Vec<scan::SourceFile> {
    let head = format!("{boundary} ");
    let mut files: Vec<scan::SourceFile> = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    let flush = |cur: &mut Option<(String, Vec<&str>)>, files: &mut Vec<scan::SourceFile>| {
        if let Some((rel, lines)) = cur.take() {
            let ext = rel.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
            files.push(scan::SourceFile { rel, ext, text: lines.join("\n") });
        }
    };
    for line in out.split('\n') {
        if let Some(rest) = line.strip_prefix(&head) {
            flush(&mut cur, &mut files);
            let rel = rest.trim_end_matches('\r').trim();
            if rel != "." && !rel.is_empty() {
                cur = Some((rel.to_string(), Vec::new()));
            }
        } else if let Some((_, lines)) = &mut cur {
            lines.push(line);
        }
    }
    flush(&mut cur, &mut files);
    files
}

/// Static scan + merge with whatever map already exists on disk. The shared
/// first half of every rescan; the app layers the model pass on top.
async fn scan_merge(root: &Path, run_id: Option<&str>) -> Result<ArchDoc> {
    let scan_root = root.to_path_buf();
    let raw = tokio::task::spawn_blocking(move || scan::scan_workspace(&scan_root))
        .await
        .map_err(|e| AppError::msg(e.to_string()))?;
    let previous = load_doc(root).await;
    let mut doc = merge(raw, previous, run_id);
    doc.root = root.display().to_string();
    Ok(doc)
}

/// Skeleton-only rebuild for a workspace path — used by the `arch-scan` dev
/// binary. Existing model annotations survive the merge; new and changed
/// functions wait for the next in-app scan to be described.
pub async fn rescan_static(root: &Path) -> Result<ArchDoc> {
    let mut doc = scan_merge(root, None).await?;
    doc.generated_at = chrono::Utc::now().timestamp_millis();
    persist(root, &doc).await?;
    Ok(doc)
}

/// Runs a scan for a chat's workspace: static pass, merge with the previous
/// document (carrying descriptions and history), optional model enrichment,
/// then persist. Remote (`ssh://`) workspaces scan the same way — their
/// sources are fetched over the connection first. `run_id` attributes history
/// entries to an agent run; `model_override` lets the Analysis pane pick its
/// own annotation model.
#[allow(clippy::too_many_arguments)]
pub async fn run_scan(
    state: Arc<AppState>,
    chat_id: &str,
    enrich_wanted: bool,
    model_override: Option<String>,
    scope: Option<String>,
    run_id: Option<String>,
    max_enrich: usize,
    cancel: CancellationToken,
    progress: impl Fn(AnalysisEvent) + Send + Sync,
) -> Result<ArchDoc> {
    let cwd = chat_cwd(&state, chat_id)
        .ok_or_else(|| AppError::msg("this chat has no working folder — set one first"))?;

    if crate::remote::parse(&cwd).is_some() {
        return run_scan_remote(
            &state,
            chat_id,
            &cwd,
            enrich_wanted,
            model_override,
            scope,
            run_id,
            max_enrich,
            cancel,
            &progress,
        )
        .await;
    }

    let root = chat_root(&state, chat_id)
        .ok_or_else(|| AppError::msg("this chat has no working folder — set one first"))?;

    progress(AnalysisEvent::Phase { phase: "scan".into(), message: "Scanning files…".into() });
    let mut doc = scan_merge(&root, run_id.as_deref()).await?;

    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }

    if enrich_wanted {
        enrich_pass(
            &state,
            chat_id,
            model_override,
            scope.as_deref(),
            &root,
            None,
            &mut doc,
            max_enrich,
            &cancel,
            &progress,
        )
        .await?;
    }

    doc.generated_at = chrono::Utc::now().timestamp_millis();
    progress(AnalysisEvent::Phase {
        phase: "write".into(),
        message: "Writing .strawberry/architecture.md".into(),
    });
    persist(&root, &doc).await?;
    Ok(doc)
}

/// The remote arm of `run_scan`: sources come over the pooled SSH connection
/// in one round trip, the scan itself runs here, and the finished map lands in
/// the local mirror plus (best-effort) the workspace's own `.strawberry/`.
#[allow(clippy::too_many_arguments)]
async fn run_scan_remote(
    state: &Arc<AppState>,
    chat_id: &str,
    cwd: &str,
    enrich_wanted: bool,
    model_override: Option<String>,
    scope: Option<String>,
    run_id: Option<String>,
    max_enrich: usize,
    cancel: CancellationToken,
    progress: &(dyn Fn(AnalysisEvent) + Send + Sync),
) -> Result<ArchDoc> {
    let t = crate::remote::parse(cwd).ok_or_else(|| AppError::msg("not a remote workspace"))?;
    let r = crate::remote::pooled(state, &t.host_id).await?;

    progress(AnalysisEvent::Phase {
        phase: "scan".into(),
        message: format!("Reading sources on {}…", r.host.label),
    });
    let sources = gather_remote_sources(&r, &t.root, &cancel).await?;
    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }

    progress(AnalysisEvent::Phase {
        phase: "scan".into(),
        message: format!("Scanning {} files…", sources.len()),
    });
    let (raw, sources) = tokio::task::spawn_blocking(move || {
        let raw = scan::scan_sources(&sources);
        (raw, sources)
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;

    let mirror = map_dir(cwd);
    let previous = match load_doc_at(&mirror).await {
        Some(d) => Some(d),
        None => fetch_remote_doc(state, cwd).await,
    };
    let mut doc = merge(raw, previous, run_id.as_deref());
    doc.root = cwd.to_string();

    if cancel.is_cancelled() {
        return Err(AppError::Cancelled);
    }

    if enrich_wanted {
        let texts: HashMap<String, String> = sources.into_iter().map(|s| (s.rel, s.text)).collect();
        enrich_pass(
            state,
            chat_id,
            model_override,
            scope.as_deref(),
            Path::new(""),
            Some(&texts),
            &mut doc,
            max_enrich,
            &cancel,
            progress,
        )
        .await?;
    }

    doc.generated_at = chrono::Utc::now().timestamp_millis();
    progress(AnalysisEvent::Phase {
        phase: "write".into(),
        message: "Writing architecture map…".into(),
    });
    persist_remote(&r, &t.root, &mirror, &doc).await?;
    Ok(doc)
}

/// The shared model-annotation pass. `texts` carries in-memory sources for
/// remote workspaces; local scans read from `root` on disk.
#[allow(clippy::too_many_arguments)]
async fn enrich_pass(
    state: &Arc<AppState>,
    chat_id: &str,
    model_override: Option<String>,
    scope: Option<&str>,
    root: &Path,
    texts: Option<&HashMap<String, String>>,
    doc: &mut ArchDoc,
    max_enrich: usize,
    cancel: &CancellationToken,
    progress: &(dyn Fn(AnalysisEvent) + Send + Sync),
) -> Result<()> {
    let model = model_override
        .filter(|m| !m.trim().is_empty())
        .or_else(|| state.db.get_chat(chat_id).ok().and_then(|c| c.model))
        .unwrap_or_else(|| state.settings().default_model);
    match state.api_key() {
        Ok(key) => {
            progress(AnalysisEvent::Phase {
                phase: "enrich".into(),
                message: format!("Describing functions with {model}…"),
            });
            let client = state.client();
            let ectx =
                enrich::EnrichCtx { client: &client, key: &key, model: &model, root, texts, scope, cancel };
            let stats = enrich::enrich(&ectx, doc, max_enrich, &|done, total| {
                progress(AnalysisEvent::Progress { done: done as u32, total: total as u32 });
            })
            .await?;
            if stats.updated > 0 {
                doc.enriched = true;
                doc.model = Some(model.clone());
            }
            if stats.updated == 0 && stats.requests > 0 && stats.failed < stats.requests {
                progress(AnalysisEvent::Notice {
                    text: format!(
                        "{model} returned no usable annotations — try a different model from the picker"
                    ),
                });
            }
            if stats.failed > 0 {
                // The count alone sends people log-diving; the actual
                // provider message says what to change.
                let why = stats
                    .last_error
                    .as_deref()
                    .map(|e| {
                        let short: String = e.chars().take(140).collect();
                        format!(" — {short}")
                    })
                    .unwrap_or_default();
                progress(AnalysisEvent::Notice {
                    text: format!("{} annotation request(s) failed{why}", stats.failed),
                });
            }
        }
        Err(_) => progress(AnalysisEvent::Notice {
            text: "No API key — map built statically, without AI descriptions".into(),
        }),
    }
    Ok(())
}

/// After a successful agent run that touched files, refresh the map in the
/// background — but only if the user has built one before. Never blocks the
/// run, never surfaces errors: the next manual scan repairs anything.
pub fn maybe_auto_update(state: Arc<AppState>, chat_id: String, run_id: String) {
    tokio::spawn(async move {
        let Some(cwd) = chat_cwd(&state, &chat_id) else { return };

        if crate::remote::parse(&cwd).is_some() {
            // Remote edits never reach the local journal, so the tool ledger
            // is the change signal instead.
            if load_doc_at(&map_dir(&cwd)).await.is_none() {
                return;
            }
            if !state.db.run_wrote_files(&run_id).unwrap_or(false) {
                return;
            }
        } else {
            let Some(root) = chat_root(&state, &chat_id) else { return };
            if load_doc(&root).await.is_none() {
                return;
            }
            let changes = match state.db.run_file_changes(&run_id) {
                Ok(c) => c,
                Err(_) => return,
            };
            let marker = format!("/{DIR}/");
            let touched = changes.iter().any(|c| {
                let p = Path::new(&c.path);
                p.starts_with(&root) && !c.path.contains(&marker)
            });
            if !touched {
                return;
            }
        }

        let enrich = state.api_key().is_ok();
        let _ = run_scan(
            state.clone(),
            &chat_id,
            enrich,
            None,
            None,
            Some(run_id),
            AUTO_ENRICH_CAP,
            CancellationToken::new(),
            |_| {},
        )
        .await;
    });
}

// -------------------------------------------------------------------- merging

/// Folds a fresh static scan into the previous document: descriptions carry
/// over for unchanged bodies, changed ones are marked stale, and every
/// difference lands in the history log.
fn merge(raw: scan::RawScan, previous: Option<ArchDoc>, run_id: Option<&str>) -> ArchDoc {
    let now = chrono::Utc::now().timestamp_millis();
    let first_scan = previous.is_none();
    let prev_files: HashMap<String, ArchFile> = previous
        .as_ref()
        .map(|p| p.files.iter().map(|f| (f.id.clone(), f.clone())).collect())
        .unwrap_or_default();

    let entry = |event: &str| FnLog { ts: now, event: event.into(), run_id: run_id.map(str::to_owned) };
    let push_log = |log: &mut Vec<FnLog>, e: FnLog| {
        log.push(e);
        if log.len() > 12 {
            let excess = log.len() - 12;
            log.drain(..excess);
        }
    };

    let mut files = raw.files;
    for file in &mut files {
        match prev_files.get(&file.id) {
            None => {
                push_log(&mut file.log, entry(if first_scan { "indexed" } else { "added" }));
                for f in &mut file.functions {
                    push_log(&mut f.log, entry(if first_scan { "indexed" } else { "added" }));
                }
            }
            Some(prev) => {
                file.role = prev.role.clone();
                if prev.kind.is_some() {
                    file.kind = prev.kind.clone();
                }
                file.log = prev.log.clone();

                // Functions match by name; duplicate names fall back to
                // declaration order within the name group.
                let mut prev_by_name: HashMap<&str, Vec<&ArchFn>> = HashMap::new();
                for pf in &prev.functions {
                    prev_by_name.entry(pf.name.as_str()).or_default().push(pf);
                }
                let mut taken: HashMap<&str, usize> = HashMap::new();

                let mut seen: Vec<&str> = Vec::new();
                for f in &mut file.functions {
                    seen.push(f.name.as_str());
                    let idx = taken.entry(f.name.as_str()).or_insert(0);
                    let old = prev_by_name.get(f.name.as_str()).and_then(|v| v.get(*idx));
                    *idx += 1;

                    match old {
                        None => push_log(&mut f.log, entry("added")),
                        Some(old) => {
                            f.log = old.log.clone();
                            f.desc = old.desc.clone();
                            f.inputs = old.inputs.clone();
                            f.outputs = old.outputs.clone();
                            f.tag = old.tag.clone();
                            f.note = old.note.clone();
                            if f.hash == old.hash {
                                f.stale = old.stale;
                            } else {
                                f.stale = true;
                                push_log(&mut f.log, entry("modified"));
                            }
                        }
                    }
                }

                // Removals are recorded on the file so the history survives
                // the function itself.
                for pf in &prev.functions {
                    if !seen.contains(&pf.name.as_str()) {
                        push_log(&mut file.log, entry(&format!("removed {}", pf.name)));
                    }
                }
                if file.hash != prev.hash && !first_scan {
                    push_log(&mut file.log, entry("modified"));
                }
            }
        }
    }

    let fn_count = files.iter().map(|f| f.functions.len() as u32).sum();
    ArchDoc {
        version: 1,
        root: String::new(),
        generated_at: now,
        model: previous.as_ref().and_then(|p| p.model.clone()),
        enriched: previous.as_ref().map(|p| p.enriched).unwrap_or(false),
        file_count: files.len() as u32,
        fn_count,
        files,
        edges: raw.edges,
    }
}

// ------------------------------------------------------------------ markdown

/// Renders the compact `architecture.md`. Files are ordered by connectedness
/// and the function inventory degrades gracefully as the budget runs out —
/// the hubs keep their detail, the leaves keep a single line.
pub fn render_md(doc: &ArchDoc) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let by = doc.model.as_deref().unwrap_or("static scan");
    let mut out = String::with_capacity(MD_BUDGET);
    out.push_str(&format!(
        "# Architecture map\n\n{} files · {} functions · generated {date} by {by}\n\nAuto-maintained by Strawberry's Analysis tool; re-scans after agent edits. May lag manual edits.\n",
        doc.file_count, doc.fn_count
    ));

    let mut order: Vec<usize> = (0..doc.files.len()).collect();
    order.sort_by_key(|&i| {
        std::cmp::Reverse(doc.files[i].in_degree * 2 + doc.files[i].functions.len() as u32)
    });

    // Reverse adjacency for a "used by" count — the number readers care about.
    let mut used_by: HashMap<&str, u32> = HashMap::new();
    for e in &doc.edges {
        *used_by.entry(e.to.as_str()).or_insert(0) += 1;
    }

    // The findings sections come first so the budget can never cut them: dead
    // code and near-duplicates are exactly what an agent gets asked to clean
    // up, and they must survive even when the file inventory is truncated.
    let dead: Vec<String> = doc
        .files
        .iter()
        .flat_map(|f| {
            f.functions.iter().filter(|x| x.dead).map(move |x| {
                format!(
                    "- `{}::{}` — no references found
",
                    f.id, x.name
                )
            })
        })
        .collect();
    if !dead.is_empty() {
        out.push_str(
            "
## Possibly dead code
",
        );
        for line in dead.iter().take(20) {
            out.push_str(line);
        }
        if dead.len() > 20 {
            out.push_str(&format!(
                "- …and {} more
",
                dead.len() - 20
            ));
        }
    }

    let mut pairs: Vec<(String, String, f32)> = Vec::new();
    for f in &doc.files {
        for func in &f.functions {
            let me = format!("{}::{}", f.id, func.name);
            for s in &func.similar {
                if s.score >= 0.5 && me.as_str() < s.target.as_str() {
                    pairs.push((me.clone(), s.target.clone(), s.score));
                }
            }
        }
    }
    pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    if !pairs.is_empty() {
        out.push_str(
            "
## Near-duplicate functions
",
        );
        for (a, b, score) in pairs.iter().take(8) {
            out.push_str(&format!(
                "- `{a}` ≈ `{b}` ({:.0}% similar)
",
                score * 100.0
            ));
        }
    }

    // Cross-service channels that imports cannot see: routes one file binds
    // and another calls, topics published and subscribed.
    let mut channels: Vec<String> = Vec::new();
    for e in &doc.edges {
        for via in &e.via {
            let line = format!("- {via}: `{}` ⇠ `{}`\n", e.to, e.from);
            if !channels.contains(&line) {
                channels.push(line);
            }
        }
    }
    if !channels.is_empty() {
        out.push_str("\n## Service channels\n");
        for line in channels.iter().take(12) {
            out.push_str(line);
        }
    }

    // Worst lint findings, quality-ordered — the cleanup queue at a glance.
    let mut flagged: Vec<(u8, String)> = Vec::new();
    for f in &doc.files {
        for l in &f.lints {
            flagged.push((f.quality, format!("- `{}` — {l}\n", f.id)));
        }
        for func in f.functions.iter().filter(|x| !x.lints.is_empty() && !x.dead) {
            flagged.push((
                func.quality,
                format!("- `{}::{}` — {} (q{})\n", f.id, func.name, func.lints.join("; "), func.quality),
            ));
        }
    }
    flagged.sort_by_key(|(q, _)| *q);
    if !flagged.is_empty() {
        out.push_str("\n## Lint flags\n");
        for (_, line) in flagged.iter().take(15) {
            out.push_str(line);
        }
        if flagged.len() > 15 {
            out.push_str(&format!("- …and {} more\n", flagged.len() - 15));
        }
    }

    out.push_str(
        "
## Files
",
    );
    let mut overflowed = false;
    for &i in &order {
        let f = &doc.files[i];
        let role = f.role.as_deref().unwrap_or("");
        let head = format!(
            "\n### {} — {}{}{} · {} lines · q{}{}\n",
            f.id,
            f.lang,
            f.kind.as_deref().map(|k| format!(" · {k}")).unwrap_or_default(),
            if f.hub { " · hub" } else { "" },
            f.lines,
            f.quality,
            if role.is_empty() { String::new() } else { format!("\n{role}.") }
        );

        let mut section = head;
        if !f.imports.is_empty() || used_by.contains_key(f.id.as_str()) {
            section.push_str(&format!(
                "uses: {} · used by {} file(s)\n",
                if f.imports.is_empty() { "—".into() } else { f.imports.join(", ") },
                used_by.get(f.id.as_str()).copied().unwrap_or(0)
            ));
        }
        if !f.lints.is_empty() {
            section.push_str(&format!("flags: {}\n", f.lints.join("; ")));
        }
        if !f.provides.is_empty() {
            section.push_str(&format!("serves: {}\n", f.provides.join(" · ")));
        }
        if !f.consumes.is_empty() {
            section.push_str(&format!("calls: {}\n", f.consumes.join(" · ")));
        }
        if !overflowed {
            for func in &f.functions {
                let mut line = format!("- `{}({})`", func.name, compress_params(&func.params));
                if let Some(d) = &func.desc {
                    line.push_str(&format!(" — {d}"));
                }
                if let (Some(i), Some(o)) = (&func.inputs, &func.outputs) {
                    line.push_str(&format!(" (in: {i}; out: {o})"));
                }
                if let Some(t) = &func.tag {
                    line.push_str(&format!(" [{t}]"));
                }
                if func.core {
                    line.push_str(" [core]");
                }
                if func.dead {
                    line.push_str(" [dead]");
                }
                if let Some(n) = &func.note {
                    line.push_str(&format!(" — {n}"));
                }
                if let Some(t) = &func.big_o {
                    line.push_str(&format!(" ~{t}"));
                    if let Some(s) = &func.space {
                        line.push_str(&format!("/{s}"));
                    }
                }
                if !func.lints.is_empty() {
                    line.push_str(&format!(" (q{}: {})", func.quality, func.lints.join(", ")));
                }
                if let Some(twin) = func.similar.first().filter(|s| s.score >= 0.45) {
                    line.push_str(&format!(" ≈ {}", twin.target));
                }
                section.push_str(&line);
                section.push('\n');
            }
        }

        if out.len() + section.len() > MD_BUDGET {
            if overflowed {
                // Even the one-line form no longer fits.
                out.push_str(&format!(
                    "\n…and {} more files.\n",
                    order.len() - order.iter().position(|x| *x == i).unwrap_or(0)
                ));
                break;
            }
            overflowed = true; // switch to one-line mode and retry this file
            let brief = format!("\n### {} — {role} · q{}\n", f.id, f.quality);
            if out.len() + brief.len() <= MD_BUDGET {
                out.push_str(&brief);
            }
            continue;
        }
        out.push_str(&section);
    }
    out
}

fn compress_params(params: &str) -> String {
    let names: Vec<&str> = params
        .split(',')
        .map(|p| p.split(':').next().unwrap_or(p).trim())
        .filter(|p| !p.is_empty())
        .collect();
    let mut s = names.join(", ");
    if s.len() > 48 {
        s.truncate(45);
        s.push('…');
    }
    s
}

/// The `<architecture>` block for the system prompt, if a map exists for this
/// workspace. Synchronous by design — prompt assembly is synchronous — so
/// remote workspaces read the local mirror `map_dir` maintains, never the
/// wire. Trimmed to a hard budget at a line boundary.
pub fn prompt_context(cwd: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(map_dir(&cwd.to_string_lossy()).join(MD_FILE)).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    if raw.len() <= PROMPT_BUDGET {
        return Some(raw);
    }
    let mut cut = PROMPT_BUDGET;
    while cut > 0 && !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    let clipped = match raw[..cut].rfind('\n') {
        Some(nl) => &raw[..nl],
        None => &raw[..cut],
    };
    Some(format!("{clipped}\n…[truncated]"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn doc_with(files: Vec<ArchFile>) -> scan::RawScan {
        scan::RawScan { files, edges: Vec::new() }
    }

    fn file(id: &str, fns: Vec<ArchFn>) -> ArchFile {
        ArchFile { id: id.into(), hash: "h1".into(), functions: fns, ..Default::default() }
    }

    fn func(name: &str, hash: &str) -> ArchFn {
        ArchFn { name: name.into(), hash: hash.into(), stale: true, ..Default::default() }
    }

    #[test]
    fn merge_carries_descriptions_and_logs_changes() {
        let mut old_fn = func("alpha", "same");
        old_fn.desc = Some("does things".into());
        old_fn.stale = false;
        let mut changed = func("beta", "old");
        changed.desc = Some("old desc".into());

        let prev = ArchDoc {
            files: vec![ArchFile {
                id: "a.ts".into(),
                hash: "h0".into(),
                functions: vec![old_fn, changed, func("gone", "x")],
                ..Default::default()
            }],
            ..Default::default()
        };

        let fresh = doc_with(vec![file(
            "a.ts",
            vec![func("alpha", "same"), func("beta", "new"), func("brandnew", "y")],
        )]);

        let merged = merge(fresh, Some(prev), Some("run1"));
        let f = &merged.files[0];

        let alpha = &f.functions[0];
        assert_eq!(alpha.desc.as_deref(), Some("does things"));
        assert!(!alpha.stale, "unchanged body keeps its description fresh");

        let beta = &f.functions[1];
        assert!(beta.stale, "changed body is marked stale");
        assert_eq!(beta.desc.as_deref(), Some("old desc"), "stale keeps the old text until re-enriched");
        assert_eq!(beta.log.last().unwrap().event, "modified");
        assert_eq!(beta.log.last().unwrap().run_id.as_deref(), Some("run1"));

        assert_eq!(f.functions[2].log.last().unwrap().event, "added");
        assert!(f.log.iter().any(|l| l.event == "removed gone"));
    }

    #[test]
    fn first_scan_logs_indexed_not_added() {
        let merged = merge(doc_with(vec![file("a.ts", vec![func("f", "h")])]), None, None);
        assert_eq!(merged.files[0].functions[0].log[0].event, "indexed");
    }

    /// The gather stream must round-trip file bodies exactly: trailing
    /// newline or not, leading blank lines, empty files.
    #[test]
    fn gathered_stream_round_trips() {
        let b = "=====STRAWBERRY-x=====";
        let stream = format!(
            "\n{b} src/a.rs\nfn a() {{}}\n\n{b} src/b.ts\nconst b = 1;\n{b} sub dir/c.py\n\ndef c():\n    pass\n\n{b} empty.rs\n\n{b} .\n"
        );
        let files = parse_gathered(&stream, b);
        let by_rel: HashMap<&str, &str> =
            files.iter().map(|f| (f.rel.as_str(), f.text.as_str())).collect();
        assert_eq!(by_rel["src/a.rs"], "fn a() {}\n", "trailing newline kept");
        assert_eq!(by_rel["src/b.ts"], "const b = 1;", "no trailing newline added");
        assert_eq!(by_rel["sub dir/c.py"], "\ndef c():\n    pass\n", "leading blank line kept");
        assert_eq!(by_rel["empty.rs"], "", "empty file survives");
        assert_eq!(files.len(), 4, "terminator line is not a file");
        assert_eq!(files[1].ext, "ts");
    }

    /// The remote command must carry the same filters the local walker uses.
    #[test]
    fn gather_cmd_carries_filters() {
        let cmd = gather_cmd("==B==");
        assert!(cmd.contains("git ls-files"));
        assert!(cmd.contains("*/node_modules/*"));
        assert!(cmd.contains("*.rs"));
        assert!(cmd.contains("*.min.*|*.d.ts"));
        assert!(cmd.contains(&scan::MAX_FILE_BYTES.to_string()));
        assert!(cmd.contains(&scan::MAX_FILES.to_string()));
        assert!(cmd.contains("'==B=='"));
    }

    /// Runs the actual gather script under `sh` against a scratch folder —
    /// the same POSIX surface a remote host presents — and round-trips the
    /// result through the parser.
    #[cfg(unix)]
    #[test]
    fn gather_cmd_runs_under_sh() {
        let dir = std::env::temp_dir().join(format!("sb-gather-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("no_eol.ts"), "const x = 1;").unwrap();
        std::fs::write(dir.join("has space.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join("node_modules/skip.js"), "no\n").unwrap();
        std::fs::write(dir.join("big.rs"), "x".repeat(scan::MAX_FILE_BYTES as usize + 1)).unwrap();

        let b = "=====STRAWBERRY-t=====";
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(gather_cmd(b))
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

        let files = parse_gathered(&String::from_utf8_lossy(&out.stdout), b);
        let by_rel: HashMap<String, String> =
            files.into_iter().map(|f| (f.rel, f.text)).collect();
        assert_eq!(by_rel.get("src/a.rs").map(String::as_str), Some("fn main() {}\n"));
        assert_eq!(by_rel.get("no_eol.ts").map(String::as_str), Some("const x = 1;"));
        assert_eq!(by_rel.get("has space.py").map(String::as_str), Some("x = 1\n"));
        assert!(!by_rel.contains_key("node_modules/skip.js"), "vendored dir leaked");
        assert!(!by_rel.contains_key("big.rs"), "size cap ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_dir_splits_local_and_remote() {
        assert!(map_dir("/tmp/work").ends_with(".strawberry"));
        let a = map_dir("ssh://host1/w/app");
        let b = map_dir("ssh://host1/w/other");
        assert_ne!(a, b, "different remote folders get different mirrors");
        assert_eq!(a, map_dir("ssh://host1/w/app"), "stable key");
        assert!(a.to_string_lossy().contains("remote-arch"));
    }

    #[test]
    fn md_stays_in_budget() {
        let files: Vec<ArchFile> = (0..400)
            .map(|i| {
                let fns = (0..10)
                    .map(|j| {
                        let mut f = func(&format!("fn{j}"), "h");
                        f.desc = Some("does a reasonably described thing here".into());
                        f.params = "alpha, beta, gamma".into();
                        f
                    })
                    .collect();
                file(&format!("src/module{i}.ts"), fns)
            })
            .collect();
        let doc = merge(doc_with(files), None, None);
        let md = render_md(&doc);
        assert!(md.len() <= MD_BUDGET + 200, "md was {} bytes", md.len());
    }
}
