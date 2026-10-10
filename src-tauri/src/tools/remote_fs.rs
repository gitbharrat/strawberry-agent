//! Remote-workspace variants of the file, search and shell tools.
//!
//! Same contracts as the local versions — the model cannot tell where the
//! workspace lives, which is the point. Files travel over SFTP; searches and
//! commands run remotely so only results cross the wire, not trees. Remote
//! writes are not journalled: the undo system snapshots this machine's disk,
//! and pretending otherwise would be a lie in the changed-files panel.

use super::{fs_tools, ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use crate::remote::{self, Remote, MAX_REMOTE_READ};
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_OUTPUT: usize = 60_000;
const REMOTE_NOTE: &str =
    "(remote workspace — this change is not journalled, so it cannot be undone from the app)";

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::msg(format!("missing required argument `{key}`")))
}

/// The connection plus the workspace root, or a plain error if routing broke.
fn target(ctx: &ToolCtx) -> Result<(Arc<Remote>, String)> {
    let r = ctx.remote.clone().ok_or_else(|| AppError::msg("not a remote workspace"))?;
    Ok((r, ctx.cwd.display().to_string()))
}

fn rpath(ctx: &ToolCtx, root: &str, raw: &str) -> Result<String> {
    remote::resolve(root, raw, ctx.settings.sandbox_files)
}

/// Workspace-relative form for output, like `display_path` locally.
fn rdisplay(root: &str, abs: &str) -> String {
    abs.strip_prefix(&format!("{}/", root.trim_end_matches('/'))).unwrap_or(abs).to_string()
}

fn clip_tail(mut text: String) -> String {
    if text.len() > MAX_OUTPUT {
        let cut = text.len() - MAX_OUTPUT;
        text = format!("[… {cut} bytes trimmed …]\n{}", &text[cut..]);
    }
    text
}

pub async fn read_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let path = rpath(&ctx, &root, &arg_str(&args, "path")?)?;
    let bytes = r.read(&path, MAX_REMOTE_READ).await?;
    if bytes.contains(&0) {
        return Ok(ToolOutput::error(format!("{path} looks like a binary file ({} bytes)", bytes.len())));
    }
    let text = String::from_utf8_lossy(&bytes);
    let (out, total, offset, end) = match fs_tools::render_numbered(&text, &args) {
        Ok(v) => v,
        Err(msg) => return Ok(ToolOutput::error(msg)),
    };
    Ok(ToolOutput::text(out).with_display(json!({
        "kind": "file",
        "path": rdisplay(&root, &path),
        "lines": total,
        "from": offset,
        "to": end,
        "remote": r.host.label,
    })))
}

pub async fn write_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let path = rpath(&ctx, &root, &arg_str(&args, "path")?)?;
    let content = arg_str(&args, "content")?;
    let previous = r.read(&path, MAX_REMOTE_READ).await.ok();
    let existed = previous.is_some();
    r.write(&path, content.as_bytes()).await?;

    let rel = rdisplay(&root, &path);
    let diff =
        previous.as_deref().map(|old| fs_tools::unified_diff(&String::from_utf8_lossy(old), &content, &rel));
    Ok(ToolOutput::text(format!(
        "{} {rel} on {} ({} lines, {} bytes) {REMOTE_NOTE}",
        if existed { "Overwrote" } else { "Created" },
        r.host.label,
        content.lines().count(),
        content.len()
    ))
    .with_display(json!({
        "kind": "write",
        "path": rel,
        "created": !existed,
        "lines": content.lines().count(),
        "diff": diff,
        "remote": r.host.label,
    })))
}

pub async fn edit_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let path = rpath(&ctx, &root, &arg_str(&args, "path")?)?;
    let old = arg_str(&args, "old_string")?;
    let new = arg_str(&args, "new_string")?;
    let replace_all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    if old == new {
        return Ok(ToolOutput::error("old_string and new_string are identical"));
    }

    let bytes = r.read(&path, MAX_REMOTE_READ).await?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let rel = rdisplay(&root, &path);

    let count = content.matches(&old).count();
    if count == 0 {
        return Ok(ToolOutput::error(format!(
            "old_string was not found in {rel}. It must match the file exactly, including indentation."
        )));
    }
    if count > 1 && !replace_all {
        return Ok(ToolOutput::error(format!(
            "old_string appears {count} times in {rel}. Add more surrounding context to make it unique, or pass replace_all."
        )));
    }

    let updated = if replace_all { content.replace(&old, &new) } else { content.replacen(&old, &new, 1) };
    r.write(&path, updated.as_bytes()).await?;

    let diff = fs_tools::unified_diff(&content, &updated, &rel);
    Ok(ToolOutput::text(format!(
        "Edited {rel} on {} ({} replacement{}) {REMOTE_NOTE}\n\n{diff}",
        r.host.label,
        if replace_all { count } else { 1 },
        if replace_all && count > 1 { "s" } else { "" }
    ))
    .with_display(json!({
        "kind": "edit",
        "path": rel,
        "replacements": if replace_all { count } else { 1 },
        "diff": diff,
        "remote": r.host.label,
    })))
}

pub async fn list_dir(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let path = rpath(&ctx, &root, raw)?;
    let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(1).clamp(1, 5);

    // One level comes straight off SFTP; deeper walks are a remote `find`, so
    // the tree is built where the files are.
    let mut text = format!("{}\n", rdisplay(&root, &path));
    if depth == 1 {
        let mut entries = r.list(&path).await?;
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        for e in &entries {
            if e.is_dir {
                text.push_str(&format!("  {}/\n", e.name));
            } else {
                text.push_str(&format!("  {}  ({})\n", e.name, fs_tools::human_size(e.size)));
            }
        }
        if entries.is_empty() {
            return Ok(ToolOutput::text(format!("{} is empty", rdisplay(&root, &path))));
        }
    } else {
        let cmd = format!(
            "find . -maxdepth {depth} -not -path '*/node_modules/*' -not -path '*/.git/*' -not -path '*/target/*' | sed 's|^\\./||' | sort | head -n 1000"
        );
        let (out, _code) = r.exec(&path, &cmd).await?;
        for line in out.lines().filter(|l| !l.trim().is_empty() && *l != ".") {
            text.push_str(&format!("  {line}\n"));
        }
    }

    Ok(ToolOutput::text(clip_tail(text)).with_display(json!({
        "kind": "list",
        "path": rdisplay(&root, &path),
        "remote": r.host.label,
    })))
}

pub async fn glob_files(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let pattern = arg_str(&args, "pattern")?;
    let base = match args.get("path").and_then(Value::as_str) {
        Some(p) => rpath(&ctx, &root, p)?,
        None => root.clone(),
    };
    // `find -path` speaks a simpler glob than the local matcher: `**` and `*`
    // both become `*`, which over-matches slightly rather than missing files.
    let fp = format!("*/{}", pattern.replace("**/", "*").replace("**", "*"));
    let cmd = format!(
        "find . -type f -path {} -not -path '*/node_modules/*' -not -path '*/.git/*' | sed 's|^\\./||' | head -n 300",
        remote::sh_quote(&fp)
    );
    let (out, _code) = r.exec(&base, &cmd).await?;
    let n = out.lines().filter(|l| !l.trim().is_empty()).count();
    if n == 0 {
        return Ok(ToolOutput::text(format!("No files match {pattern} on {}", r.host.label)));
    }
    Ok(ToolOutput::text(clip_tail(out))
        .with_display(json!({ "kind": "glob", "count": n, "remote": r.host.label })))
}

pub async fn grep(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let pattern = arg_str(&args, "pattern")?;
    let base = match args.get("path").and_then(Value::as_str) {
        Some(p) => rpath(&ctx, &root, p)?,
        None => root.clone(),
    };
    let max = args.get("max_results").and_then(Value::as_u64).unwrap_or(200).clamp(1, 1000);
    let ci = if args.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false) { "-i " } else { "" };
    let include = args
        .get("glob")
        .and_then(Value::as_str)
        .map(|g| format!("--include={} ", remote::sh_quote(g)))
        .unwrap_or_default();
    let cmd = format!(
        "grep -rn -I -E {ci}{include}--exclude-dir=node_modules --exclude-dir=.git --exclude-dir=target -e {} . 2>/dev/null | sed 's|^\\./||' | head -n {max}",
        remote::sh_quote(&pattern)
    );
    let (out, code) = r.exec(&base, &cmd).await?;
    if out.trim().is_empty() {
        // grep exits 1 for "no matches", which is an answer, not a failure.
        let note = if code > 1 { format!(" (grep exit {code})") } else { String::new() };
        return Ok(ToolOutput::text(format!("No matches for {pattern} on {}{note}", r.host.label)));
    }
    let n = out.lines().count();
    Ok(ToolOutput::text(clip_tail(out))
        .with_display(json!({ "kind": "grep", "count": n, "remote": r.host.label })))
}

pub async fn run_command(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let (r, root) = target(&ctx)?;
    let command = arg_str(&args, "command")?;
    let cwd = match args.get("cwd").and_then(Value::as_str) {
        Some(p) => rpath(&ctx, &root, p)?,
        None => root.clone(),
    };
    let (out, code) = r.exec(&cwd, &command).await?;
    let mut text = clip_tail(out);
    if text.trim().is_empty() {
        text = format!("(no output, exit {code})");
    }
    let mut result = ToolOutput::text(format!("{} $ {command}\n{text}\n[exit {code}]", r.host.label));
    result.is_error = code != 0;
    Ok(result.with_display(json!({
        "kind": "command",
        "command": command,
        "cwd": cwd,
        "exitCode": code,
        "remote": r.host.label,
    })))
}
