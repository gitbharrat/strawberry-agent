//! Chat memory tools.
//!
//! Every conversation the user has ever had with this app is in SQLite; these
//! two tools turn that into the agent's long-term memory. `memory_search`
//! finds where something was discussed, `memory_read_chat` pulls the relevant
//! transcript back — compactly, because past chats can be enormous and only a
//! slice of one is ever worth the context.

use super::{ToolCtx, ToolOutput};
use crate::error::Result;
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_HITS: i64 = 20;
const MAX_MESSAGES: usize = 60;
/// Per-message clip when reading a transcript. Tool output is model context.
const CLIP_CHARS: usize = 700;
const TOOL_CLIP_CHARS: usize = 160;

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

pub async fn memory_search(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if query.len() < 2 {
        return Ok(ToolOutput::error("query must be at least 2 characters"));
    }
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, MAX_HITS as u64) as i64;

    // Over-fetch so hits from the current conversation can be dropped — the
    // model already has this chat in context.
    let hits = ctx.db.search(&query, limit * 3)?;
    let mut out = String::new();
    let mut count = 0usize;
    for h in hits {
        if h.chat_id == ctx.chat_id {
            continue;
        }
        out.push_str(&format!(
            "[{}] {} · {} · chat_id={}\n  {}: {}\n",
            count + 1,
            h.chat_title,
            when(h.created_at),
            h.chat_id,
            h.role,
            clip(&h.snippet, 220),
        ));
        count += 1;
        if count as i64 >= limit {
            break;
        }
    }

    if count == 0 {
        return Ok(ToolOutput::text(format!(
            "No past-chat messages match \"{query}\". Try a shorter or different keyword."
        )));
    }
    out.push_str("\nUse memory_read_chat with a chat_id to read the surrounding conversation.");
    Ok(ToolOutput::text(out).with_display(json!({ "kind": "memory", "query": query, "hits": count })))
}

pub async fn memory_read_chat(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let chat_id = args.get("chat_id").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if chat_id.is_empty() {
        return Ok(ToolOutput::error("missing required argument `chat_id` — get one from memory_search"));
    }
    let limit =
        args.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, MAX_MESSAGES as u64) as usize;

    let chat = match ctx.db.get_chat(&chat_id) {
        Ok(c) => c,
        Err(_) => return Ok(ToolOutput::error(format!("no chat with id {chat_id}"))),
    };
    let messages = ctx.db.list_messages(&chat_id)?;
    if messages.is_empty() {
        return Ok(ToolOutput::text(format!("Chat \"{}\" has no messages.", chat.title)));
    }

    let total = messages.len();
    let tail = &messages[total.saturating_sub(limit)..];

    let mut out = format!(
        "Chat \"{}\" · started {} · {} message(s){}\n\n",
        chat.title,
        when(chat.created_at),
        total,
        if total > tail.len() { format!(" (showing the last {})", tail.len()) } else { String::new() },
    );
    for m in tail {
        match m.role.as_str() {
            // Tool chatter is mostly noise at recall time; one clipped line
            // preserves what happened without the bulk.
            "tool" => out.push_str(&format!("  [tool result: {}]\n", clip(&m.content, TOOL_CLIP_CHARS))),
            role => {
                if m.content.is_empty() {
                    continue;
                }
                out.push_str(&format!("{role}: {}\n", clip(&m.content, CLIP_CHARS)));
            }
        }
    }

    Ok(ToolOutput::text(out).with_display(json!({
        "kind": "memory",
        "chat": chat.title,
        "messages": tail.len(),
    })))
}
