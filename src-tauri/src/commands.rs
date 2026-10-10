//! Tauri command surface — the entire API the UI can call.
//!
//! Two rules hold throughout: no command blocks on long work (anything slow
//! returns an id and streams over a channel), and no command can panic the app
//! (every fallible path returns `AppError`, which serialises to a displayable
//! object).

use crate::agent::{runner, AgentEvent, RunHandle};
use crate::db::{
    self, Attachment, Chat, ChatPatch, Message, NewMessage, Project, ProjectPatch, Run, SearchHit, SshHost,
    ToolCallRecord,
};
use crate::error::{AppError, Result};
use crate::openrouter::ModelInfo;
use crate::secrets;
use crate::settings::Settings;
use crate::state::AppState;
use crate::terminal::TerminalInfo;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;
use tokio::sync::broadcast::error::RecvError;

type St<'a> = State<'a, Arc<AppState>>;

// ------------------------------------------------------------------ app meta

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub data_dir: String,
    pub has_api_key: bool,
}

#[tauri::command]
pub fn app_info(state: St) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        data_dir: db::data_dir().display().to_string(),
        has_api_key: state.has_api_key(),
    }
}

// ------------------------------------------------------------------ settings

#[tauri::command]
pub fn get_settings(state: St) -> Settings {
    state.settings()
}

#[tauri::command]
pub fn set_settings(state: St, settings: Settings) -> Result<Settings> {
    state.apply_settings(settings)?;
    Ok(state.settings())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub configured: bool,
    pub masked: Option<String>,
    /// `file` or `keychain` — where the value actually lives right now.
    pub store: String,
    /// Path of the credentials file, shown when the file store is in use.
    pub path: String,
}

#[tauri::command]
pub fn api_key_status(state: St) -> KeyStatus {
    let store = match secrets::backend() {
        secrets::Backend::Keychain => "keychain",
        secrets::Backend::File => "file",
    };
    let path = secrets::credentials_path().display().to_string();
    match state.api_key() {
        Ok(k) => KeyStatus { configured: true, masked: Some(secrets::mask(&k)), store: store.into(), path },
        Err(_) => KeyStatus { configured: false, masked: None, store: store.into(), path },
    }
}

#[tauri::command]
pub fn set_api_key(state: St, key: String) -> Result<KeyStatus> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(AppError::msg("the key is empty"));
    }
    secrets::set(secrets::OPENROUTER_KEY, &key)?;
    state.invalidate_api_key();
    Ok(api_key_status(state))
}

#[tauri::command]
pub fn clear_api_key(state: St) -> Result<()> {
    secrets::delete(secrets::OPENROUTER_KEY)?;
    state.invalidate_api_key();
    Ok(())
}

/// Round-trips the key against OpenRouter's `/key` endpoint. Costs nothing and
/// proves the key works before the user sends a first message.
#[tauri::command]
pub async fn verify_api_key(state: St<'_>, key: Option<String>) -> Result<Value> {
    let key = match key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => state.api_key()?,
    };
    state.client().key_info(&key).await
}

#[tauri::command]
pub async fn get_credits(state: St<'_>) -> Result<Value> {
    let key = state.api_key()?;
    state.client().credits(&key).await
}

// -------------------------------------------------------------------- models

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    pub models: Vec<ModelInfo>,
    pub fetched_at: i64,
    pub stale: bool,
}

/// Model list, served from cache first.
///
/// The catalogue is ~400 entries and changes slowly, so the cached copy is
/// returned immediately and only refreshed when it is a day old or the user
/// asks. On a slow connection this is the difference between an instant model
/// picker and a spinner.
#[tauri::command]
pub async fn list_models(state: St<'_>, refresh: Option<bool>) -> Result<ModelCatalog> {
    const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
    let now = chrono::Utc::now().timestamp_millis();
    let cached = state.db.cached_models()?;
    let force = refresh.unwrap_or(false);

    if let Some((payload, fetched_at)) = &cached {
        let fresh = now - fetched_at < MAX_AGE_MS;
        if !force && (fresh || state.settings().low_bandwidth) {
            if let Ok(models) = serde_json::from_str::<Vec<ModelInfo>>(payload) {
                return Ok(ModelCatalog { models, fetched_at: *fetched_at, stale: !fresh });
            }
        }
    }

    let key = match state.api_key() {
        Ok(k) => k,
        Err(e) => {
            // Without a key we can still serve whatever we cached before.
            return match cached {
                Some((payload, fetched_at)) => Ok(ModelCatalog {
                    models: serde_json::from_str(&payload).unwrap_or_default(),
                    fetched_at,
                    stale: true,
                }),
                None => Err(e),
            };
        }
    };

    match state.client().models(&key).await {
        Ok(models) => {
            state.db.cache_models(&serde_json::to_string(&models)?)?;
            Ok(ModelCatalog { models, fetched_at: now, stale: false })
        }
        Err(e) => match cached {
            Some((payload, fetched_at)) => Ok(ModelCatalog {
                models: serde_json::from_str(&payload).unwrap_or_default(),
                fetched_at,
                stale: true,
            }),
            None => Err(e),
        },
    }
}

// ------------------------------------------------------------------ projects

#[tauri::command]
pub fn list_projects(state: St) -> Result<Vec<Project>> {
    state.db.list_projects()
}

#[tauri::command]
pub fn create_project(state: St, name: String, root_path: Option<String>) -> Result<Project> {
    state.db.create_project(&name, root_path.as_deref())
}

#[tauri::command]
pub fn update_project(state: St, patch: ProjectPatch) -> Result<Project> {
    state.db.update_project(&patch)?;
    state.db.get_project(&patch.id)
}

#[tauri::command]
pub fn delete_project(state: St, id: String) -> Result<()> {
    state.db.delete_project(&id)
}

// --------------------------------------------------------------------- chats

#[tauri::command]
pub fn list_chats(state: St, project_id: Option<String>) -> Result<Vec<Chat>> {
    state.db.list_chats(project_id.as_deref())
}

#[tauri::command]
pub fn create_chat(
    state: St,
    project_id: Option<String>,
    title: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
) -> Result<Chat> {
    let model = model.or_else(|| Some(state.settings().default_model));
    // Inherit the project's root so file tools work without extra setup.
    let cwd = cwd.or_else(|| {
        project_id.as_deref().and_then(|id| state.db.get_project(id).ok()).and_then(|p| p.root_path)
    });
    state.db.create_chat(
        project_id.as_deref(),
        &title.unwrap_or_else(|| crate::agent::runner::UNTITLED.into()),
        model.as_deref(),
        cwd.as_deref(),
    )
}

#[tauri::command]
pub fn get_chat(state: St, id: String) -> Result<Chat> {
    state.db.get_chat(&id)
}

#[tauri::command]
pub fn update_chat(state: St, patch: ChatPatch) -> Result<Chat> {
    state.db.update_chat(&patch)?;
    state.db.get_chat(&patch.id)
}

#[tauri::command]
pub fn delete_chat(state: St, id: String) -> Result<()> {
    state.runs.cancel_chat(&id);
    state.db.delete_chat(&id)
}

#[tauri::command]
pub fn list_messages(state: St, chat_id: String) -> Result<Vec<Message>> {
    state.db.list_messages(&chat_id)
}

#[tauri::command]
pub fn list_tool_calls(state: St, chat_id: String) -> Result<Vec<ToolCallRecord>> {
    state.db.list_tool_calls(&chat_id)
}

#[tauri::command]
pub fn delete_message(state: St, id: String) -> Result<()> {
    state.db.delete_message(&id)
}

/// Rewrites one message's content in place — the chat-side design editor
/// persisting a swatch or wireframe edit. Refused while the chat is running so
/// an in-flight step cannot read a half-written transcript.
#[tauri::command]
pub fn update_message(state: St, id: String, content: String) -> Result<Message> {
    let m = state.db.get_message(&id)?;
    if state.runs.is_chat_running(&m.chat_id) {
        return Err(AppError::msg("wait for the current run to finish before editing this message"));
    }
    state.db.update_message_content(&id, &content)
}

#[tauri::command]
pub fn search_messages(state: St, query: String, limit: Option<i64>) -> Result<Vec<SearchHit>> {
    if query.trim().len() < 2 {
        return Ok(Vec::new());
    }
    state.db.search(query.trim(), limit.unwrap_or(50))
}

// ---------------------------------------------------------------- attachments

/// Copies picked files into the app's own storage so an attachment survives the
/// original being moved or deleted.
#[tauri::command]
pub async fn attach_files(state: St<'_>, chat_id: String, paths: Vec<String>) -> Result<Vec<Attachment>> {
    let max_bytes = state.settings().max_upload_mb * 1024 * 1024;
    let dir = db::data_dir().join("attachments").join(&chat_id);
    tokio::fs::create_dir_all(&dir).await?;

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let src = std::path::PathBuf::from(&p);
        let meta = tokio::fs::metadata(&src).await.map_err(|e| AppError::msg(format!("{p}: {e}")))?;
        if meta.len() > max_bytes {
            return Err(AppError::msg(format!(
                "{} is {} — over the {} MB limit",
                src.file_name().unwrap_or_default().to_string_lossy(),
                crate::tools::fs_tools::human_size(meta.len()),
                state.settings().max_upload_mb
            )));
        }
        let name = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let id = db::new_id();
        let dest = dir.join(format!("{id}-{name}"));
        tokio::fs::copy(&src, &dest).await?;

        let (mime, kind) = classify(&name);
        let a = Attachment {
            id,
            chat_id: chat_id.clone(),
            message_id: None,
            name,
            mime,
            size: meta.len() as i64,
            path: dest.display().to_string(),
            kind,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        state.db.insert_attachment(&a)?;
        out.push(a);
    }
    Ok(out)
}

/// Attaches raw bytes — a clipboard paste or a drag-and-drop that never had a
/// path on disk. Base64 rather than a byte array because serde would otherwise
/// decode a JSON array of numbers, which is an order of magnitude slower for a
/// screenshot-sized payload.
#[tauri::command]
pub async fn attach_bytes(
    state: St<'_>,
    chat_id: String,
    name: String,
    mime: String,
    data_base64: String,
) -> Result<Attachment> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| AppError::msg(format!("clipboard data was not valid base64: {e}")))?;

    let max_bytes = state.settings().max_upload_mb * 1024 * 1024;
    if bytes.len() as u64 > max_bytes {
        return Err(AppError::msg(format!(
            "that paste is {} — over the {} MB limit",
            crate::tools::fs_tools::human_size(bytes.len() as u64),
            state.settings().max_upload_mb
        )));
    }

    let dir = db::data_dir().join("attachments").join(&chat_id);
    tokio::fs::create_dir_all(&dir).await?;
    let id = db::new_id();
    let safe_name = if name.trim().is_empty() { "pasted".to_string() } else { name };
    let dest = dir.join(format!("{id}-{safe_name}"));
    tokio::fs::write(&dest, &bytes).await?;

    let kind = if mime.starts_with("image/") {
        "image"
    } else if mime == "application/pdf" {
        "pdf"
    } else {
        "text"
    };

    let a = Attachment {
        id,
        chat_id,
        message_id: None,
        name: safe_name,
        mime,
        size: bytes.len() as i64,
        path: dest.display().to_string(),
        kind: kind.into(),
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    state.db.insert_attachment(&a)?;
    Ok(a)
}

#[tauri::command]
pub fn list_attachments(state: St, chat_id: String) -> Result<Vec<Attachment>> {
    state.db.list_attachments(&chat_id)
}

fn classify(name: &str) -> (String, String) {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => ("image/png".into(), "image".into()),
        "jpg" | "jpeg" => ("image/jpeg".into(), "image".into()),
        "gif" => ("image/gif".into(), "image".into()),
        "webp" => ("image/webp".into(), "image".into()),
        "avif" => ("image/avif".into(), "image".into()),
        "pdf" => ("application/pdf".into(), "pdf".into()),
        "json" => ("application/json".into(), "text".into()),
        "csv" => ("text/csv".into(), "text".into()),
        "md" | "markdown" => ("text/markdown".into(), "text".into()),
        "" => ("text/plain".into(), "text".into()),
        _ => ("text/plain".into(), "text".into()),
    }
}

// ----------------------------------------------------------------------- runs

/// Sends a user message and starts an agent run.
///
/// Returns as soon as the run is registered. Everything after that arrives on
/// `on_event`, so the UI thread is never waiting on the network.
#[tauri::command]
pub async fn send_message(
    state: St<'_>,
    chat_id: String,
    text: String,
    attachment_ids: Option<Vec<String>>,
    on_event: Channel<AgentEvent>,
) -> Result<String> {
    if state.runs.is_chat_running(&chat_id) {
        return Err(AppError::msg("this chat already has a run in progress"));
    }
    state.api_key()?;

    let attachments = attachment_ids.unwrap_or_default();
    if text.trim().is_empty() && attachments.is_empty() {
        return Err(AppError::msg("nothing to send"));
    }

    let msg = state.db.insert_message(&NewMessage {
        chat_id: chat_id.clone(),
        role: "user".into(),
        content: text,
        status: "complete".into(),
        ..Default::default()
    })?;
    if !attachments.is_empty() {
        state.db.attach_to_message(&attachments, &msg.id)?;
    }

    // A chat bound to an execution graph routes its sends through the graph;
    // the composer does not know or care.
    if let Some(graph_id) = bound_graph(&state, &chat_id) {
        return start_graph_run(&state, chat_id, graph_id, on_event);
    }

    start_run(&state, chat_id, on_event).await
}

/// The graph a chat's sends run through — when bound, and the graph still exists.
fn bound_graph(state: &crate::state::AppState, chat_id: &str) -> Option<String> {
    let gid = state.db.get_meta(&graph_bind_key(chat_id)).ok().flatten()?;
    (!gid.is_empty() && state.db.get_graph(&gid).is_ok()).then_some(gid)
}

/// Rewinds the chat to just before `seq` and runs again. Powers both "retry"
/// and "edit and resend".
#[tauri::command]
pub async fn retry_from(
    state: St<'_>,
    chat_id: String,
    seq: i64,
    edited_text: Option<String>,
    on_event: Channel<AgentEvent>,
) -> Result<String> {
    // Retry means "replace what is happening": a run still in flight is
    // cancelled and given a moment to unwind, never used as a reason to
    // silently refuse the resend.
    if state.runs.is_chat_running(&chat_id) {
        state.runs.cancel_chat(&chat_id);
        for _ in 0..40 {
            if !state.runs.is_chat_running(&chat_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if state.runs.is_chat_running(&chat_id) {
            return Err(AppError::msg(
                "the current run is not stopping — wait a moment and retry again",
            ));
        }
    }
    state.api_key()?;

    match edited_text {
        Some(text) => {
            state.db.truncate_from(&chat_id, seq)?;
            state.db.insert_message(&NewMessage {
                chat_id: chat_id.clone(),
                role: "user".into(),
                content: text,
                status: "complete".into(),
                ..Default::default()
            })?;
        }
        None => state.db.truncate_from(&chat_id, seq)?,
    }

    // Retry must run the same way the original send did: a graph-bound chat
    // re-enters its graph — a plain agent run here made "resend" a different
    // (and wrong) operation on those chats.
    if let Some(graph_id) = bound_graph(&state, &chat_id) {
        return start_graph_run(&state, chat_id, graph_id, on_event);
    }

    start_run(&state, chat_id, on_event).await
}

/// Injects a user message into a chat whose run is still in flight. The agent
/// runner rebuilds its request from the database on every step, so the text
/// joins the conversation at the next step — mid-run steering. A graph run
/// picks it up on its next invocation instead; the composer queues for those.
#[tauri::command]
pub fn run_steer(state: St, chat_id: String, text: String) -> Result<db::Message> {
    if text.trim().is_empty() {
        return Err(AppError::msg("nothing to send"));
    }
    if !state.runs.is_chat_running(&chat_id) {
        return Err(AppError::msg("no run in progress — send normally"));
    }
    state.db.insert_message(&NewMessage {
        chat_id,
        role: "user".into(),
        content: text,
        status: "complete".into(),
        ..Default::default()
    })
}

async fn start_run(state: &Arc<AppState>, chat_id: String, on_event: Channel<AgentEvent>) -> Result<String> {
    let chat = state.db.get_chat(&chat_id)?;
    let model = chat.model.clone().unwrap_or_else(|| state.settings().default_model);

    let run_id = db::new_id();
    state.db.create_run(&run_id, &chat_id, &model)?;
    let handle = state.runs.start(&run_id, &chat_id);

    // Subscribe before the loop starts so the very first event is delivered.
    forward(handle.clone(), on_event);

    let ctx = runner::RunContext { state: state.clone(), handle, run_id: run_id.clone(), chat_id };
    tokio::spawn(runner::execute(ctx));
    Ok(run_id)
}

/// Reattaches a UI channel to a run that is already in flight — after a window
/// reload, or when the user navigates back to a chat that is still working.
#[tauri::command]
pub fn run_subscribe(state: St, run_id: String, on_event: Channel<AgentEvent>) -> Result<()> {
    match state.runs.get(&run_id) {
        Some(handle) => {
            forward(handle, on_event);
            Ok(())
        }
        None => {
            // Already finished: tell the caller immediately instead of leaving
            // it waiting for an event that will never come.
            let _ = on_event.send(AgentEvent::RunFinished { run_id, status: "complete".into(), error: None });
            Ok(())
        }
    }
}

fn forward(handle: Arc<RunHandle>, ch: Channel<AgentEvent>) {
    let mut rx = handle.tx.subscribe();
    // tauri's runtime handle, NOT tokio::spawn: this is reached from
    // `run_subscribe`, a synchronous command that executes on the main thread
    // where no tokio reactor is ambient. `tokio::spawn` there aborts the whole
    // process the first time a client reattaches to a live run.
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let done = matches!(ev, AgentEvent::RunFinished { .. });
                    if ch.send(ev).is_err() || done {
                        break;
                    }
                }
                // The UI fell far enough behind that events were dropped. Tell
                // it to resync from the database rather than showing a gap.
                Err(RecvError::Lagged(n)) => {
                    let _ = ch.send(AgentEvent::Notice {
                        level: "resync".into(),
                        text: format!("{n} updates skipped"),
                    });
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

// ------------------------------------------------------- model execution graph

fn graph_bind_key(chat_id: &str) -> String {
    format!("graphbind:{chat_id}")
}

fn graph_doc(row: db::GraphRow) -> Result<crate::meg::GraphDoc> {
    let spec: crate::meg::GraphSpec = serde_json::from_str(&row.spec)
        .map_err(|e| AppError::msg(format!("graph {} is unreadable: {e}", row.id)))?;
    let artifacts = serde_json::from_str(&row.artifacts).unwrap_or_default();
    Ok(crate::meg::GraphDoc {
        id: row.id,
        project_id: row.project_id,
        name: row.name,
        spec,
        artifacts,
        updated_at: row.updated_at,
    })
}

#[tauri::command]
pub fn graph_list(state: St) -> Result<Vec<crate::meg::GraphDoc>> {
    Ok(state.db.list_graphs()?.into_iter().filter_map(|r| graph_doc(r).ok()).collect())
}

#[tauri::command]
pub fn graph_save(
    state: St,
    id: Option<String>,
    project_id: Option<String>,
    name: String,
    spec: crate::meg::GraphSpec,
) -> Result<crate::meg::GraphDoc> {
    let id = id.unwrap_or_else(db::new_id);
    let mut spec = spec;
    spec.id = id.clone();
    spec.name = name.clone();
    let json = serde_json::to_string(&spec)?;
    state.db.save_graph(&id, project_id.as_deref(), &name, &json)?;
    graph_doc(state.db.get_graph(&id)?)
}

/// Stores a manually edited artifact for a node — the user's hand-tuned
/// design — and pins the node, so a later run serves the edit instead of
/// overwriting it. The graph pane's design editor calls this.
#[tauri::command]
pub fn graph_set_artifact(
    state: St,
    id: String,
    node_id: String,
    content: String,
) -> Result<crate::meg::GraphDoc> {
    let row = state.db.get_graph(&id)?;
    let mut arts: std::collections::HashMap<String, crate::meg::NodeArtifact> =
        serde_json::from_str(&row.artifacts).unwrap_or_default();
    let now = chrono::Utc::now().timestamp_millis();
    let entry = arts.entry(node_id.clone()).or_insert_with(|| crate::meg::NodeArtifact {
        hash: String::new(),
        content: String::new(),
        verdict: None,
        model: None,
        tokens_in: 0,
        tokens_out: 0,
        cost: 0.0,
        elapsed_ms: 0,
        created_at: now,
    });
    entry.content = content;
    entry.model = Some("edited by hand".into());
    entry.created_at = now;
    state.db.set_graph_artifacts(&id, &serde_json::to_string(&arts)?)?;

    let mut spec: crate::meg::GraphSpec = serde_json::from_str(&row.spec)
        .map_err(|e| AppError::msg(format!("graph {} is unreadable: {e}", row.id)))?;
    if let Some(n) = spec.nodes.iter_mut().find(|n| n.id == node_id) {
        if !n.pinned {
            n.pinned = true;
            state.db.save_graph(&id, row.project_id.as_deref(), &row.name, &serde_json::to_string(&spec)?)?;
        }
    }
    graph_doc(state.db.get_graph(&id)?)
}

#[tauri::command]
pub fn graph_delete(state: St, id: String) -> Result<()> {
    state.db.delete_graph(&id)
}

/// Binds (or unbinds) a chat to a graph: while bound, every send in that chat
/// runs the graph instead of the plain agent loop.
#[tauri::command]
pub fn graph_bind(state: St, chat_id: String, graph_id: Option<String>) -> Result<()> {
    match graph_id {
        Some(gid) => {
            state.db.get_graph(&gid)?; // Refuse to bind to nothing.
            state.db.set_meta(&graph_bind_key(&chat_id), &gid)
        }
        None => state.db.set_meta(&graph_bind_key(&chat_id), ""),
    }
}

#[tauri::command]
pub fn graph_binding(state: St, chat_id: String) -> Result<Option<String>> {
    Ok(state.db.get_meta(&graph_bind_key(&chat_id))?.filter(|v| !v.is_empty()))
}

/// Drops cached node outputs — the whole graph's, or one node's.
#[tauri::command]
pub fn graph_clear_cache(state: St, id: String, node_id: Option<String>) -> Result<crate::meg::GraphDoc> {
    let row = state.db.get_graph(&id)?;
    let next = match &node_id {
        Some(nid) => {
            let mut map: std::collections::HashMap<String, Value> =
                serde_json::from_str(&row.artifacts).unwrap_or_default();
            map.remove(nid);
            serde_json::to_string(&map)?
        }
        None => "{}".to_string(),
    };
    state.db.set_graph_artifacts(&id, &next)?;
    graph_doc(state.db.get_graph(&id)?)
}

/// Runs a graph in a chat directly from the graph pane. The message becomes a
/// real user message in the chat, and the run is a real run — same registry,
/// same reattach, same cost accounting.
#[tauri::command]
pub async fn graph_run(
    state: St<'_>,
    chat_id: String,
    graph_id: String,
    text: String,
    on_event: Channel<AgentEvent>,
) -> Result<String> {
    if state.runs.is_chat_running(&chat_id) {
        return Err(AppError::msg("this chat already has a run in progress"));
    }
    state.api_key()?;
    if text.trim().is_empty() {
        return Err(AppError::msg("nothing to send"));
    }
    state.db.get_graph(&graph_id)?;
    state.db.insert_message(&NewMessage {
        chat_id: chat_id.clone(),
        role: "user".into(),
        content: text,
        status: "complete".into(),
        ..Default::default()
    })?;
    start_graph_run(&state, chat_id, graph_id, on_event)
}

/// Re-works one node's cached output under an instruction — a micro-run that
/// touches nothing else in the graph. No chat message is involved.
#[tauri::command]
pub async fn graph_refine(
    state: St<'_>,
    chat_id: String,
    graph_id: String,
    node_id: String,
    instruction: String,
    on_event: Channel<AgentEvent>,
) -> Result<String> {
    if state.runs.is_chat_running(&chat_id) {
        return Err(AppError::msg("this chat already has a run in progress"));
    }
    state.api_key()?;
    if instruction.trim().is_empty() {
        return Err(AppError::msg("say how the output should change"));
    }
    state.db.get_graph(&graph_id)?;
    let run_id = db::new_id();
    state.db.create_run(&run_id, &chat_id, "refine")?;
    let handle = state.runs.start(&run_id, &chat_id);
    forward(handle.clone(), on_event);
    let ctx = runner::RunContext { state: state.inner().clone(), handle, run_id: run_id.clone(), chat_id };
    tokio::spawn(crate::meg::execute_refine(crate::meg::GraphRefine {
        ctx,
        graph_id,
        node_id,
        instruction,
    }));
    Ok(run_id)
}

/// Drafts a graph for a goal with one model call. Returns the planner's raw
/// JSON; the frontend materialises it into a spec and the user approves it
/// before anything runs. `customs` summarises the user's saved composite
/// nodes so the planner can reuse them.
#[tauri::command]
pub async fn graph_plan(
    state: St<'_>,
    chat_id: String,
    goal: String,
    customs: Option<Value>,
) -> Result<Value> {
    if goal.trim().is_empty() {
        return Err(AppError::msg("describe what the graph should accomplish"));
    }
    crate::meg::plan_graph(&state, &chat_id, goal.trim(), customs).await
}

fn start_graph_run(
    state: &Arc<AppState>,
    chat_id: String,
    graph_id: String,
    on_event: Channel<AgentEvent>,
) -> Result<String> {
    let run_id = db::new_id();
    let name = state.db.get_graph(&graph_id).map(|g| g.name).unwrap_or_default();
    state.db.create_run(&run_id, &chat_id, &format!("graph:{name}"))?;
    let handle = state.runs.start(&run_id, &chat_id);
    forward(handle.clone(), on_event);
    let ctx = runner::RunContext { state: state.clone(), handle, run_id: run_id.clone(), chat_id };
    tokio::spawn(crate::meg::execute(crate::meg::GraphRun { ctx, graph_id }));
    Ok(run_id)
}

#[tauri::command]
pub fn cancel_run(state: St, run_id: String) -> Result<()> {
    state.runs.cancel(&run_id)
}

#[tauri::command]
pub fn approve_tool(state: St, run_id: String, call_id: String, approved: bool) -> Result<()> {
    state.runs.resolve_approval(&run_id, &call_id, approved)
}

/// The chat's stored plan document, if the model has written one.
#[tauri::command]
pub fn get_plan(state: St, chat_id: String) -> Result<Option<Value>> {
    Ok(state.db.get_meta(&format!("plan:{chat_id}"))?.and_then(|raw| serde_json::from_str(&raw).ok()))
}

#[tauri::command]
pub fn answer_question(state: St, run_id: String, question_id: String, answer: String) -> Result<()> {
    state.runs.resolve_question(&run_id, &question_id, answer)
}

#[tauri::command]
pub fn bg_shells(state: St) -> Vec<crate::shells::ShellInfo> {
    state.shells.list()
}

#[tauri::command]
pub fn bg_shell_kill(state: St, id: String) -> Result<()> {
    state.shells.kill(&id)
}

#[tauri::command]
pub fn active_runs(state: St) -> Result<Vec<Run>> {
    state.db.active_runs()
}

#[tauri::command]
pub fn running_chat_ids(state: St) -> Vec<String> {
    state.runs.active_ids()
}

// ------------------------------------------------------------------ terminals

#[tauri::command]
pub fn term_open_local(
    state: St,
    id: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Result<TerminalInfo> {
    let id = id.unwrap_or_else(db::new_id);
    let shell = state.settings().shell;
    state.terminals.open_local(id, shell, cwd, cols.max(2), rows.max(2), on_data)
}

#[tauri::command]
pub async fn term_open_ssh(
    state: St<'_>,
    id: Option<String>,
    host_id: String,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Result<TerminalInfo> {
    let id = id.unwrap_or_else(db::new_id);
    state.terminals.open_ssh(state.db.clone(), id, host_id, cols.max(2), rows.max(2), on_data).await
}

#[tauri::command]
pub fn term_write(state: St, id: String, data: String) -> Result<()> {
    state.terminals.write(&id, data.as_bytes())
}

#[tauri::command]
pub fn term_resize(state: St, id: String, cols: u16, rows: u16) -> Result<()> {
    state.terminals.resize(&id, cols.max(2), rows.max(2))
}

#[tauri::command]
pub fn term_close(state: St, id: String) -> Result<()> {
    state.terminals.close(&id)
}

#[tauri::command]
pub fn term_list(state: St) -> Vec<TerminalInfo> {
    state.terminals.list()
}

// ----------------------------------------------------------------------- ssh

#[tauri::command]
pub fn list_ssh_hosts(state: St) -> Result<Vec<SshHost>> {
    state.db.list_ssh_hosts()
}

#[tauri::command]
pub fn save_ssh_host(state: St, host: SshHost, secret: Option<String>) -> Result<String> {
    let id = state.db.upsert_ssh_host(&host)?;
    match secret {
        Some(s) if !s.is_empty() => secrets::set(&secrets::ssh_account(&id), &s)?,
        _ => {}
    }
    Ok(id)
}

#[tauri::command]
pub fn delete_ssh_host(state: St, id: String) -> Result<()> {
    let _ = secrets::delete(&secrets::ssh_account(&id));
    state.db.delete_ssh_host(&id)
}

#[tauri::command]
pub async fn test_ssh_host(state: St<'_>, id: String) -> Result<String> {
    let host = state.db.get_ssh_host(&id)?;
    crate::ssh::test_connection(state.db.clone(), &host).await
}

/// Clears a remembered host key so the next connection re-pins it. Needed after
/// a legitimate server rebuild.
#[tauri::command]
pub fn forget_host_key(state: St, id: String) -> Result<()> {
    let host = state.db.get_ssh_host(&id)?;
    state.db.set_meta(&format!("ssh-hostkey:{}:{}", host.host, host.port), "")?;
    Ok(())
}

/// A host block from an OpenSSH config file — offered in the terminal's host
/// menu next to the app's own saved hosts. Connection happens through the
/// system `ssh`, so keys, agents and ProxyJump behave exactly as in a shell.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHost {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    /// "project" (workspace .ssh/config) or "user" (~/.ssh/config).
    pub source: String,
}

/// Hosts from the workspace's `.ssh/config` (if the chat has a folder) and the
/// user's `~/.ssh/config`. Read-only, UI-facing; never exposed to the agent.
#[tauri::command]
pub fn ssh_config_hosts(state: St, chat_id: Option<String>) -> Vec<SshConfigHost> {
    let mut out = Vec::new();
    if let Some(id) = chat_id.as_deref() {
        if let Some(root) = crate::analysis::chat_root(&state, id) {
            parse_ssh_config(&root.join(".ssh/config"), "project", &mut out);
        }
    }
    if let Some(home) = dirs::home_dir() {
        parse_ssh_config(&home.join(".ssh/config"), "user", &mut out);
    }
    out
}

/// Minimal OpenSSH config parser: Host blocks with HostName/User/Port.
/// `Include` and `Match` are skipped — the aliases still work because the
/// system ssh does the real resolution at connect time.
/// Resolves a menu selection to a saved-host id: `host` ids pass through,
/// `alias` entries are saved as app hosts on first use (deduplicated by
/// label) so the pooled russh connection has real credentials to work with.
fn ensure_host_id(state: &St<'_>, chat_id: &str, kind: &str, value: String) -> Result<String> {
    match kind {
        "host" => Ok(value),
        "alias" => {
            let all = ssh_config_hosts(state.clone(), Some(chat_id.to_string()));
            let cfg = all
                .into_iter()
                .find(|h| h.alias == value)
                .ok_or_else(|| AppError::msg(format!("{value} is not in any ssh config")))?;
            // Reuse an existing saved host for this alias rather than piling
            // up duplicates on every workspace switch.
            let label = format!("cfg:{}", cfg.alias);
            let existing = state.db.list_ssh_hosts()?.into_iter().find(|h| h.label == label);
            let host = SshHost {
                id: existing.map(|h| h.id).unwrap_or_default(),
                label,
                host: cfg.host_name.clone().unwrap_or_else(|| cfg.alias.clone()),
                port: cfg.port.map(i64::from).unwrap_or(22),
                username: cfg
                    .user
                    .clone()
                    .or_else(|| std::env::var("USER").ok())
                    .unwrap_or_else(|| "root".into()),
                auth: "key".into(),
                key_path: cfg.identity_file.clone(),
                created_at: 0,
                last_used: None,
            };
            state.db.upsert_ssh_host(&host)
        }
        other => Err(AppError::msg(format!("unknown host kind `{other}`"))),
    }
}

/// Tab completion for the folder step: directories on the remote host that
/// extend `prefix`, shell-style.
#[tauri::command]
pub async fn remote_complete_dir(
    state: St<'_>,
    chat_id: String,
    kind: String,
    value: String,
    prefix: String,
) -> Result<Vec<String>> {
    let host_id = ensure_host_id(&state, &chat_id, &kind, value)?;
    let r = crate::remote::pooled(&state, &host_id).await?;
    r.complete_dir(&prefix).await
}

/// Points a chat's working directory at a folder on an SSH host, making it a
/// remote workspace: agent tools, the file browser and searches all operate
/// on that machine from then on.
///
/// `kind` is `host` (a saved host id) or `alias` (an OpenSSH config entry —
/// resolved to a saved host on first use so the pooled russh connection has
/// real credentials to work with).
#[tauri::command]
pub async fn set_remote_workspace(
    state: St<'_>,
    chat_id: String,
    kind: String,
    value: String,
    dir: String,
) -> Result<String> {
    let host_id = ensure_host_id(&state, &chat_id, &kind, value)?;
    let r = crate::remote::pooled(&state, &host_id).await?;
    let abs = r.canonical_dir(&dir).await?;
    let cwd = crate::remote::format(&host_id, &abs);
    state.db.update_chat(&db::ChatPatch {
        id: chat_id,
        title: None,
        model: None,
        system_prompt: None,
        cwd: Some(cwd.clone()),
        project_id: None,
        archived: None,
        pinned: None,
    })?;
    Ok(cwd)
}

fn parse_ssh_config(path: &std::path::Path, source: &str, out: &mut Vec<SshConfigHost>) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let mut current: Vec<usize> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                current.clear();
                for alias in parts {
                    // Patterns are routing rules, not destinations.
                    if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
                        continue;
                    }
                    if out.iter().any(|h: &SshConfigHost| h.alias == alias) {
                        continue;
                    }
                    current.push(out.len());
                    out.push(SshConfigHost {
                        alias: alias.to_string(),
                        host_name: None,
                        user: None,
                        port: None,
                        identity_file: None,
                        source: source.to_string(),
                    });
                }
            }
            "match" => current.clear(),
            "hostname" => {
                if let Some(v) = parts.next() {
                    for &i in &current {
                        out[i].host_name = Some(v.to_string());
                    }
                }
            }
            "identityfile" => {
                if let Some(v) = parts.next() {
                    for &i in &current {
                        out[i].identity_file = Some(v.to_string());
                    }
                }
            }
            "user" => {
                if let Some(v) = parts.next() {
                    for &i in &current {
                        out[i].user = Some(v.to_string());
                    }
                }
            }
            "port" => {
                if let Some(v) = parts.next().and_then(|p| p.parse().ok()) {
                    for &i in &current {
                        out[i].port = Some(v);
                    }
                }
            }
            _ => {}
        }
    }
}

// --------------------------------------------------------------------- tools

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
    pub risk: String,
}

#[tauri::command]
pub fn list_tools() -> Vec<ToolSummary> {
    crate::tools::registry()
        .iter()
        .map(|t| ToolSummary {
            name: t.name.into(),
            description: t.description.into(),
            risk: format!("{:?}", t.risk).to_lowercase(),
        })
        .collect()
}

/// Opens the webview inspector. Reached from the app's right-click menu; the
/// `devtools` feature keeps it available in release builds too.
#[tauri::command]
pub fn open_devtools(window: tauri::WebviewWindow) {
    window.open_devtools();
}

/// Folds the chat's older history into a rolling structured summary. The UI
/// asks for it from the context meter; the model can do the same via the
/// compact_context tool.
#[tauri::command]
pub async fn compact_chat(state: St<'_>, chat_id: String) -> Result<crate::agent::compact::CompactOutcome> {
    if state.runs.is_chat_running(&chat_id) {
        return Err(AppError::msg("wait for the current run to finish before compacting"));
    }
    crate::agent::compact::compact_chat(state.inner().clone(), &chat_id, None).await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdown {
    /// Base system prompt: behaviour, mode text, custom prompts, environment.
    pub system_tokens: u32,
    /// The injected `<architecture>` block, when the workspace has a map.
    pub architecture_tokens: u32,
    /// JSON tool schemas sent with every request (0 when tools are off).
    pub tools_tokens: u32,
    /// The encoded conversation history as it would be sent right now.
    pub history_tokens: u32,
    /// Messages that made it into the encoded history.
    pub encoded_messages: u32,
    /// All stored messages in the chat, for the "N of M sent" line.
    pub stored_messages: u32,
}

const fn est_tokens(chars: usize) -> u32 {
    (chars / 4) as u32
}

/// What the context is spent on, estimated the same way the request is built:
/// the real system prompt, the real tool schemas, the real encoded history —
/// measured in characters and divided by four. The meter's total stays the
/// provider-billed number; this answers where it goes.
#[tauri::command]
pub async fn context_breakdown(state: St<'_>, chat_id: String) -> Result<ContextBreakdown> {
    let db = state.db.clone();
    let settings = state.settings();
    let chat = db.get_chat(&chat_id)?;
    let project = chat.project_id.as_deref().and_then(|id| db.get_project(id).ok());

    let system =
        crate::agent::prompt::system_prompt(&settings, project.as_ref(), &chat, settings.tools_enabled);
    // The architecture block is embedded in the prompt; measure its exact span
    // rather than rebuilding it, so the two numbers always sum correctly.
    let arch_chars = match (system.find("<architecture>"), system.find("</architecture>")) {
        (Some(a), Some(b)) if b > a => b + "</architecture>".len() - a,
        _ => 0,
    };

    let tools_chars = if settings.tools_enabled {
        serde_json::to_string(&crate::tools::specs(&settings.enabled_tools)).map(|s| s.len()).unwrap_or(0)
    } else {
        0
    };

    let stored = db.list_messages(&chat_id)?;
    let history = crate::agent::prompt::encode_history(&db, &stored, settings.history_limit as usize, false)?;
    let history_chars = serde_json::to_string(&history).map(|s| s.len()).unwrap_or(0);

    Ok(ContextBreakdown {
        system_tokens: est_tokens(system.len() - arch_chars),
        architecture_tokens: est_tokens(arch_chars),
        tools_tokens: est_tokens(tools_chars),
        history_tokens: est_tokens(history_chars),
        encoded_messages: history.len() as u32,
        stored_messages: stored.len() as u32,
    })
}

// ------------------------------------------------------------------ analysis

/// The stored architecture map for this chat's workspace, if one exists.
/// Remote workspaces answer from the local mirror first, then fall back to
/// the copy the workspace itself carries (a scan from another machine).
#[tauri::command]
pub async fn analysis_get(state: St<'_>, chat_id: String) -> Result<crate::analysis::AnalysisSnapshot> {
    let cwd = crate::analysis::chat_cwd(&state, &chat_id);
    if let Some(c) = cwd.as_deref().filter(|c| crate::remote::parse(c).is_some()) {
        let mut doc = crate::analysis::load_doc_at(&crate::analysis::map_dir(c)).await;
        if doc.is_none() {
            doc = crate::analysis::fetch_remote_doc(&state, c).await;
        }
        return Ok(crate::analysis::AnalysisSnapshot { root: Some(c.to_string()), doc });
    }
    let root = crate::analysis::chat_root(&state, &chat_id);
    let doc = match &root {
        Some(r) => crate::analysis::load_doc(r).await,
        None => None,
    };
    Ok(crate::analysis::AnalysisSnapshot { root: root.map(|r| r.display().to_string()), doc })
}

/// Builds (or refreshes) the architecture map. The static graph is free; with
/// `enrich`, `model` (defaulting to the chat's model) writes the descriptions.
/// Streams progress on `on_event` and returns the finished document.
#[tauri::command]
pub async fn analysis_scan(
    state: St<'_>,
    chat_id: String,
    enrich: bool,
    model: Option<String>,
    scope: Option<String>,
    on_event: Channel<crate::analysis::AnalysisEvent>,
) -> Result<crate::analysis::ArchDoc> {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut runs = state.analysis_runs.lock();
        if let Some((_, prev)) = runs.insert(chat_id.clone(), (generation, cancel.clone())) {
            prev.cancel();
        }
    }

    let result = crate::analysis::run_scan(
        state.inner().clone(),
        &chat_id,
        enrich,
        model,
        scope,
        None,
        usize::MAX,
        cancel,
        move |ev| {
            let _ = on_event.send(ev);
        },
    )
    .await;

    let mut runs = state.analysis_runs.lock();
    if runs.get(&chat_id).is_some_and(|(g, _)| *g == generation) {
        runs.remove(&chat_id);
    }
    result
}

#[tauri::command]
pub fn analysis_cancel(state: St, chat_id: String) -> Result<()> {
    if let Some((_, tok)) = state.analysis_runs.lock().remove(&chat_id) {
        tok.cancel();
    }
    Ok(())
}

// ------------------------------------------------------- files and changes

/// Working folder plus what git knows about it, for the strip under the
/// composer.
#[tauri::command]
pub async fn workspace_status(
    state: St<'_>,
    chat_id: Option<String>,
) -> Result<crate::workspace::WorkspaceStatus> {
    let cwd = match chat_id {
        Some(id) => state.db.get_chat(&id).ok().and_then(|c| c.cwd),
        None => None,
    };
    // Remote workspaces ask the remote git — branch and dirty count come from
    // where the repo actually lives.
    if let Some(t) = cwd.as_deref().and_then(crate::remote::parse) {
        return Ok(remote_git_status(&state, &t, cwd.clone().unwrap_or_default()).await);
    }
    Ok(crate::workspace::status(cwd).await)
}

/// One round trip that answers everything the workspace bar shows.
async fn remote_git_status(
    state: &St<'_>,
    t: &crate::remote::RemoteTarget,
    cwd: String,
) -> crate::workspace::WorkspaceStatus {
    let mut status = crate::workspace::WorkspaceStatus {
        cwd: Some(cwd),
        exists: true,
        is_repo: false,
        branch: None,
        detached: false,
        dirty: None,
        git_available: true,
    };
    let Ok(r) = crate::remote::pooled(state, &t.host_id).await else {
        return status;
    };
    let script = r#"if git rev-parse --git-dir >/dev/null 2>&1; then b=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); if [ "$b" = HEAD ]; then echo "DETACHED $(git rev-parse --short HEAD)"; else echo "BRANCH $b"; fi; git status --porcelain 2>/dev/null | wc -l; else echo NOREPO; fi"#;
    let Ok((out, _code)) = r.exec(&t.root, script).await else {
        return status;
    };
    let mut lines = out.lines().map(str::trim);
    match lines.next() {
        Some(l) if l.starts_with("BRANCH ") => {
            status.is_repo = true;
            status.branch = Some(l.trim_start_matches("BRANCH ").to_string());
        }
        Some(l) if l.starts_with("DETACHED ") => {
            status.is_repo = true;
            status.detached = true;
            status.branch = Some(l.trim_start_matches("DETACHED ").to_string());
        }
        _ => return status,
    }
    status.dirty = lines.next().and_then(|l| l.parse().ok());
    status
}

#[tauri::command]
pub async fn git_init(state: St<'_>, path: String) -> Result<crate::workspace::WorkspaceStatus> {
    if let Some(t) = crate::remote::parse(&path) {
        let r = crate::remote::pooled(&state, &t.host_id).await?;
        let (out, code) = r.exec(&t.root, "git init").await?;
        if code != 0 {
            return Err(AppError::msg(format!("git init on {}: {}", r.host.label, out.trim())));
        }
        return Ok(remote_git_status(&state, &t, path).await);
    }
    crate::workspace::init(&path).await
}

#[tauri::command]
pub fn fs_home() -> String {
    crate::files::home_dir()
}

#[tauri::command]
pub async fn fs_list(
    state: St<'_>,
    path: String,
    show_hidden: Option<bool>,
) -> Result<Vec<crate::files::FsEntry>> {
    let show = show_hidden.unwrap_or(false);
    if let Some(t) = crate::remote::parse(&path) {
        let r = crate::remote::pooled(&state, &t.host_id).await?;
        let mut out = Vec::new();
        for e in r.list(&t.root).await? {
            let hidden = e.name.starts_with('.');
            if (hidden
                || matches!(
                    e.name.as_str(),
                    "node_modules" | "target" | ".git" | "dist" | ".next" | "__pycache__"
                ))
                && !show
            {
                continue;
            }
            out.push(crate::files::FsEntry {
                path: crate::remote::format(
                    &t.host_id,
                    &format!("{}/{}", t.root.trim_end_matches('/'), e.name),
                ),
                is_dir: e.is_dir,
                size: e.size,
                modified: e.modified,
                hidden,
                expandable: e.is_dir,
                name: e.name,
            });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        return Ok(out);
    }
    crate::files::list_dir(&crate::files::resolve(&path), show)
}

#[tauri::command]
pub async fn fs_read(state: St<'_>, path: String) -> Result<crate::files::FileContents> {
    if let Some(t) = crate::remote::parse(&path) {
        let r = crate::remote::pooled(&state, &t.host_id).await?;
        return remote_file_contents(&r, &t).await;
    }
    crate::files::read_file(&crate::files::resolve(&path))
}

/// Builds the viewer/editor payload for a remote file, mirroring the local
/// `files::read_file` contract.
async fn remote_file_contents(
    r: &crate::remote::Remote,
    t: &crate::remote::RemoteTarget,
) -> Result<crate::files::FileContents> {
    let meta = r.metadata(&t.root).await?;
    let bytes = r.read(&t.root, crate::remote::MAX_REMOTE_READ).await?;
    let name = t.root.rsplit('/').next().unwrap_or_default().to_string();
    let binary = bytes.iter().take(8192).any(|b| *b == 0);
    let content = if binary { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() };
    Ok(crate::files::FileContents {
        path: crate::remote::format(&r.host.id, &t.root),
        lines: content.lines().count(),
        hash: crate::journal::content_hash(&bytes),
        language: crate::files::language_for(&name),
        name,
        content,
        size: meta.size.unwrap_or(bytes.len() as u64),
        binary,
        truncated: false,
        read_only: binary,
        modified: meta.mtime.map(|m| m as i64 * 1000).unwrap_or(0),
    })
}

#[tauri::command]
pub async fn fs_write(
    state: St<'_>,
    path: String,
    content: String,
    expected_hash: Option<String>,
    chat_id: Option<String>,
) -> Result<crate::files::FileContents> {
    if let Some(t) = crate::remote::parse(&path) {
        let r = crate::remote::pooled(&state, &t.host_id).await?;
        // Same guard as locally: refuse to clobber a change made elsewhere
        // between opening the file and saving it.
        if let Some(expected) = expected_hash.as_deref() {
            if let Ok(current) = r.read(&t.root, crate::remote::MAX_REMOTE_READ).await {
                if crate::journal::content_hash(&current) != expected {
                    return Err(AppError::msg(
                        "the remote file changed since it was opened — reload it before saving",
                    ));
                }
            }
        }
        r.write(&t.root, content.as_bytes()).await?;
        return remote_file_contents(&r, &t).await;
    }
    crate::files::write_file(
        &state.db,
        &crate::files::resolve(&path),
        &content,
        expected_hash.as_deref(),
        chat_id.as_deref(),
    )
    .await
}

/// Deletes a file or folder from the file browser. Returns the journal change
/// id for a file deletion (so the UI can offer Undo), None for directories.
#[tauri::command]
pub async fn fs_delete(
    state: St<'_>,
    path: String,
    chat_id: Option<String>,
    recursive: Option<bool>,
) -> Result<Option<String>> {
    if crate::remote::parse(&path).is_some() {
        return Err(AppError::msg(
            "deleting on a remote workspace is not supported from the browser yet — use the terminal",
        ));
    }
    crate::files::delete_path(
        &state.db,
        &crate::files::resolve(&path),
        chat_id.as_deref(),
        recursive.unwrap_or(false),
    )
    .await
}

/// Files this chat has touched, newest first, collapsed to one entry per path.
///
/// The UI shows this as a running "what changed" list beside the conversation —
/// the answer to the question every agent user actually has.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    pub name: String,
    pub latest_change_id: String,
    pub op: String,
    pub origin: String,
    pub edits: usize,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub reverted: bool,
    pub exists: bool,
    pub updated_at: i64,
}

#[tauri::command]
pub fn chat_changes(state: St, chat_id: String) -> Result<Vec<ChangedFile>> {
    let all = state.db.list_file_changes(&chat_id)?;
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    let mut out: Vec<ChangedFile> = Vec::new();

    // `all` is newest-first, so the first row for a path is the current state
    // and later rows only add to the edit count.
    for c in all {
        match seen.get(&c.path) {
            Some(&idx) => {
                out[idx].edits += 1;
                if !c.reverted {
                    out[idx].reverted = false;
                }
                if c.op == "create" {
                    out[idx].op = "create".into();
                }
            }
            None => {
                seen.insert(c.path.clone(), out.len());
                out.push(ChangedFile {
                    name: std::path::Path::new(&c.path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| c.path.clone()),
                    exists: std::path::Path::new(&c.path).exists(),
                    latest_change_id: c.id,
                    op: c.op,
                    origin: c.origin,
                    edits: 1,
                    bytes_before: c.bytes_before,
                    bytes_after: c.bytes_after,
                    reverted: c.reverted,
                    updated_at: c.created_at,
                    path: c.path,
                });
            }
        }
    }
    Ok(out)
}

/// Files created by the agent, newest first. Backs the Artifacts list.
#[tauri::command]
pub fn list_artifacts(
    state: St,
    project_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<crate::db::Artifact>> {
    let all = state.db.list_artifacts(project_id.as_deref(), limit.unwrap_or(40))?;
    // A file that has since been deleted on disk is not an artifact any more.
    Ok(all.into_iter().filter(|a| std::path::Path::new(&a.path).exists()).collect())
}

#[tauri::command]
pub fn list_file_changes(state: St, chat_id: String) -> Result<Vec<crate::db::FileChange>> {
    state.db.list_file_changes(&chat_id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDiff {
    pub path: String,
    pub language: String,
    pub before: String,
    pub after: String,
    pub reverted: bool,
    pub op: String,
}

/// Before/after for the diff viewer. `after` is read from disk, so the diff
/// reflects reality rather than what the model thought it wrote.
#[tauri::command]
pub async fn change_diff(state: St<'_>, change_id: String) -> Result<ChangeDiff> {
    let change = state.db.get_file_change(&change_id)?;
    let before = crate::journal::before_contents(&change).await.unwrap_or_default();
    let after = tokio::fs::read_to_string(&change.path).await.unwrap_or_default();
    let name = std::path::Path::new(&change.path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ChangeDiff {
        language: crate::files::language_for(&name),
        path: change.path,
        before,
        after,
        reverted: change.reverted,
        op: change.op,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertResult {
    pub path: String,
    pub restored_bytes: i64,
    pub was_modified_since: bool,
}

#[tauri::command]
pub async fn revert_change(state: St<'_>, change_id: String, force: Option<bool>) -> Result<RevertResult> {
    let r = crate::journal::revert(&state.db, &change_id, force.unwrap_or(false)).await?;
    Ok(RevertResult {
        path: r.path,
        restored_bytes: r.restored_bytes,
        was_modified_since: r.was_modified_since,
    })
}

/// Reverts everything one run changed. This is the per-message undo: the
/// message's `run_id` names the run, and the journal already knows which
/// writes belonged to it.
#[tauri::command]
pub async fn revert_run(state: St<'_>, run_id: String, force: Option<bool>) -> Result<Vec<RevertResult>> {
    let changes = state.db.run_file_changes(&run_id)?;
    if changes.is_empty() {
        return Err(AppError::msg("this run has no changes left to revert"));
    }
    let mut out = Vec::new();
    let mut failures = Vec::new();
    for c in changes {
        match crate::journal::revert(&state.db, &c.id, force.unwrap_or(false)).await {
            Ok(r) => out.push(RevertResult {
                path: r.path,
                restored_bytes: r.restored_bytes,
                was_modified_since: r.was_modified_since,
            }),
            Err(e) => failures.push(format!("{}: {e}", c.path)),
        }
    }
    if out.is_empty() {
        return Err(AppError::msg(failures.join("; ")));
    }
    Ok(out)
}

/// Reverts every unreverted change in a chat, newest first so that a file
/// edited several times ends up at its original contents.
#[tauri::command]
pub async fn revert_all_changes(
    state: St<'_>,
    chat_id: String,
    force: Option<bool>,
) -> Result<Vec<RevertResult>> {
    let changes = state.db.list_file_changes(&chat_id)?;
    let mut out = Vec::new();
    let mut failures = Vec::new();

    for c in changes.into_iter().filter(|c| !c.reverted) {
        match crate::journal::revert(&state.db, &c.id, force.unwrap_or(false)).await {
            Ok(r) => out.push(RevertResult {
                path: r.path,
                restored_bytes: r.restored_bytes,
                was_modified_since: r.was_modified_since,
            }),
            Err(e) => failures.push(format!("{}: {e}", c.path)),
        }
    }
    if out.is_empty() && !failures.is_empty() {
        return Err(AppError::msg(failures.join("; ")));
    }
    Ok(out)
}
