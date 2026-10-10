//! Context compaction.
//!
//! Long conversations drown their own signal: the prompt refills with stale
//! tool output on every step. Compaction folds the older history into one
//! structured summary and prunes the originals. The techniques that make it
//! more than a naive "summarise everything":
//!
//!  * **rolling** — a previous summary is fed back as the base and updated,
//!    never re-derived from scratch, so repeated compactions stay cheap and
//!    stable;
//!  * **structured** — fixed sections (goal, decisions, code state, open
//!    threads, constraints) so later turns can rely on where facts live;
//!  * **tool elision** — tool output is clipped to a line before the model
//!    ever sees it, and identical consecutive results collapse to one ×N;
//!  * **token-budgeted keep window** — the untouched tail is chosen by
//!    estimated size, cut at a user-message boundary so tool-call pairs are
//!    never split.

use crate::db::Message;
use crate::error::{AppError, Result};
use crate::openrouter::{ChatRequest, StreamEvent};
use crate::state::AppState;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

/// Marks the summary message; also how a later compaction finds its base.
pub const SUMMARY_MARK: &str = "⧉ Context summary";

/// Roughly chars/4 — only used to pick the cut, never billed.
fn approx_tokens(s: &str) -> usize {
    s.len() / 4
}

const KEEP_MIN_MSGS: usize = 6;
const KEEP_TOKENS: usize = 2400;
const MIN_COMPACTABLE: usize = 6;
const CLIP_MSG: usize = 1400;
const CLIP_TOOL: usize = 160;

const PROMPT: &str = "Fold this conversation history into a context summary for continued work. \
Markdown with exactly these sections: ## Goal · ## Decisions · ## Code state (files touched, what changed) \
· ## Open threads · ## Constraints & preferences. Keep file paths and identifiers verbatim. \
Terse bullets, max ~350 words. Reply with the summary alone.";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactOutcome {
    pub removed: usize,
    pub kept: usize,
    pub summary_chars: usize,
}

/// Picks the first index of the kept tail: newest-backwards until both the
/// message minimum and the token budget are satisfied, then walks back to a
/// user message so an assistant turn is never split from its tool results.
fn keep_from(messages: &[Message]) -> usize {
    let mut idx = messages.len();
    let mut kept_tokens = 0usize;
    while idx > 0 {
        let m = &messages[idx - 1];
        kept_tokens += approx_tokens(&m.content);
        idx -= 1;
        if messages.len() - idx >= KEEP_MIN_MSGS && kept_tokens >= KEEP_TOKENS {
            break;
        }
    }
    while idx > 0 && messages[idx].role != "user" {
        idx -= 1;
    }
    idx
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

/// Renders the history-to-fold with tool elision and duplicate collapsing.
fn render_input(base: Option<&str>, older: &[Message]) -> String {
    let mut out = String::new();
    if let Some(b) = base {
        out.push_str("Previous summary (update it, do not restart):\n");
        out.push_str(b);
        out.push_str("\n\nNewer messages to fold in:\n");
    }
    let mut last_line: Option<String> = None;
    let mut repeat = 0usize;
    let flush = |out: &mut String, line: &Option<String>, repeat: usize| {
        if let Some(l) = line {
            out.push_str(l);
            if repeat > 1 {
                out.push_str(&format!(" (×{repeat})"));
            }
            out.push('\n');
        }
    };
    for m in older {
        let line = match m.role.as_str() {
            "tool" => format!("tool: {}", clip(&m.content, CLIP_TOOL)),
            role => {
                if m.content.is_empty() {
                    continue;
                }
                format!("{role}: {}", clip(&m.content, CLIP_MSG))
            }
        };
        if last_line.as_deref() == Some(line.as_str()) {
            repeat += 1;
            continue;
        }
        flush(&mut out, &last_line, repeat);
        last_line = Some(line);
        repeat = 1;
    }
    flush(&mut out, &last_line, repeat);
    out
}

/// Compacts a chat in place. Safe to run mid-conversation: the tail is
/// untouched and the next model step simply reloads the shorter history.
pub async fn compact_chat(
    state: Arc<AppState>,
    chat_id: &str,
    model_override: Option<String>,
) -> Result<CompactOutcome> {
    let key = state.api_key()?;
    let messages = state.db.list_messages(chat_id)?;

    // A previous summary is the rolling base; everything before it is already
    // folded, so only messages after it are candidates.
    let base_idx = messages.iter().rposition(|m| m.role == "user" && m.content.starts_with(SUMMARY_MARK));
    let start = base_idx.map(|i| i + 1).unwrap_or(0);
    let keep = keep_from(&messages).max(start);
    if keep.saturating_sub(start) < MIN_COMPACTABLE {
        return Err(AppError::msg(
            "not enough history to compact yet — the recent tail is always kept as-is",
        ));
    }

    let base = base_idx.map(|i| messages[i].content.trim_start_matches(SUMMARY_MARK).trim());
    let input = render_input(base, &messages[start..keep]);

    let model = model_override
        .filter(|m| !m.trim().is_empty())
        .or_else(|| state.db.get_chat(chat_id).ok().and_then(|c| c.model))
        .unwrap_or_else(|| state.settings().default_model);

    let mut req = ChatRequest::new(
        model,
        vec![json!({ "role": "system", "content": PROMPT }), json!({ "role": "user", "content": input })],
    );
    req.temperature = Some(0.2);
    req.max_tokens = Some(2000);
    req.reasoning = Some(json!({ "effort": "low", "exclude": true }));

    let mut sink = |_: StreamEvent| {};
    let cancel = tokio_util::sync::CancellationToken::new();
    let turn = state.client().stream_chat(&key, &req, &cancel, 1, &mut sink).await?;
    let summary = if turn.content.trim().is_empty() { turn.reasoning.trim() } else { turn.content.trim() };
    if summary.is_empty() {
        return Err(AppError::msg("the model returned an empty summary; nothing was changed"));
    }

    // Prune first, then drop the summary into the freed sequence range —
    // strictly below the kept tail so ordering is stable.
    let first_kept_seq = messages[keep].seq;
    let removed = state.db.delete_messages_before(chat_id, first_kept_seq)?;
    state.db.insert_message_at(
        chat_id,
        first_kept_seq - 1,
        "user",
        &format!("{SUMMARY_MARK}\n\n{summary}"),
    )?;

    Ok(CompactOutcome { removed, kept: messages.len() - keep, summary_chars: summary.len() })
}
