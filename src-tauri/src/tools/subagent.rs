//! Subagents: a bounded inner agent loop as a tool.
//!
//! The parent delegates a self-contained task — "find every caller of X",
//! "write the tests for Y" — and gets back only the final answer, keeping the
//! detail out of its own context window. Because the read-only batch rule in
//! the runner applies to `subagent` calls too, the model can fan several out
//! in one turn and they run concurrently.
//!
//! Guard rails:
//!  * a subagent cannot spawn subagents (no recursion) and cannot `ask_user`;
//!  * its writes obey the same permission mode as the parent, minus the
//!    prompts: in a mode that would ask, a subagent simply runs read-only,
//!    because a hidden inner loop must never block on an approval card;
//!  * its steps are bounded and its tool calls carry the parent's run id, so
//!    everything it does is journalled and revertible like any other work.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use crate::openrouter::{ChatRequest, StreamEvent, ToolCall};
use crate::settings::PermissionMode;
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_STEPS: u64 = 12;

/// Tools a subagent may use, depending on what the parent's mode allows
/// without prompting.
fn tool_names_for(mode: PermissionMode) -> Vec<&'static str> {
    let read = vec!["read_file", "list_dir", "glob", "grep"];
    match mode {
        PermissionMode::Auto => {
            let mut all = read;
            all.extend([
                "write_file",
                "edit_file",
                "run_command",
                "fetch_url",
                "web_search",
                "shell_read",
                "shell_list",
            ]);
            all
        }
        PermissionMode::AcceptEdits => {
            let mut all = read;
            all.extend(["write_file", "edit_file"]);
            all
        }
        _ => read,
    }
}

pub async fn subagent(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `task`"))?
        .to_string();
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| ctx.settings.default_model.clone());
    let steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(8).clamp(1, MAX_STEPS);

    let mode = ctx.settings.mode();
    let allowed = tool_names_for(mode);

    let system = format!(
        "You are a focused sub-agent. Complete exactly this task and nothing else:\n\n{task}\n\n\
         Work in {}. {}Your final message is returned verbatim to the agent that \
         spawned you — make it a complete, self-contained answer with the concrete \
         findings (paths, line numbers, code), not a narration of what you did.",
        ctx.cwd.display(),
        if matches!(mode, PermissionMode::ReadOnly | PermissionMode::Ask) {
            "You have read-only access on this run. "
        } else {
            ""
        }
    );

    let (answer, tools_used) =
        run_inner_loop(&ctx, system, task.clone(), model.clone(), steps, &allowed).await?;
    match answer {
        Some(answer) => Ok(ToolOutput::text(answer).with_display(json!({
            "kind": "subagent",
            "task": task,
            "model": model,
            "toolsUsed": tools_used,
        }))),
        None => Ok(ToolOutput::error(format!(
            "The sub-agent hit its step limit ({steps}) before finishing. Partial work may exist; \
             consider a narrower task or a higher max_steps."
        ))),
    }
}

/// The bounded tool loop shared by `subagent` and `deep_research`: run the
/// model with a restricted tool set until it answers without calling tools or
/// runs out of steps. Returns `(final answer or None on step limit, tools used)`.
pub(crate) async fn run_inner_loop(
    ctx: &Arc<ToolCtx>,
    system: String,
    user: String,
    model: String,
    steps: u64,
    allowed: &[&str],
) -> Result<(Option<String>, usize)> {
    let key = ctx.state.api_key()?;
    let client = ctx.state.client();
    let specs: Vec<Value> =
        super::registry().iter().filter(|t| allowed.contains(&t.name)).map(super::ToolDef::to_spec).collect();

    let mut messages =
        vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": user })];
    let mut tools_used = 0usize;

    for _ in 0..steps {
        if ctx.cancel.is_cancelled() {
            return Err(AppError::Cancelled);
        }

        let mut req = ChatRequest::new(model.clone(), messages.clone());
        req.tools = Some(specs.clone());
        let mut sink = |_: StreamEvent| {};
        let turn = client.stream_chat(&key, &req, &ctx.cancel, 1, &mut sink).await?;

        let calls: Vec<ToolCall> =
            turn.tool_calls.iter().filter(|c| !c.function.name.is_empty()).cloned().collect();

        if calls.is_empty() {
            let answer = if turn.content.trim().is_empty() {
                "(the sub-agent finished without producing an answer)".to_string()
            } else {
                turn.content
            };
            return Ok((Some(answer), tools_used));
        }

        let mut assistant = json!({ "role": "assistant", "tool_calls": serde_json::to_value(&calls)? });
        if !turn.content.is_empty() {
            assistant["content"] = json!(turn.content);
        }
        messages.push(assistant);

        for call in &calls {
            let result = match super::find(&call.function.name) {
                Some(def) => {
                    let call_args: Value =
                        serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
                    tools_used += 1;
                    match super::invoke(def, ctx.clone(), call_args).await {
                        Ok(out) => out.content,
                        Err(AppError::Cancelled) => return Err(AppError::Cancelled),
                        Err(e) => format!("Tool failed: {e}"),
                    }
                }
                None => format!("Unknown tool `{}`", call.function.name),
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result,
            }));
        }
    }

    Ok((None, tools_used))
}
