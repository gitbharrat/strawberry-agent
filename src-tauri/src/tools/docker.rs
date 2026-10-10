//! Docker tools.
//!
//! Thin, structured wrappers over the `docker` CLI rather than the socket API:
//! the CLI is what is guaranteed to be configured on a developer machine
//! (contexts, colima, remote engines all Just Work), and these tools exist so
//! the agent can see and drive containers without shell-quoting guesswork.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;

const MAX_OUTPUT: usize = 60_000;

/// Whether a docker CLI is on PATH. Cached: the environment block asks on
/// every prompt build.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths).any(|d| {
                    let c = d.join("docker");
                    c.is_file() || c.with_extension("exe").is_file()
                })
            })
            .unwrap_or(false)
    })
}

/// Runs `docker <args>` and captures its output. Cancellation kills the child.
/// On a remote workspace the CLI runs on the remote machine — its containers
/// are the ones the model is working next to.
async fn docker(ctx: &ToolCtx, args: &[&str]) -> Result<(String, i32)> {
    if let Some(r) = &ctx.remote {
        let cmd = std::iter::once("docker".to_string())
            .chain(args.iter().map(|a| crate::remote::sh_quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let (out, code) = r.exec(&ctx.cwd.display().to_string(), &cmd).await?;
        let mut text = out;
        if text.len() > MAX_OUTPUT {
            let cut = text.len() - MAX_OUTPUT;
            text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
        }
        return Ok((text, code as i32));
    }
    let child = tokio::process::Command::new("docker")
        .args(args)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::msg(format!("could not run docker (is it installed and on PATH?): {e}")))?;

    // Cancellation drops the wait future, and with it the child handle;
    // kill_on_drop takes the process down.
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
    if text.len() > MAX_OUTPUT {
        // Keep the tail: errors and summaries live at the end.
        let cut = text.len() - MAX_OUTPUT;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }
    Ok((text, out.status.code().unwrap_or(-1)))
}

fn finish(command: &str, text: String, code: i32) -> ToolOutput {
    let body = if text.trim().is_empty() { format!("(no output, exit {code})") } else { text };
    let mut result = ToolOutput::text(format!("$ docker {command}\n{body}\n[exit {code}]"));
    result.is_error = code != 0;
    result.with_display(json!({ "kind": "docker", "command": command, "exitCode": code }))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(args, key).ok_or_else(|| AppError::msg(format!("missing required argument `{key}`")))
}

pub async fn docker_ps(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(true);
    let mut argv = vec!["ps", "--format", "table {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}\t{{.ID}}"];
    if all {
        argv.insert(1, "-a");
    }
    let (text, code) = docker(&ctx, &argv).await?;
    Ok(finish(if all { "ps -a" } else { "ps" }, text, code))
}

pub async fn docker_logs(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let container = req_str(&args, "container")?;
    let tail = args.get("tail").and_then(Value::as_i64).unwrap_or(120).clamp(1, 5000).to_string();
    let mut argv = vec!["logs", "--tail", tail.as_str(), "--timestamps"];
    let since = opt_str(&args, "since");
    if let Some(s) = since {
        argv.push("--since");
        argv.push(s);
    }
    argv.push(container);
    let (text, code) = docker(&ctx, &argv).await?;
    Ok(finish(&format!("logs --tail {tail} {container}"), text, code))
}

pub async fn docker_exec(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let container = req_str(&args, "container")?;
    let command = req_str(&args, "command")?;
    let argv = ["exec", container, "/bin/sh", "-c", command];
    let (text, code) = docker(&ctx, &argv).await?;
    Ok(finish(&format!("exec {container} sh -c '{command}'"), text, code))
}

pub async fn docker_inspect(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let target = req_str(&args, "target")?;
    // A focused summary first; full JSON only on request. Raw `docker inspect`
    // on a busy container is several screens of noise the model pays for.
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
    let (text, code) = if full {
        docker(&ctx, &["inspect", target]).await?
    } else {
        docker(
            &ctx,
            &[
                "inspect",
                "--format",
                "name: {{.Name}}\nimage: {{.Config.Image}}\nstate: {{json .State.Status}} (started {{.State.StartedAt}})\nrestarts: {{.RestartCount}}\ncmd: {{json .Config.Cmd}}\nenv: {{json .Config.Env}}\nports: {{json .NetworkSettings.Ports}}\nmounts: {{range .Mounts}}{{.Source}} -> {{.Destination}} ({{.Mode}}); {{end}}\nnetworks: {{range $k, $v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}} {{end}}",
                target,
            ],
        )
        .await?
    };
    Ok(finish(&format!("inspect {target}"), text, code))
}
