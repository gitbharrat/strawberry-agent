//! tmux tools.
//!
//! Our own background sessions die with the app; tmux sessions do not, and
//! users often already have one full of state. These tools drive tmux through
//! its CLI — the one interface it guarantees — so the agent can create
//! sessions, type into panes, and read them back, including panes a human is
//! attached to at the same time.
//!
//! tmux has no push channel, so `wait_for` here is a capture-poll loop rather
//! than the notify-based wait the in-app sessions get. The poll interval is
//! coarse enough to be invisible in CPU terms and fine enough that "wait for
//! the build to finish" feels immediate.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Control operations are instant; anything slower means the server is wedged.
const TMUX_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_EVERY: Duration = Duration::from_millis(250);
const DEFAULT_LINES: i64 = 200;
const MAX_LINES: i64 = 2000;

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::msg(format!("missing required argument `{key}`")))
}

/// Runs one tmux command. No shell in between: arguments go to the binary
/// verbatim, so nothing the model sends can be reinterpreted by quoting.
async fn tmux(args: &[&str]) -> Result<String> {
    let out = tokio::time::timeout(
        TMUX_TIMEOUT,
        tokio::process::Command::new("tmux")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| AppError::Timeout(TMUX_TIMEOUT.as_millis() as u64))?
    .map_err(|e| AppError::msg(format!("could not run tmux: {e}")))?;

    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.contains("error connecting to") || err.contains("no server running") {
        return Err(AppError::msg(
            "no tmux server is running. Start one with tmux_run (it creates the session), or ask the user to run tmux.",
        ));
    }
    Err(AppError::msg(if err.is_empty() { "tmux failed".into() } else { err }))
}

/// The session part of a `session[:window[.pane]]` target.
fn session_of(target: &str) -> &str {
    target.split(':').next().unwrap_or(target)
}

async fn has_session(name: &str) -> bool {
    // `has-session` exits non-zero both for "no such session" and "no server",
    // which is exactly the answer we need here.
    tmux(&["has-session", "-t", name]).await.is_ok()
}

pub async fn tmux_list(_ctx: Arc<ToolCtx>, _args: Value) -> Result<ToolOutput> {
    let panes = match tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}:#{window_index}.#{pane_index}  #{pane_current_command}  #{pane_width}x#{pane_height}#{?client_attached,  (user attached),}",
    ])
    .await
    {
        Ok(p) => p,
        Err(e) if e.to_string().contains("no tmux server") => {
            return Ok(ToolOutput::text(
                "No tmux server is running. tmux_run with a new session name will start one.",
            ));
        }
        Err(e) => return Err(e),
    };

    Ok(ToolOutput::text(format!("tmux panes (target  command  size):\n{}", panes.trim_end())))
}

pub async fn tmux_run(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let target = arg_str(&args, "target")?;
    let command = arg_str(&args, "command")?;
    let session = session_of(&target).to_string();

    let mut created = false;
    if !has_session(&session).await {
        // A fresh detached session, homed in the chat's working directory so
        // relative paths behave the way the rest of the harness does.
        tmux(&["new-session", "-d", "-s", &session, "-c", &ctx.cwd.display().to_string()]).await?;
        created = true;
    }

    // Literal text first, then the Enter key — two calls, so nothing in the
    // command line is ever parsed as a key name.
    tmux(&["send-keys", "-t", &target, "-l", "--", &command]).await?;
    tmux(&["send-keys", "-t", &target, "Enter"]).await?;

    Ok(ToolOutput::text(format!(
        "{}Sent to tmux `{target}`. Use tmux_capture (with wait_for if you expect specific output) to read the pane.",
        if created { format!("Created tmux session `{session}`. ") } else { String::new() },
    ))
    .with_display(json!({ "kind": "tmux", "target": target, "command": command, "created": created })))
}

pub async fn tmux_capture(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let target = arg_str(&args, "target")?;
    let lines = args.get("lines").and_then(Value::as_i64).unwrap_or(DEFAULT_LINES).clamp(10, MAX_LINES);
    let wait_ms = args.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
    let pattern = match args.get("wait_for").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => {
            Some(regex_lite::Regex::new(p).map_err(|e| AppError::msg(format!("bad wait_for regex: {e}")))?)
        }
        _ => None,
    };

    let cap = ctx.timeout.saturating_sub(Duration::from_secs(5)).max(Duration::from_millis(100));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms).min(cap);
    let history = format!("-{lines}");

    loop {
        let text = tmux(&["capture-pane", "-p", "-t", &target, "-S", &history]).await?;
        let trimmed = text.trim_end();

        let matched = pattern.as_ref().map(|re| re.is_match(trimmed));
        let done = match matched {
            None => true,
            Some(true) => true,
            Some(false) => tokio::time::Instant::now() >= deadline,
        };
        if done {
            let note = match matched {
                Some(true) => " · pattern matched",
                Some(false) => " · pattern did NOT match before the wait ended",
                None => "",
            };
            return Ok(ToolOutput::text(format!("Last {lines} lines of `{target}`{note}:\n{trimmed}")));
        }

        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
            () = tokio::time::sleep(POLL_EVERY) => {}
        }
    }
}

pub async fn tmux_kill(_ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let target = arg_str(&args, "target")?;
    tmux(&["kill-session", "-t", session_of(&target)]).await?;
    Ok(ToolOutput::text(format!("Killed tmux session `{}`.", session_of(&target))))
}

/// Cheap availability probe for the system-prompt environment block.
pub fn available() -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|d| {
                let c = d.join("tmux");
                c.is_file() || c.with_extension("exe").is_file()
            })
        })
        .unwrap_or(false)
}
