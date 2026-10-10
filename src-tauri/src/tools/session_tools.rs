//! Background shell session tools.
//!
//! The pattern the model is taught: `shell_start` a server or long build,
//! `shell_read` with `wait_for` to block until the interesting line appears,
//! `shell_send` to drive anything interactive, `shell_kill` when done. Reads
//! are cursor-based, so repeated calls stream the output without repetition.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::msg(format!("missing required argument `{key}`")))
}

pub async fn shell_start(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let command = arg_str(&args, "command")?;
    let session = ctx.state.shells.start(&command, &ctx.cwd)?;

    // A brief settle read so the model immediately sees startup output (or an
    // instant crash) instead of a bare id.
    let first = ctx
        .state
        .shells
        .read(&session.id, 0, None, Duration::from_millis(700))
        .await
        .map(|r| r.text)
        .unwrap_or_default();

    Ok(ToolOutput::text(format!(
        "Started session {} running `{command}`.\n{}\nUse shell_read with this session_id (and a cursor of {}) for more output.",
        session.id,
        if first.is_empty() { "(no output yet)".to_string() } else { first.clone() },
        first.len(),
    ))
    .with_display(json!({ "kind": "shellStart", "sessionId": session.id, "command": command })))
}

pub async fn shell_read(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let id = arg_str(&args, "session_id")?;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0);
    let wait_ms = args.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);

    let pattern = match args.get("wait_for").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => {
            Some(regex_lite::Regex::new(p).map_err(|e| AppError::msg(format!("bad wait_for regex: {e}")))?)
        }
        _ => None,
    };

    // The wait is capped below the tool deadline so this read always returns
    // with whatever exists rather than dying by timeout.
    let cap = ctx.timeout.saturating_sub(Duration::from_secs(5)).max(Duration::from_millis(100));
    let deadline = Duration::from_millis(wait_ms).min(cap);

    let read = ctx.state.shells.read(&id, cursor, pattern.as_ref(), deadline).await?;

    let mut text = read.text.clone();
    if text.len() > 40_000 {
        let cut = text.len() - 40_000;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }
    let mut notes = Vec::new();
    if read.trimmed {
        notes.push("older output was trimmed from the buffer".to_string());
    }
    if pattern.is_some() {
        notes.push(if read.matched {
            "pattern matched".into()
        } else {
            "pattern did NOT match before the wait ended".into()
        });
    }
    if !read.alive {
        notes.push("the session has exited".into());
    }

    Ok(ToolOutput::text(format!(
        "{}\n[cursor: {} · {}]",
        if text.is_empty() { "(no new output)".to_string() } else { text },
        read.cursor,
        if notes.is_empty() { "running".to_string() } else { notes.join(" · ") },
    ))
    .with_display(
        json!({ "kind": "shellRead", "sessionId": id, "alive": read.alive, "matched": read.matched }),
    ))
}

pub async fn shell_send(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let id = arg_str(&args, "session_id")?;
    let mut input = arg_str(&args, "input")?;
    if args.get("press_enter").and_then(Value::as_bool).unwrap_or(true) && !input.ends_with('\n') {
        input.push('\n');
    }
    ctx.state.shells.write(&id, input.as_bytes())?;
    Ok(ToolOutput::text(format!("Sent. Follow with shell_read on session {id} to see the response.")))
}

pub async fn shell_kill(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let id = arg_str(&args, "session_id")?;
    ctx.state.shells.kill(&id)?;
    Ok(ToolOutput::text(format!("Killed session {id}.")))
}

pub async fn shell_list(ctx: Arc<ToolCtx>, _args: Value) -> Result<ToolOutput> {
    let sessions = ctx.state.shells.list();
    if sessions.is_empty() {
        return Ok(ToolOutput::text("No background sessions."));
    }
    let text = sessions
        .iter()
        .map(|s| {
            format!(
                "{}  `{}`  {}  ({} bytes buffered)",
                s.id,
                s.command,
                if s.alive { "running" } else { "exited" },
                s.buffered_bytes
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::text(text))
}
