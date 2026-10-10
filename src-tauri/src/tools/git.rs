//! Git tools.
//!
//! Structured wrappers so the model stops shell-quoting its way through
//! version control: status, diff, log, commit and branch as first-class calls
//! with clipped output. On a remote workspace every one of them runs on that
//! machine, over the same pooled connection as the file tools — git follows
//! the repo, wherever the repo lives.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;

const MAX_OUTPUT: usize = 60_000;

/// Runs `git <args>` in the workspace — locally or on the remote host.
async fn git(ctx: &ToolCtx, args: &[&str]) -> Result<(String, i32)> {
    if let Some(r) = &ctx.remote {
        let cmd = std::iter::once("git".to_string())
            .chain(args.iter().map(|a| crate::remote::sh_quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let (out, code) = r.exec(&ctx.cwd.display().to_string(), &cmd).await?;
        return Ok((clip(out), code as i32));
    }

    let child = tokio::process::Command::new("git")
        .args(args)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::msg(format!("could not run git: {e}")))?;

    let out = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
        r = child.wait_with_output() => r.map_err(|e| AppError::msg(e.to_string()))?,
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok((clip(text), out.status.code().unwrap_or(-1)))
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_OUTPUT {
        let cut = text.len() - MAX_OUTPUT;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }
    text
}

fn finish(what: &str, text: String, code: i32) -> ToolOutput {
    let body = if text.trim().is_empty() { format!("(no output, exit {code})") } else { text };
    let mut result = ToolOutput::text(format!("$ git {what}\n{body}\n[exit {code}]"));
    result.is_error = code != 0;
    result.with_display(json!({ "kind": "git", "command": what, "exitCode": code }))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

pub async fn git_status(ctx: Arc<ToolCtx>, _args: Value) -> Result<ToolOutput> {
    let (text, code) = git(&ctx, &["status", "--porcelain=v1", "-b"]).await?;
    Ok(finish("status", text, code))
}

pub async fn git_diff(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let mut argv: Vec<String> = vec!["diff".into()];
    if args.get("staged").and_then(Value::as_bool).unwrap_or(false) {
        argv.push("--cached".into());
    }
    if let Some(base) = opt_str(&args, "base") {
        argv.push(base.to_string());
    }
    if let Some(path) = opt_str(&args, "path") {
        argv.push("--".into());
        argv.push(path.to_string());
    }
    let shown = argv.join(" ");
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (text, code) = git(&ctx, &refs).await?;
    Ok(finish(&shown, text, code))
}

pub async fn git_log(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let n = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 200).to_string();
    let mut argv: Vec<&str> = vec!["log", "--oneline", "--decorate", "-n", &n];
    let path = opt_str(&args, "path").map(str::to_owned);
    if let Some(p) = &path {
        argv.push("--");
        argv.push(p);
    }
    let (text, code) = git(&ctx, &argv).await?;
    Ok(finish(&format!("log -n {n}"), text, code))
}

pub async fn git_commit(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let message = opt_str(&args, "message")
        .ok_or_else(|| AppError::msg("missing required argument `message`"))?
        .to_string();
    let add_all = args.get("add_all").and_then(Value::as_bool).unwrap_or(true);
    if add_all {
        let (out, code) = git(&ctx, &["add", "-A"]).await?;
        if code != 0 {
            return Ok(finish("add -A", out, code));
        }
    }
    let (text, code) = git(&ctx, &["commit", "-m", &message]).await?;
    Ok(finish("commit", text, code))
}

pub async fn git_branch(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if let Some(name) = opt_str(&args, "create") {
        let (text, code) = git(&ctx, &["switch", "-c", name]).await?;
        return Ok(finish(&format!("switch -c {name}"), text, code));
    }
    if let Some(name) = opt_str(&args, "switch") {
        let (text, code) = git(&ctx, &["switch", name]).await?;
        return Ok(finish(&format!("switch {name}"), text, code));
    }
    let (text, code) = git(&ctx, &["branch", "--all", "-v"]).await?;
    Ok(finish("branch --all -v", text, code))
}
