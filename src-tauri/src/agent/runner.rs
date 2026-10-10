//! The agent loop.
//!
//! One run = one user turn, expanded into as many model calls and tool
//! executions as it takes to finish. The loop is a background tokio task: it
//! keeps going if the window is closed, it can be cancelled at any await point,
//! and every intermediate state is written to SQLite so nothing is lost.

use super::prompt;
use super::{AgentEvent, RunHandle};
use crate::db::NewMessage;
use crate::error::{AppError, Result};
use crate::openrouter::{ChatRequest, StreamEvent, ToolCall, Turn};
use crate::settings::{PermissionMode, Settings};
use crate::state::AppState;
use crate::tools::{self, Risk, ToolEnv};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Flush accumulated text at least this often even if the model is slow, so a
/// trickling response still feels alive.
const MAX_COALESCE: Duration = Duration::from_millis(120);
/// Flush early once this much text is buffered.
const COALESCE_BYTES: usize = 2048;

/// Placeholder a chat carries until the model names it.
pub const UNTITLED: &str = "New chat";

/// Batches token deltas so the UI receives ~60 updates a second instead of one
/// per token. This single buffer is the difference between a chat that scrolls
/// smoothly and one that stutters at high token rates.
struct Coalescer {
    buf: String,
    last: Instant,
    budget: Duration,
}

impl Coalescer {
    fn new(budget_ms: u64) -> Self {
        Self {
            buf: String::with_capacity(COALESCE_BYTES),
            last: Instant::now(),
            budget: Duration::from_millis(budget_ms.clamp(0, MAX_COALESCE.as_millis() as u64)),
        }
    }

    /// Returns the batch to emit, if it is time.
    fn push(&mut self, text: &str) -> Option<String> {
        self.buf.push_str(text);
        if self.buf.len() >= COALESCE_BYTES || self.last.elapsed() >= self.budget {
            self.last = Instant::now();
            return Some(std::mem::take(&mut self.buf));
        }
        None
    }

    fn drain(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            self.last = Instant::now();
            Some(std::mem::take(&mut self.buf))
        }
    }
}

pub struct RunContext {
    pub state: Arc<AppState>,
    pub handle: Arc<RunHandle>,
    pub run_id: String,
    pub chat_id: String,
}

impl RunContext {
    pub(crate) fn emit(&self, ev: AgentEvent) {
        // A send failure only means nobody is watching right now; the run
        // continues and the UI catches up from the database.
        let _ = self.handle.tx.send(ev);
    }
}

/// Entry point. Always terminates by emitting `RunFinished` and clearing the
/// registry entry, whatever happens inside.
pub async fn execute(ctx: RunContext) {
    let outcome = run_inner(&ctx).await;

    // Naming runs regardless of how the turn ended: an errored or cancelled
    // chat is exactly the one the user will want to find again in the rail.
    if !ctx.handle.cancel.is_cancelled() {
        if let Ok(key) = ctx.state.api_key() {
            let client = ctx.state.client();
            let model = ctx.state.settings().default_model;
            maybe_title(&ctx, &client, &key, &model).await;
        }
    }

    let (status, error) = match &outcome {
        Ok(()) => ("complete", None),
        Err(AppError::Cancelled) => ("cancelled", None),
        Err(e) => ("error", Some(e.to_string())),
    };

    // If this run edited files in a mapped workspace, refresh the architecture
    // map in the background so the next turn (and the graph) see reality.
    if status == "complete" {
        crate::analysis::maybe_auto_update(ctx.state.clone(), ctx.chat_id.clone(), ctx.run_id.clone());
    }

    let _ = ctx.state.db.finish_run(&ctx.run_id, status, error.as_deref());
    let _ = ctx.state.db.touch_chat(&ctx.chat_id);
    ctx.emit(AgentEvent::RunFinished { run_id: ctx.run_id.clone(), status: status.into(), error });
    ctx.state.runs.finish(&ctx.run_id);
}

async fn run_inner(ctx: &RunContext) -> Result<()> {
    let settings = ctx.state.settings();
    let key = ctx.state.api_key()?;
    let client = ctx.state.client();
    let db = ctx.state.db.clone();

    let chat = db.get_chat(&ctx.chat_id)?;
    let project = match chat.project_id.as_deref() {
        Some(id) => db.get_project(id).ok(),
        None => None,
    };

    let model = chat.model.clone().unwrap_or_else(|| settings.default_model.clone());
    let caps = model_caps(ctx, &model);

    let tools_on = settings.tools_enabled && caps.tools;
    if settings.tools_enabled && !caps.tools {
        ctx.emit(AgentEvent::Notice {
            level: "warn".into(),
            text: format!("{model} does not support tool calling — running without tools."),
        });
    }
    let tool_specs = tools_on.then(|| tools::specs(&settings.enabled_tools));

    // A `ssh://host/path` working directory swings the whole run to that
    // machine: tools get the pooled connection and treat `cwd` as remote.
    let remote_target = chat.cwd.as_deref().and_then(crate::remote::parse);
    let (cwd, remote) = match &remote_target {
        Some(t) => {
            let r = crate::remote::pooled(&ctx.state, &t.host_id)
                .await
                .map_err(|e| AppError::msg(format!("the remote workspace is unreachable: {e}")))?;
            (PathBuf::from(&t.root), Some(r))
        }
        None => (resolve_cwd(&chat, project.as_ref()), None),
    };
    let allowed_roots = resolve_roots(&settings, &cwd);
    let denied_paths: Vec<PathBuf> =
        settings.denied_paths.iter().map(|p| crate::tools::expand_home(p)).collect();
    let system = prompt::system_prompt(&settings, project.as_ref(), &chat, tools_on);

    ctx.emit(AgentEvent::RunStarted {
        run_id: ctx.run_id.clone(),
        chat_id: ctx.chat_id.clone(),
        model: model.clone(),
    });

    let history_limit = if settings.low_bandwidth {
        settings.history_limit.min(40) as usize
    } else {
        settings.history_limit as usize
    };

    let mut compacted = false;
    for step in 0..settings.max_steps {
        if ctx.handle.cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        ctx.emit(AgentEvent::StepStarted { step });

        let stored = db.list_messages(&ctx.chat_id)?;
        // Everything after this seq arrived while the step below streamed —
        // the signal that the user steered mid-run and still awaits a reply.
        let snapshot_seq = stored.last().map(|m| m.seq).unwrap_or(0);
        let mut messages = vec![json!({ "role": "system", "content": system })];
        messages.extend(prompt::encode_history(&db, &stored, history_limit, caps.images)?);

        let mut req = ChatRequest::new(model.clone(), messages);
        req.tools = tool_specs.clone();
        req.temperature = settings.temperature;
        req.max_tokens = settings.max_tokens;
        req.top_p = settings.top_p;
        if !settings.fallback_models.is_empty() {
            // OpenRouter tries these in order if the primary is unavailable —
            // the cheapest possible resilience against a provider outage.
            let mut chain = vec![model.clone()];
            chain.extend(settings.fallback_models.iter().cloned());
            req.models = Some(chain);
        }
        if caps.reasoning {
            if let Some(effort) = &settings.reasoning_effort {
                req.reasoning = Some(json!({ "effort": effort, "exclude": !settings.show_reasoning }));
            }
        }

        let turn = stream_turn(ctx, &client, &key, &req, &settings, &model).await?;

        // Persist before doing anything else: if the process dies here, the
        // work already done is still in the chat.
        let assistant_id = persist_assistant(ctx, &turn, &model)?;

        if let Some(u) = &turn.usage {
            db.bump_run_usage(&ctx.run_id, u.prompt_tokens, u.completion_tokens, u.cost, 1)?;
            ctx.emit(AgentEvent::Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                cost: u.cost,
            });

            // Auto-compaction: once the prompt crosses the configured share of
            // the window, fold the older history before the next step. At most
            // once per run — if it did not help, doing it again will not.
            if !compacted && settings.auto_compact_pct > 0 {
                if let Some(window) = context_window(ctx, &model) {
                    if u.prompt_tokens as f64 >= window as f64 * settings.auto_compact_pct as f64 / 100.0 {
                        compacted = true;
                        match super::compact::compact_chat(ctx.state.clone(), &ctx.chat_id, None).await {
                            Ok(o) => ctx.emit(AgentEvent::Notice {
                                level: "info".into(),
                                text: format!(
                                    "Context auto-compacted: {} message(s) folded into a summary.",
                                    o.removed
                                ),
                            }),
                            Err(e) => tracing::debug!("auto-compact skipped: {e}"),
                        }
                    }
                }
            }
        }

        if let Some(err) = &turn.error {
            return Err(AppError::msg(err.clone()));
        }

        let calls: Vec<ToolCall> =
            turn.tool_calls.iter().filter(|c| !c.function.name.is_empty()).cloned().collect();

        if calls.is_empty() {
            // Steered input that arrived after this step's request snapshot
            // has not been answered yet — take another step for it instead of
            // finishing past it.
            let steered = db
                .list_messages(&ctx.chat_id)?
                .iter()
                .any(|m| m.role == "user" && m.seq > snapshot_seq);
            if !steered {
                return Ok(());
            }
            continue;
        }

        let env = ToolEnv {
            db: db.clone(),
            cwd: cwd.clone(),
            remote: remote.clone(),
            allowed_roots: allowed_roots.clone(),
            denied_paths: denied_paths.clone(),
            cancel: ctx.handle.cancel.clone(),
            timeout: Duration::from_millis(settings.tool_timeout_ms),
            sandbox_commands: settings.sandbox_commands,
            handle: ctx.handle.clone(),
            state: ctx.state.clone(),
            settings: settings.clone(),
            chat_id: ctx.chat_id.clone(),
            run_id: ctx.run_id.clone(),
        };
        run_tool_calls(ctx, &settings, &env, &assistant_id, &calls).await?;
    }

    ctx.emit(AgentEvent::Notice {
        level: "warn".into(),
        text: format!(
            "Stopped after {} steps. Send another message to continue, or raise the step limit in Settings.",
            settings.max_steps
        ),
    });
    Ok(())
}

// ------------------------------------------------------------------ streaming

pub(crate) async fn stream_turn(
    ctx: &RunContext,
    client: &crate::openrouter::Client,
    key: &str,
    req: &ChatRequest,
    settings: &Settings,
    model: &str,
) -> Result<Turn> {
    let message_id = crate::db::new_id();
    ctx.emit(AgentEvent::MessageStarted { message_id: message_id.clone(), role: "assistant".into() });

    let mut content = Coalescer::new(settings.stream_coalesce_ms);
    let mut reasoning = Coalescer::new(settings.stream_coalesce_ms.max(48));
    let mut started_tools: std::collections::HashSet<String> = Default::default();

    let result = {
        let mid = message_id.clone();
        let mut on = |ev: StreamEvent| match ev {
            StreamEvent::Content(t) => {
                if let Some(batch) = content.push(&t) {
                    ctx.emit(AgentEvent::Delta { message_id: mid.clone(), text: batch });
                }
            }
            StreamEvent::Reasoning(t) => {
                if let Some(batch) = reasoning.push(&t) {
                    ctx.emit(AgentEvent::Reasoning { message_id: mid.clone(), text: batch });
                }
            }
            StreamEvent::ToolCallDelta { id, name, args_delta } => {
                if !id.is_empty() && !name.is_empty() && started_tools.insert(id.clone()) {
                    // Flush prose before announcing a tool so the UI order
                    // matches the order the model produced things in.
                    if let Some(batch) = content.drain() {
                        ctx.emit(AgentEvent::Delta { message_id: mid.clone(), text: batch });
                    }
                    ctx.emit(AgentEvent::ToolCallStarted { id: id.clone(), name });
                }
                if !args_delta.is_empty() {
                    ctx.emit(AgentEvent::ToolCallArgs { id, delta: args_delta });
                }
            }
            StreamEvent::Retry { attempt, delay_ms, reason } => {
                ctx.emit(AgentEvent::Retry { attempt, delay_ms, reason });
            }
            StreamEvent::Error(text) => {
                ctx.emit(AgentEvent::Notice { level: "error".into(), text });
            }
            StreamEvent::Finish(reason) => {
                if reason == "length" {
                    ctx.emit(AgentEvent::Notice {
                        level: "warn".into(),
                        text: "The model hit its output limit and stopped mid-answer. Raise \"max output tokens\" in Settings, or ask it to continue.".into(),
                    });
                }
            }
        };

        client.stream_chat(key, req, &ctx.handle.cancel, settings.max_retries, &mut on).await
    };

    // Whatever happened, do not strand buffered text.
    if let Some(batch) = content.drain() {
        ctx.emit(AgentEvent::Delta { message_id: message_id.clone(), text: batch });
    }
    if let Some(batch) = reasoning.drain() {
        ctx.emit(AgentEvent::Reasoning { message_id: message_id.clone(), text: batch });
    }

    let mut turn = result?;
    turn.id = Some(message_id);
    let _ = model;
    Ok(turn)
}

pub(crate) fn persist_assistant(ctx: &RunContext, turn: &Turn, model: &str) -> Result<String> {
    let id = turn.id.clone().unwrap_or_else(crate::db::new_id);
    let tool_calls =
        if turn.tool_calls.is_empty() { None } else { Some(serde_json::to_string(&turn.tool_calls)?) };
    let usage = turn.usage.as_ref().map(serde_json::to_string).transpose()?;

    ctx.state.db.insert_message(&NewMessage {
        id: Some(id.clone()),
        chat_id: ctx.chat_id.clone(),
        role: "assistant".into(),
        content: turn.content.clone(),
        reasoning: (!turn.reasoning.is_empty()).then(|| turn.reasoning.clone()),
        tool_calls,
        tool_call_id: None,
        model: Some(model.to_string()),
        status: if turn.error.is_some() { "error".into() } else { "complete".into() },
        error: turn.error.clone(),
        usage: usage.clone(),
        run_id: Some(ctx.run_id.clone()),
    })?;

    ctx.emit(AgentEvent::MessageFinished {
        message_id: id.clone(),
        content: turn.content.clone(),
        reasoning: (!turn.reasoning.is_empty()).then(|| turn.reasoning.clone()),
        finish_reason: turn.finish_reason.clone(),
        usage: usage.and_then(|u| serde_json::from_str(&u).ok()),
    });
    Ok(id)
}

// ----------------------------------------------------------------- tool calls

pub(crate) async fn run_tool_calls(
    ctx: &RunContext,
    settings: &Settings,
    env: &ToolEnv,
    assistant_id: &str,
    calls: &[ToolCall],
) -> Result<()> {
    // Read-only calls have no ordering constraints between them, so run the
    // whole batch concurrently. Anything that writes, executes or leaves the
    // machine runs one at a time, in the order the model asked for.
    let all_read =
        calls.iter().all(|c| tools::find(&c.function.name).map(|d| d.risk == Risk::Read).unwrap_or(false));

    if all_read && calls.len() > 1 {
        let mut set = Vec::with_capacity(calls.len());
        for call in calls {
            set.push(execute_one(ctx, settings, env, assistant_id.to_string(), call.clone()));
        }
        for r in futures_util::future::join_all(set).await {
            r?;
        }
    } else {
        for call in calls {
            execute_one(ctx, settings, env, assistant_id.to_string(), call.clone()).await?;
        }
    }
    Ok(())
}

async fn execute_one(
    ctx: &RunContext,
    settings: &Settings,
    env: &ToolEnv,
    assistant_id: String,
    call: ToolCall,
) -> Result<()> {
    let started = Instant::now();
    let name = call.function.name.clone();
    let call_id = if call.id.is_empty() { crate::db::new_id() } else { call.id.clone() };

    let args: Value = if call.function.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}))
    };

    ctx.state.db.insert_tool_call(&crate::db::ToolCallRecord {
        id: call_id.clone(),
        run_id: ctx.run_id.clone(),
        chat_id: ctx.chat_id.clone(),
        message_id: Some(assistant_id),
        name: name.clone(),
        args: args.to_string(),
        result: None,
        status: "running".into(),
        error: None,
        started_at: chrono::Utc::now().timestamp_millis(),
        finished_at: None,
    })?;

    let Some(def) = tools::find(&name) else {
        return finish_tool(
            ctx,
            &call_id,
            &name,
            false,
            format!("Unknown tool `{name}`. Available tools: {}", tool_names()),
            None,
            started,
        );
    };

    // Malformed JSON arguments are a model error, not a crash: tell it plainly
    // so it can retry with valid arguments.
    if !call.function.arguments.trim().is_empty()
        && serde_json::from_str::<Value>(&call.function.arguments).is_err()
    {
        return finish_tool(
            ctx,
            &call_id,
            &name,
            false,
            "Tool arguments were not valid JSON. Send them again as a well-formed JSON object.".into(),
            None,
            started,
        );
    }

    // A call whose path argument lands in a protected credential store always
    // prompts — every mode, no "always allow". The user saying yes to the
    // exact call is the only thing that opens that door, and only for it.
    let protected = protected_path(env, &args);
    let decision = match (gate(settings, def.risk), &protected) {
        (Gate::Refuse(r), _) => Gate::Refuse(r),
        (_, Some(_)) => Gate::Prompt,
        (g, None) => g,
    };
    match decision {
        Gate::Allow => {}
        Gate::Refuse(reason) => {
            return finish_tool(ctx, &call_id, &name, false, reason.into(), None, started);
        }
        Gate::Prompt => {
            ctx.emit(AgentEvent::ToolApproval {
                id: call_id.clone(),
                name: name.clone(),
                args: args.clone(),
                risk: format!("{:?}", def.risk).to_lowercase(),
                warning: protected.as_ref().map(|p| {
                    format!(
                        "{p} is a protected credential store (SSH keys, tokens). Allowing this one call exposes its contents to the model."
                    )
                }),
            });
            if !await_approval(ctx, &call_id).await? {
                return finish_tool(
                    ctx,
                    &call_id,
                    &name,
                    false,
                    "The user declined to run this tool. Ask what they would like to do instead.".into(),
                    None,
                    started,
                );
            }
        }
    }

    ctx.emit(AgentEvent::ToolStarted { id: call_id.clone(), name: name.clone(), args: args.clone() });

    let tctx = env.ctx(&call_id, protected.is_some());
    let (ok, text, display) = match tools::invoke(def, tctx, args).await {
        Ok(out) => (!out.is_error, out.content, out.display),
        Err(AppError::Cancelled) => return Err(AppError::Cancelled),
        Err(e) => (false, format!("Tool `{name}` failed: {e}"), None),
    };

    finish_tool(ctx, &call_id, &name, ok, text, display, started)
}

fn finish_tool(
    ctx: &RunContext,
    call_id: &str,
    name: &str,
    ok: bool,
    text: String,
    display: Option<Value>,
    started: Instant,
) -> Result<()> {
    ctx.state.db.finish_tool_call(
        call_id,
        if ok { "complete" } else { "error" },
        Some(&text),
        if ok { None } else { Some(text.as_str()) },
    )?;

    ctx.state.db.insert_message(&NewMessage {
        chat_id: ctx.chat_id.clone(),
        role: "tool".into(),
        content: text.clone(),
        tool_call_id: Some(call_id.to_string()),
        status: "complete".into(),
        run_id: Some(ctx.run_id.clone()),
        ..Default::default()
    })?;

    ctx.emit(AgentEvent::ToolFinished {
        id: call_id.to_string(),
        name: name.to_string(),
        ok,
        summary: summarize(&text),
        display,
        duration_ms: started.elapsed().as_millis() as i64,
    });
    Ok(())
}

/// Blocks the run until the user answers the approval prompt, or the run is
/// cancelled. There is no timeout by design — an unanswered prompt is a visible
/// state in the UI, not a hang.
async fn await_approval(ctx: &RunContext, call_id: &str) -> Result<bool> {
    let (tx, rx) = oneshot::channel();
    ctx.handle.approvals.lock().insert(call_id.to_string(), tx);
    tokio::select! {
        biased;
        _ = ctx.handle.cancel.cancelled() => Err(AppError::Cancelled),
        answer = rx => Ok(answer.unwrap_or(false)),
    }
}

/// What the current permission mode says about a tool of this risk.
enum Gate {
    /// Run it without asking.
    Allow,
    /// Ask the user first.
    Prompt,
    /// Refuse, and tell the model why.
    Refuse(&'static str),
}

/// The first path-like argument that lands inside a protected credential
/// store, if any. Checked before the tool runs so the approval prompt can say
/// exactly what is about to be exposed.
fn protected_path(env: &ToolEnv, args: &Value) -> Option<String> {
    if env.remote.is_some() {
        // Remote paths are the remote machine's business; the deny list
        // protects this machine's credential stores.
        return None;
    }
    for key in ["path", "cwd", "local_path"] {
        let Some(raw) = args.get(key).and_then(Value::as_str) else { continue };
        let abs = tools::lexical_absolute(&env.cwd, &tools::expand_home(raw));
        let target = abs.canonicalize().unwrap_or_else(|_| abs.clone());
        for denied in &env.denied_paths {
            let d = denied.canonicalize().unwrap_or_else(|_| denied.clone());
            if target.starts_with(&d) || abs.starts_with(denied) {
                return Some(abs.display().to_string());
            }
        }
    }
    None
}

fn gate(s: &Settings, risk: Risk) -> Gate {
    use PermissionMode::*;
    match (s.mode(), risk) {
        (_, Risk::Read) => Gate::Allow,

        (ReadOnly, Risk::Write) => Gate::Refuse(
            "This chat is in read-only mode, so files cannot be modified. Describe the change you would make and let the user switch modes if they want it applied.",
        ),
        (ReadOnly, Risk::Exec) => Gate::Refuse(
            "This chat is in read-only mode, so commands cannot be run. Say which command you would run and why.",
        ),
        (ReadOnly, Risk::Network) => Gate::Refuse(
            "This chat is in read-only mode, so network requests are not allowed.",
        ),

        (Ask, _) => Gate::Prompt,

        (AcceptEdits, Risk::Write) => Gate::Allow,
        (AcceptEdits, _) => Gate::Prompt,

        (Auto, _) => Gate::Allow,
    }
}

fn summarize(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    let s: String = first.chars().take(160).collect();
    if s.is_empty() {
        format!("{} bytes", text.len())
    } else {
        s
    }
}

fn tool_names() -> String {
    tools::registry().iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

// -------------------------------------------------------------------- helpers

struct Caps {
    tools: bool,
    images: bool,
    reasoning: bool,
}

/// Reads capabilities out of the cached model catalogue. If the catalogue has
/// not been fetched yet we assume the model is fully capable rather than
/// silently disabling tools — the request will simply fail loudly if not.
fn model_caps(ctx: &RunContext, model: &str) -> Caps {
    let cached = ctx.state.db.cached_models().ok().flatten();
    let Some((payload, _)) = cached else {
        return Caps { tools: true, images: true, reasoning: true };
    };
    let Ok(list) = serde_json::from_str::<Vec<crate::openrouter::ModelInfo>>(&payload) else {
        return Caps { tools: true, images: true, reasoning: true };
    };
    match list.iter().find(|m| m.id == model) {
        Some(m) => {
            Caps { tools: m.supports_tools(), images: m.supports_images(), reasoning: m.supports_reasoning() }
        }
        None => Caps { tools: true, images: true, reasoning: true },
    }
}

/// The model's context window from the cached catalogue, if known.
fn context_window(ctx: &RunContext, model: &str) -> Option<i64> {
    let (payload, _) = ctx.state.db.cached_models().ok().flatten()?;
    let list: Vec<crate::openrouter::ModelInfo> = serde_json::from_str(&payload).ok()?;
    let m = list.iter().find(|m| m.id == model)?;
    m.context_length.or_else(|| m.top_provider.as_ref().and_then(|t| t.context_length))
}

pub(crate) fn resolve_cwd(chat: &crate::db::Chat, project: Option<&crate::db::Project>) -> PathBuf {
    chat.cwd
        .as_deref()
        .or(project.and_then(|p| p.root_path.as_deref()))
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

/// Sandbox roots for file tools.
///
/// With the sandbox on (the default) the agent may only touch this chat's
/// working directory plus anything the user has explicitly added. Turning it off
/// returns an empty list, which `resolve_path` reads as unconfined.
pub(crate) fn resolve_roots(settings: &Settings, cwd: &Path) -> Vec<PathBuf> {
    if !settings.sandbox_files {
        return Vec::new();
    }
    let mut roots: Vec<PathBuf> =
        settings.allowed_roots.iter().map(|p| crate::tools::expand_home(p)).collect();
    roots.push(cwd.to_path_buf());
    roots
}

/// Names an untitled chat from its first exchange.
///
/// Best-effort: a failure here must never affect the run, so a model that
/// returns nothing usable falls back to a trimmed version of the opening
/// message. A chat always ends up with a better name than "New chat".
pub(crate) async fn maybe_title(
    ctx: &RunContext,
    client: &crate::openrouter::Client,
    key: &str,
    model: &str,
) {
    let Ok(chat) = ctx.state.db.get_chat(&ctx.chat_id) else { return };
    let Ok(messages) = ctx.state.db.list_messages(&ctx.chat_id) else { return };
    let user = messages.iter().find(|m| m.role == "user").map(|m| m.content.clone());
    let assistant = messages.iter().rev().find(|m| m.role == "assistant" && !m.content.is_empty());
    let (Some(user), Some(assistant)) = (user, assistant) else { return };

    // Retitle while the chat still carries a placeholder — including the
    // echo-the-message fallback, so a later successful model call upgrades it.
    if chat.title != UNTITLED && chat.title != fallback_title(&user) {
        return;
    }

    let mut req = ChatRequest::new(model, prompt::title_prompt(&user, &assistant.content));
    // Keep any thinking out of the reply. `enabled: false` is NOT sent:
    // endpoints with mandatory reasoning (GLM flash among them) 400 on it,
    // which made every title fall back to the trimmed first message.
    req.reasoning = Some(json!({ "exclude": true }));
    req.max_tokens = Some(64);
    req.temperature = Some(0.3);

    let mut sink = |_: StreamEvent| {};
    let cancel = ctx.handle.cancel.clone();
    let turn = client.stream_chat(key, &req, &cancel, 0, &mut sink).await.ok();

    // Reasoning models sometimes put the answer in the reasoning channel even
    // when asked not to think, so both channels are candidates.
    let title = turn
        .as_ref()
        .and_then(|t| clean_title(&t.content))
        .or_else(|| turn.as_ref().and_then(|t| clean_title(&t.reasoning)))
        .unwrap_or_else(|| fallback_title(&user));
    if title.is_empty() {
        return;
    }

    let _ = ctx.state.db.update_chat(&crate::db::ChatPatch {
        id: ctx.chat_id.clone(),
        title: Some(title.clone()),
        model: None,
        system_prompt: None,
        cwd: None,
        project_id: None,
        archived: None,
        pinned: None,
    });
    ctx.emit(AgentEvent::TitleChanged { chat_id: ctx.chat_id.clone(), title });
}

/// Accepts a model-written title, or `None` if it is unusable.
fn clean_title(raw: &str) -> Option<String> {
    // Reasoning models sometimes prefix a line of preamble; the title is the
    // last non-empty line.
    let line = raw.lines().rev().find(|l| !l.trim().is_empty())?;
    let trimmed = line
        .trim()
        .trim_start_matches(['#', '-', '*', '>'])
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim_end_matches(['.', '!', ':'])
        .trim();
    if trimmed.is_empty() || trimmed.chars().count() > 60 {
        return None;
    }
    Some(trimmed.to_string())
}

/// Deterministic name from the opening message, used when the model gives us
/// nothing we can use.
fn fallback_title(user: &str) -> String {
    let first = user.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let words: Vec<&str> = first.split_whitespace().take(7).collect();
    if words.is_empty() {
        return String::new();
    }
    let mut title = words.join(" ");
    title.truncate(58);
    title
}
