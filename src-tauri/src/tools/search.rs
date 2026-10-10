//! Search tools built on ripgrep's own engine (`ignore` + `grep-searcher`).
//!
//! These run on the blocking pool: they are CPU- and syscall-bound, and keeping
//! them off the async workers is what stops a search of a huge tree from
//! stalling a concurrently streaming response.

use super::{display_path, resolve_path, ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const MAX_HITS: usize = 200;

pub async fn glob_files(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::glob_files(ctx, args).await;
    }
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `pattern`"))?
        .to_string();
    let root = resolve_path(&ctx, args.get("path").and_then(Value::as_str).unwrap_or("."))?;

    let glob = globset::GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| AppError::msg(format!("bad glob `{pattern}`: {e}")))?
        .compile_matcher();

    let root2 = root.clone();
    let mut hits = tokio::task::spawn_blocking(move || {
        let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
        for entry in ignore::WalkBuilder::new(&root2).hidden(false).git_ignore(true).build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let rel = entry.path().strip_prefix(&root2).unwrap_or(entry.path());
            if !glob.is_match(rel) && !glob.is_match(entry.path()) {
                continue;
            }
            let mtime =
                entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
            found.push((mtime, entry.path().to_path_buf()));
            if found.len() >= 1000 {
                break;
            }
        }
        found
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;

    // Newest first: when someone globs, they are almost always looking for what
    // changed recently.
    hits.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));

    if hits.is_empty() {
        return Ok(ToolOutput::text(format!(
            "No files match `{pattern}` under {}",
            display_path(&ctx, &root)
        )));
    }
    let list: Vec<String> = hits.iter().map(|(_, p)| display_path(&ctx, p)).collect();
    let text = format!("{} match(es) for `{pattern}`:\n{}", list.len(), list.join("\n"));
    Ok(ToolOutput::text(text)
        .with_display(json!({ "kind": "glob", "pattern": pattern, "count": list.len() })))
}

pub async fn grep(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    if ctx.remote.is_some() {
        return super::remote_fs::grep(ctx, args).await;
    }
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `pattern`"))?
        .to_string();
    let root = resolve_path(&ctx, args.get("path").and_then(Value::as_str).unwrap_or("."))?;
    let file_glob = args.get("glob").and_then(Value::as_str).map(str::to_owned);
    let ci = args.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false);
    let max = args.get("max_results").and_then(Value::as_u64).unwrap_or(MAX_HITS as u64) as usize;

    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(ci)
        .line_terminator(Some(b'\n'))
        .build(&pattern)
        .map_err(|e| AppError::msg(format!("bad regex `{pattern}`: {e}")))?;

    let glob = match &file_glob {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| AppError::msg(format!("bad glob `{g}`: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };

    let root2 = root.clone();
    let cancel = ctx.cancel.clone();
    let results = tokio::task::spawn_blocking(move || {
        let count = AtomicUsize::new(0);
        let mut lines: Vec<(std::path::PathBuf, u64, String)> = Vec::new();
        let mut searcher =
            SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();

        for entry in ignore::WalkBuilder::new(&root2).hidden(false).git_ignore(true).build().flatten() {
            if cancel.is_cancelled() || count.load(Ordering::Relaxed) >= max {
                break;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if let Some(g) = &glob {
                let name = entry.file_name();
                if !g.is_match(entry.path()) && !g.is_match(std::path::Path::new(name)) {
                    continue;
                }
            }
            let path = entry.path().to_path_buf();
            let _ = searcher.search_path(
                &matcher,
                &path,
                UTF8(|lnum, line| {
                    if count.fetch_add(1, Ordering::Relaxed) < max {
                        lines.push((path.clone(), lnum, line.trim_end().to_string()));
                    }
                    Ok(count.load(Ordering::Relaxed) < max)
                }),
            );
        }
        lines
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()))?;

    if results.is_empty() {
        return Ok(ToolOutput::text(format!(
            "No matches for `{pattern}` in {}{}",
            display_path(&ctx, &root),
            file_glob.map(|g| format!(" (files matching {g})")).unwrap_or_default()
        )));
    }

    let mut text = format!("{} match(es) for `{pattern}`:\n", results.len());
    for (path, lnum, line) in &results {
        let clipped: String = line.chars().take(400).collect();
        text.push_str(&format!("{}:{lnum}: {clipped}\n", display_path(&ctx, path)));
    }
    if results.len() >= max {
        text.push_str(&format!("[stopped at {max} matches — narrow the pattern or pass a glob]\n"));
    }

    let files: std::collections::BTreeSet<_> = results.iter().map(|(p, ..)| display_path(&ctx, p)).collect();
    Ok(ToolOutput::text(text).with_display(json!({
        "kind": "grep",
        "pattern": pattern,
        "matches": results.len(),
        "files": files.len(),
    })))
}
