//! Filesystem tools: read, write, edit, list.

use super::{display_path, resolve_path, ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::Arc;

const DEFAULT_LINE_LIMIT: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::msg(format!("missing required argument `{key}`")))
}

pub async fn read_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::read_file(ctx, args).await;
    }
    let path = resolve_path(&ctx, &arg_str(&args, "path")?)?;
    let meta =
        tokio::fs::metadata(&path).await.map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    if meta.is_dir() {
        return Ok(ToolOutput::error(format!("{} is a directory — use list_dir", path.display())));
    }
    if meta.len() > MAX_READ_BYTES {
        return Ok(ToolOutput::error(format!(
            "{} is {:.1} MB, too large to read whole. Use grep to locate what you need, then read with offset/limit.",
            path.display(),
            meta.len() as f64 / 1_048_576.0
        )));
    }

    let bytes = tokio::fs::read(&path).await?;
    if bytes.contains(&0) {
        return Ok(ToolOutput::error(format!(
            "{} looks like a binary file ({} bytes)",
            path.display(),
            bytes.len()
        )));
    }
    let text = String::from_utf8_lossy(&bytes);

    let (out, total, offset, end) = match render_numbered(&text, &args) {
        Ok(v) => v,
        Err(msg) => return Ok(ToolOutput::error(msg)),
    };

    Ok(ToolOutput::text(out).with_display(json!({
        "kind": "file",
        "path": display_path(&ctx, &path),
        "lines": total,
        "from": offset,
        "to": end,
    })))
}

/// Line-numbered rendering shared by the local and remote read_file. Returns
/// (text, total lines, from, to) or a user-facing error message.
pub fn render_numbered(
    text: &str,
    args: &Value,
) -> std::result::Result<(String, usize, usize, usize), String> {
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LINE_LIMIT as u64) as usize;

    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = offset - 1;
    if start >= total && total > 0 {
        return Err(format!("offset {offset} is past the end ({total} lines)"));
    }
    let end = (start + limit).min(total);

    let mut out = String::with_capacity((end - start) * 64);
    for (i, line) in all[start..end].iter().enumerate() {
        let n = start + i + 1;
        if line.chars().count() > MAX_LINE_CHARS {
            let clipped: String = line.chars().take(MAX_LINE_CHARS).collect();
            out.push_str(&format!("{n:>6}\t{clipped}… [line truncated]\n"));
        } else {
            out.push_str(&format!("{n:>6}\t{line}\n"));
        }
    }
    if end < total {
        out.push_str(&format!(
            "\n[showing lines {offset}-{end} of {total}; call again with offset={} to continue]\n",
            end + 1
        ));
    }
    if total == 0 {
        out.push_str("[empty file]\n");
    }
    Ok((out, total, offset, end))
}

pub async fn write_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::write_file(ctx, args).await;
    }
    let path = resolve_path(&ctx, &arg_str(&args, "path")?)?;
    let content = arg_str(&args, "content")?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let previous = tokio::fs::read(&path).await.ok();
    let existed = previous.is_some();

    tokio::fs::write(&path, content.as_bytes()).await?;
    crate::journal::record(
        &ctx.db,
        crate::journal::Provenance::agent(&ctx.chat_id, &ctx.run_id, &ctx.call_id),
        &path,
        previous.as_deref(),
        content.as_bytes(),
    )
    .await?;

    let lines = content.lines().count();
    let diff = previous
        .as_deref()
        .map(|old| unified_diff(&String::from_utf8_lossy(old), &content, &display_path(&ctx, &path)));

    Ok(ToolOutput::text(format!(
        "{} {} ({lines} lines, {} bytes)",
        if existed { "Overwrote" } else { "Created" },
        display_path(&ctx, &path),
        content.len()
    ))
    .with_display(json!({
        "kind": "write",
        "path": display_path(&ctx, &path),
        "created": !existed,
        "lines": lines,
        "diff": diff,
    })))
}

pub async fn edit_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::edit_file(ctx, args).await;
    }
    let path = resolve_path(&ctx, &arg_str(&args, "path")?)?;
    let old = arg_str(&args, "old_string")?;
    let new = arg_str(&args, "new_string")?;
    let replace_all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);

    if old == new {
        return Ok(ToolOutput::error("old_string and new_string are identical"));
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;

    let count = content.matches(&old).count();
    if count == 0 {
        return Ok(ToolOutput::error(format!(
            "old_string was not found in {}. It must match the file exactly, including indentation.",
            display_path(&ctx, &path)
        )));
    }
    if count > 1 && !replace_all {
        return Ok(ToolOutput::error(format!(
            "old_string appears {count} times in {}. Add more surrounding context to make it unique, or pass replace_all.",
            display_path(&ctx, &path)
        )));
    }

    let updated = if replace_all { content.replace(&old, &new) } else { content.replacen(&old, &new, 1) };
    tokio::fs::write(&path, updated.as_bytes()).await?;
    crate::journal::record(
        &ctx.db,
        crate::journal::Provenance::agent(&ctx.chat_id, &ctx.run_id, &ctx.call_id),
        &path,
        Some(content.as_bytes()),
        updated.as_bytes(),
    )
    .await?;

    let rel = display_path(&ctx, &path);
    let diff = unified_diff(&content, &updated, &rel);
    Ok(ToolOutput::text(format!(
        "Edited {rel} ({} replacement{})\n\n{diff}",
        count.min(if replace_all { count } else { 1 }),
        if replace_all && count > 1 { "s" } else { "" }
    ))
    .with_display(json!({ "kind": "edit", "path": rel, "replacements": if replace_all { count } else { 1 }, "diff": diff })))
}

pub async fn list_dir(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::list_dir(ctx, args).await;
    }
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let root = resolve_path(&ctx, raw)?;
    let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(1).clamp(1, 5) as usize;

    let root2 = root.clone();
    let entries = tokio::task::spawn_blocking(move || {
        let mut out: Vec<(bool, String, u64)> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root2)
            .max_depth(Some(depth))
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .follow_links(false)
            .build();
        for entry in walker.flatten() {
            if entry.depth() == 0 {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            let rel = entry.path().strip_prefix(&root2).unwrap_or(entry.path()).display().to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((is_dir, rel, size));
            if out.len() >= 1000 {
                break;
            }
        }
        out
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;

    if entries.is_empty() {
        return Ok(ToolOutput::text(format!("{} is empty", display_path(&ctx, &root))));
    }

    let mut sorted = entries;
    sorted.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut text = format!("{}\n", display_path(&ctx, &root));
    for (is_dir, rel, size) in &sorted {
        if *is_dir {
            text.push_str(&format!("  {rel}/\n"));
        } else {
            text.push_str(&format!("  {rel}  ({})\n", human_size(*size)));
        }
    }
    if sorted.len() >= 1000 {
        text.push_str("  [truncated at 1000 entries]\n");
    }

    Ok(ToolOutput::text(text).with_display(json!({
        "kind": "list",
        "path": display_path(&ctx, &root),
        "count": sorted.len(),
    })))
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Compact unified diff. Both the model and the UI read this, so it is kept
/// small — full context would blow up the token budget on large files.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let mut any = false;
    for group in diff.grouped_ops(3).iter() {
        any = true;
        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => "-",
                    similar::ChangeTag::Insert => "+",
                    similar::ChangeTag::Equal => " ",
                };
                out.push_str(sign);
                out.push_str(change.value());
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        out.push_str("@@\n");
    }
    if !any {
        return "(no textual change)".into();
    }
    if out.len() > 12_000 {
        out.truncate(12_000);
        out.push_str("\n… [diff truncated]\n");
    }
    out
}
