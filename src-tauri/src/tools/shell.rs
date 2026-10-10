//! Execution tools: local shell and remote SSH.

use super::{resolve_path, ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

const MAX_OUTPUT: usize = 60_000;

/// The shell to run tool commands through.
///
/// Deliberately NOT a login shell. `-l` sources the user's profile, and
/// profiles routinely do slow or blocking things — version-manager init,
/// update checks, integrations that expect a tty. In the field that showed up
/// as `rm` taking minutes. A plain non-interactive `-c` runs the command and
/// nothing else; PATH still arrives via the environment this app inherited
/// from wherever it was launched. The interactive terminal surface keeps the
/// full login shell — aliases belong there, not in tool calls.
fn tool_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        let sh = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        return (sh, vec!["/C".into()]);
    }
    let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    (sh, vec!["-c".into()])
}

/// Runs a Python snippet with the workspace's interpreter — local or remote.
/// A heredoc through `run_command` keeps the code byte-exact (no shell
/// quoting) and inherits every existing behaviour: cwd resolution, timeout,
/// sandboxing, remote execution.
pub async fn run_python(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let code = args
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `code`"))?;
    const MARK: &str = "SB_PY_EOF";
    if code.contains(MARK) {
        return Ok(ToolOutput::error("the code may not contain the literal SB_PY_EOF"));
    }
    let command = format!(
        "PY=$(command -v python3 || command -v python) && exec \"$PY\" - <<'{MARK}'\n{code}\n{MARK}"
    );
    let mut forwarded = serde_json::json!({ "command": command });
    if let Some(cwd) = args.get("cwd") {
        forwarded["cwd"] = cwd.clone();
    }
    if let Some(t) = args.get("timeout_ms") {
        forwarded["timeout_ms"] = t.clone();
    }
    run_command(ctx, forwarded).await
}

pub async fn run_command(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::run_command(ctx, args).await;
    }
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `command`"))?
        .to_string();
    let cwd = match args.get("cwd").and_then(Value::as_str) {
        Some(p) => resolve_path(&ctx, p)?,
        None => ctx.cwd.clone(),
    };

    let (program, argv) = build_invocation(&ctx, &command);
    let mut child = tokio::process::Command::new(program)
        .args(&argv)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Nothing here has a terminal; saying so stops tools from waiting on one.
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("CI", "1")
        // Without this the child survives cancellation and keeps writing to a
        // pipe nobody reads.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::msg(format!("could not start shell: {e}")))?;

    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let mut out = Vec::new();
    let mut err = Vec::new();

    // Waiting for EOF on the pipes is not the same as waiting for the child: a
    // login shell's profile can spawn background processes that inherit the
    // write ends, and those keep the pipes open long after the command itself
    // has finished. So reads run concurrently, the child's exit is the signal
    // to stop, and a short grace period drains whatever is still buffered.
    let status = {
        let reads = async {
            let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        };
        tokio::pin!(reads);
        // A completed future must never be polled again, so remember which
        // select branch won before deciding whether to drain.
        let mut reads_done = false;

        let exit = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                let _ = child.start_kill();
                return Err(AppError::Cancelled);
            }
            r = child.wait() => r.map_err(|e| AppError::msg(e.to_string()))?,
            // Pipes closing first means the child is done writing; its exit
            // follows immediately.
            () = &mut reads => {
                reads_done = true;
                child.wait().await.map_err(|e| AppError::msg(e.to_string()))?
            }
        };

        if !reads_done {
            // Child is gone; give lingering pipe-holders half a second, then
            // move on with whatever arrived.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), &mut reads).await;
        }
        exit
    };

    let mut text = String::new();
    if !out.is_empty() {
        text.push_str(&String::from_utf8_lossy(&out));
    }
    if !err.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err));
    }
    let truncated = text.len() > MAX_OUTPUT;
    if truncated {
        // Keep the tail: error messages and summaries live at the end.
        let cut = text.len() - MAX_OUTPUT;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }
    let code = status.code().unwrap_or(-1);
    if text.trim().is_empty() {
        text = format!("(no output, exit {code})");
    }

    let mut result = ToolOutput::text(format!("$ {command}\n{text}\n[exit {code}]"));
    result.is_error = code != 0;
    Ok(result.with_display(json!({
        "kind": "command",
        "command": command,
        "cwd": cwd.display().to_string(),
        "exitCode": code,
        "truncated": truncated,
    })))
}

/// Builds the argv for a command, wrapping it in an OS sandbox when one is
/// enabled and available.
///
/// This is defence in depth, not a security boundary: a determined command can
/// still reach the network and read most of the disk. What it does buy is
/// protection against the realistic failure — an agent running something that
/// writes outside the workspace by mistake.
fn build_invocation(ctx: &ToolCtx, command: &str) -> (String, Vec<String>) {
    let (shell, flags) = tool_shell();
    let mut inner: Vec<String> = flags;
    inner.push(command.to_string());

    if !ctx.sandbox_commands || ctx.allowed_roots.is_empty() {
        return (shell, inner);
    }

    #[cfg(target_os = "macos")]
    {
        if std::path::Path::new("/usr/bin/sandbox-exec").exists() {
            let mut argv = vec!["-p".to_string(), seatbelt_profile(&ctx.allowed_roots)];
            argv.push(shell);
            argv.extend(inner);
            return ("/usr/bin/sandbox-exec".to_string(), argv);
        }
    }

    (shell, inner)
}

/// A seatbelt profile that allows everything except writes outside the
/// workspace and the usual scratch directories.
#[cfg(target_os = "macos")]
fn seatbelt_profile(roots: &[std::path::PathBuf]) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*");
    for r in roots {
        p.push_str(&format!("\n  (subpath \"{}\")", r.display()));
    }
    for extra in ["/tmp", "/private/tmp", "/private/var/tmp", "/dev"] {
        p.push_str(&format!("\n  (subpath \"{extra}\")"));
    }
    p.push_str(")\n");
    p
}

pub async fn ssh_exec(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let host_id = args
        .get("host_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `host_id`"))?;
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `command`"))?
        .to_string();

    let host = ctx.db.get_ssh_host(host_id)?;
    let (output, code) = crate::ssh::exec_once(&ctx.db, &host, &command, &ctx.cancel).await?;
    ctx.db.touch_ssh_host(host_id)?;

    let mut text = output;
    if text.len() > MAX_OUTPUT {
        let cut = text.len() - MAX_OUTPUT;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }

    let mut result =
        ToolOutput::text(format!("{}@{} $ {command}\n{text}\n[exit {code}]", host.username, host.host));
    result.is_error = code != 0;
    Ok(result.with_display(json!({
        "kind": "ssh",
        "host": host.label,
        "command": command,
        "exitCode": code,
    })))
}

/// Shared arg plumbing for the two transfer tools: host row + sandboxed local
/// path + remote path.
fn transfer_args(ctx: &ToolCtx, args: &Value) -> Result<(crate::db::SshHost, std::path::PathBuf, String)> {
    let host_id = args
        .get("host_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `host_id`"))?;
    let local = args
        .get("local_path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `local_path`"))?;
    let remote = args
        .get("remote_path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `remote_path`"))?
        .to_string();
    let host = ctx.db.get_ssh_host(host_id)?;
    // The local side honours the same sandbox as every file tool; the remote
    // side is whatever the SSH account can see, which is the point.
    let local = resolve_path(ctx, local)?;
    Ok((host, local, remote))
}

pub async fn ssh_download(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (host, local, remote) = transfer_args(&ctx, &args)?;
    let n = crate::ssh::sftp_download(&ctx.db, &host, &remote, &local, &ctx.cancel).await?;
    ctx.db.touch_ssh_host(&host.id)?;
    Ok(ToolOutput::text(format!(
        "Downloaded {remote} from {} to {} ({n} bytes).",
        host.label,
        local.display()
    ))
    .with_display(json!({ "kind": "sshTransfer", "direction": "download", "host": host.label, "bytes": n })))
}

pub async fn ssh_upload(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (host, local, remote) = transfer_args(&ctx, &args)?;
    let n = crate::ssh::sftp_upload(&ctx.db, &host, &local, &remote, &ctx.cancel).await?;
    ctx.db.touch_ssh_host(&host.id)?;
    Ok(ToolOutput::text(format!("Uploaded {} to {}:{remote} ({n} bytes).", local.display(), host.label))
        .with_display(
            json!({ "kind": "sshTransfer", "direction": "upload", "host": host.label, "bytes": n }),
        ))
}

pub async fn list_ssh_hosts(ctx: Arc<ToolCtx>, _args: Value) -> Result<ToolOutput> {
    let hosts = ctx.db.list_ssh_hosts()?;
    if hosts.is_empty() {
        return Ok(ToolOutput::text("No SSH hosts configured. The user can add one in Settings → SSH."));
    }
    let text = hosts
        .iter()
        .map(|h| format!("{}  {}@{}:{}  (id: {})", h.label, h.username, h.host, h.port, h.id))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::text(text).with_display(json!({ "kind": "sshHosts", "count": hosts.len() })))
}
