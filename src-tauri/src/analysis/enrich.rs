//! Model enrichment: the AI layer of the architecture map.
//!
//! The scanner finds *where* everything is; the selected model explains *what*
//! it is — a description, inputs and outputs, a role tag, and a one-line note
//! for math kernels and routing hubs. Token discipline throughout:
//!
//!  * only new or changed functions are sent (body-hash gated), so a re-scan
//!    after one edit costs one small request, not a repo's worth;
//!  * bodies are clipped and batched, and the reply is compact JSON;
//!  * reasoning is disabled — this is annotation, not problem solving.

use super::{ArchDoc, ArchFn};
use crate::error::Result;
use crate::openrouter::{ChatRequest, Client, StreamEvent};
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Per-run ceiling. At ~22 functions a request this is at most ~28 small
/// requests; anything beyond stays static until the next scan.
const MAX_FNS_PER_RUN: usize = 600;
const MAX_FNS_PER_BATCH: usize = 16;
const MAX_BATCH_CHARS: usize = 6000;
const MAX_BODY_LINES: usize = 30;
const MAX_BODY_CHARS: usize = 1100;
const CONCURRENCY: usize = 4;

const SYSTEM: &str = "You annotate code for an architecture map. Reply with ONLY a JSON array — no prose, no markdown fences. For each function item return {\"k\":key,\"d\":what it does (max 10 words),\"i\":inputs (max 6 words),\"o\":output or effect (max 6 words),\"t\":one of core|math|routing|io|state|ui|util,\"n\":only when t is math or routing — the equation or dispatch logic in max 12 words,\"tc\":big-O time complexity over the dominant input like O(n) or O(n log n),\"sc\":big-O extra space like O(1)}. For each file item return {\"k\":key,\"d\":the file's role (max 8 words),\"r\":one of entry|orchestrator|ui|state|model|util|io|config|test}. Be concrete and terse; no filler words.";

pub struct EnrichStats {
    pub updated: usize,
    pub requests: u32,
    pub failed: u32,
    pub cost: f64,
    /// First failure's message, so the UI can say *why* instead of a bare count.
    pub last_error: Option<String>,
}

/// Everything a batch needs to reach the model.
pub struct EnrichCtx<'a> {
    pub client: &'a Client,
    pub key: &'a str,
    pub model: &'a str,
    pub root: &'a Path,
    /// Sources already in memory, keyed by workspace-relative path — the
    /// remote-workspace case, where `root` points at nothing local. When set,
    /// bodies come from here instead of disk.
    pub texts: Option<&'a HashMap<String, String>>,
    /// Folder scope: when set, only files under this workspace-relative
    /// prefix are annotated — the pane's "analyse this folder" focus.
    pub scope: Option<&'a str>,
    pub cancel: &'a CancellationToken,
}

/// One unit of work for the model, with body text already clipped.
struct Item {
    key: String,
    header: String,
    body: String,
}

/// Annotates stale functions and files in place. `progress(done, total)` fires
/// per completed batch.
pub async fn enrich(
    ctx: &EnrichCtx<'_>,
    doc: &mut ArchDoc,
    max_fns: usize,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<EnrichStats> {
    let items = collect_items(ctx, doc, max_fns.min(MAX_FNS_PER_RUN));
    let mut stats = EnrichStats { updated: 0, requests: 0, failed: 0, cost: 0.0, last_error: None };
    if items.is_empty() {
        return Ok(stats);
    }

    let batches = pack(items);
    let total = batches.len();
    let mut answers: HashMap<String, Value> = HashMap::new();

    // A bounded window of in-flight requests: enough to hide latency, few
    // enough to stay polite on rate limits.
    let mut waves = batches.into_iter().collect::<Vec<_>>();
    let mut done = 0usize;
    while !waves.is_empty() {
        if ctx.cancel.is_cancelled() {
            break;
        }
        let wave: Vec<Vec<Item>> = waves.drain(..waves.len().min(CONCURRENCY)).collect();
        let mut futs = FuturesUnordered::new();
        for batch in wave {
            futs.push(run_batch(ctx, batch));
        }
        while let Some(res) = futs.next().await {
            done += 1;
            stats.requests += 1;
            match res {
                Ok((map, cost)) => {
                    stats.cost += cost;
                    answers.extend(map);
                }
                Err(e) => {
                    tracing::warn!("annotation batch failed with {}: {e}", ctx.model);
                    stats.failed += 1;
                    stats.last_error.get_or_insert_with(|| e.to_string());
                }
            }
            progress(done, total);
        }
    }

    stats.updated = apply(doc, &answers);
    Ok(stats)
}

/// Gathers stale work, most-connected files first so a capped run annotates
/// the parts of the codebase people actually look at.
fn collect_items(ctx: &EnrichCtx<'_>, doc: &ArchDoc, cap: usize) -> Vec<Item> {
    let mut order: Vec<usize> = (0..doc.files.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(doc.files[i].in_degree + doc.files[i].functions.len() as u32));

    let mut items = Vec::new();
    for i in order {
        if items.len() >= cap {
            break;
        }
        let file = &doc.files[i];
        if let Some(scope) = ctx.scope {
            if file.id != scope && !file.id.starts_with(&format!("{scope}/")) {
                continue;
            }
        }
        // A file needs the model when its sentence or its architectural kind
        // is missing — older maps have roles but predate kinds.
        let needs_role = file.role.is_none() || file.kind.is_none();
        let stale: Vec<&ArchFn> =
            file.functions.iter().filter(|f| f.stale || f.desc.is_none() || f.big_o.is_none()).collect();
        if !needs_role && stale.is_empty() {
            continue;
        }

        // One read per file serves every function in it.
        let text: String = match ctx.texts {
            Some(m) => match m.get(&file.id) {
                Some(t) => t.clone(),
                None => continue,
            },
            None => match std::fs::read_to_string(ctx.root.join(&file.id)) {
                Ok(t) => t,
                Err(_) => continue,
            },
        };
        let lines: Vec<&str> = text.lines().collect();

        if needs_role {
            let head: String = lines.iter().take(20).copied().collect::<Vec<_>>().join("\n");
            items.push(Item {
                key: format!("file:{}", file.id),
                header: format!(
                    "file {} ({} fns, {} lines) — describe its role",
                    file.id,
                    file.functions.len(),
                    file.lines
                ),
                body: clip(&head, 24, 700),
            });
        }

        for f in stale {
            if items.len() >= cap {
                break;
            }
            let start = (f.line as usize).saturating_sub(1).min(lines.len());
            let end = (f.end_line as usize).min(lines.len());
            let body = lines[start..end].join("\n");
            let hint = f.hint.as_deref().map(|h| format!(" hint:{h}")).unwrap_or_default();
            items.push(Item {
                key: format!("{}::{}", file.id, f.name),
                header: format!("fn {}::{}({}){hint}", file.id, f.name, f.params),
                body: clip(&body, MAX_BODY_LINES, MAX_BODY_CHARS),
            });
        }
    }
    items
}

/// Clips a body for the prompt: drop blank lines, keep the head and tail so
/// both the setup and the return are visible.
fn clip(body: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let text = if lines.len() <= max_lines {
        lines.join("\n")
    } else {
        let head = &lines[..max_lines - 6];
        let tail = &lines[lines.len() - 5..];
        format!("{}\n…\n{}", head.join("\n"), tail.join("\n"))
    };
    if text.len() > max_chars {
        let mut cut = max_chars;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &text[..cut])
    } else {
        text
    }
}

fn pack(items: Vec<Item>) -> Vec<Vec<Item>> {
    let mut batches: Vec<Vec<Item>> = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut chars = 0usize;
    for item in items {
        let size = item.header.len() + item.body.len();
        if !cur.is_empty() && (cur.len() >= MAX_FNS_PER_BATCH || chars + size > MAX_BATCH_CHARS) {
            batches.push(std::mem::take(&mut cur));
            chars = 0;
        }
        chars += size;
        cur.push(item);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    batches
}

async fn run_batch(ctx: &EnrichCtx<'_>, batch: Vec<Item>) -> Result<(HashMap<String, Value>, f64)> {
    let mut prompt = String::with_capacity(MAX_BATCH_CHARS + 512);
    prompt.push_str("Annotate these. Return one JSON object per key, all in a single JSON array. Use each item's k value verbatim.\n");
    for item in &batch {
        prompt.push_str(&format!("\n--- k={}\n{}\n{}\n", item.key, item.header, item.body));
    }

    let mut req = ChatRequest::new(
        ctx.model,
        vec![json!({ "role": "system", "content": SYSTEM }), json!({ "role": "user", "content": prompt })],
    );
    req.temperature = Some(0.1);
    // Providers count thinking tokens against max_tokens, and models with
    // mandatory reasoning (GLM flash) will happily spend most of a tight
    // budget thinking and truncate the JSON mid-array. Low effort keeps the
    // thinking short; the head-room keeps the answer whole either way.
    req.max_tokens = Some(((batch.len() as u32) * 55 + 400) * 3);
    // Deliberately NOT `enabled: false`: mandatory-reasoning endpoints reject
    // that outright with a 400, which silently killed every annotation batch.
    // `effort` + `exclude` are accepted everywhere, and OpenRouter drops the
    // field for models that do not support it.
    req.reasoning = Some(json!({ "effort": "low", "exclude": true }));

    let mut sink = |_: StreamEvent| {};
    let turn = ctx.client.stream_chat(ctx.key, &req, ctx.cancel, 1, &mut sink).await?;
    let cost = turn.usage.as_ref().map(|u| u.cost).unwrap_or(0.0);

    let mut out = HashMap::new();
    // Some reasoning models leak the answer into the thinking channel even
    // when told not to; both channels are candidates.
    let mut items = parse_array(&turn.content);
    if items.is_empty() && !turn.reasoning.is_empty() {
        items = parse_array(&turn.reasoning);
    }
    for v in items {
        if let Some(k) = v.get("k").and_then(Value::as_str) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Ok((out, cost))
}

/// Pulls a JSON array out of a model reply that may carry stray text or a
/// code fence around it. A reply cut off mid-array (max_tokens) is salvaged
/// down to its last complete object rather than thrown away whole.
fn parse_array(content: &str) -> Vec<Value> {
    let Some(s) = content.find('[') else { return Vec::new() };
    if let Some(e) = content.rfind(']') {
        if e > s {
            if let Ok(v) = serde_json::from_str::<Vec<Value>>(&content[s..=e]) {
                return v;
            }
        }
    }
    // Truncated: close the array after the last complete object.
    let tail = &content[s..];
    if let Some(last) = tail.rfind('}') {
        let repaired = format!("{}]", &tail[..=last]);
        return serde_json::from_str::<Vec<Value>>(&repaired).unwrap_or_default();
    }
    Vec::new()
}

/// A big-O string, tightly capped — "O(n log n)" is as long as they get.
fn big_o_cap(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    if s.is_empty() || s.len() > 18 {
        return None;
    }
    Some(s.to_string())
}

fn word_cap(v: &Value, key: &str, words: usize) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    let capped: Vec<&str> = s.split_whitespace().take(words).collect();
    Some(capped.join(" "))
}

fn apply(doc: &mut ArchDoc, answers: &HashMap<String, Value>) -> usize {
    let mut updated = 0usize;
    for file in &mut doc.files {
        if let Some(v) = answers.get(&format!("file:{}", file.id)) {
            if let Some(d) = word_cap(v, "d", 10) {
                file.role = Some(d);
                updated += 1;
            }
            if let Some(r) = v.get("r").and_then(Value::as_str) {
                let r = r.trim().to_ascii_lowercase();
                if matches!(
                    r.as_str(),
                    "entry" | "orchestrator" | "ui" | "state" | "model" | "util" | "io" | "config" | "test"
                ) {
                    file.kind = Some(r);
                }
            }
        }
        for f in &mut file.functions {
            let Some(v) = answers.get(&format!("{}::{}", file.id, f.name)) else { continue };
            if let Some(d) = word_cap(v, "d", 12) {
                f.desc = Some(d);
                f.stale = false;
                updated += 1;
            }
            f.inputs = word_cap(v, "i", 8).or(f.inputs.take());
            f.outputs = word_cap(v, "o", 8).or(f.outputs.take());
            if let Some(t) = v.get("t").and_then(Value::as_str) {
                let t = t.trim().to_ascii_lowercase();
                if matches!(t.as_str(), "core" | "math" | "routing" | "io" | "state" | "ui" | "util") {
                    if t == "core" {
                        f.core = true;
                        // Keep a static hint visible as the tag if the model
                        // only said "core".
                        f.tag = Some(f.hint.clone().unwrap_or(t));
                    } else {
                        f.tag = Some(t);
                    }
                }
            }
            if matches!(f.tag.as_deref(), Some("math") | Some("routing")) {
                f.note = word_cap(v, "n", 14).or(f.note.take());
            }
            f.big_o = big_o_cap(v, "tc").or(f.big_o.take());
            f.space = big_o_cap(v, "sc").or(f.space.take());
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json() {
        let reply = "Here you go:\n```json\n[{\"k\":\"a.ts::f\",\"d\":\"adds numbers\"}]\n```";
        let arr = parse_array(reply);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["k"], "a.ts::f");
    }

    /// A reply cut off by max_tokens (thinking models eat the budget) keeps
    /// every complete object instead of losing the whole batch.
    #[test]
    fn salvages_truncated_array() {
        let reply = "[{\"k\":\"a.ts::f\",\"d\":\"first\"},{\"k\":\"a.ts::g\",\"d\":\"second\"},{\"k\":\"a.ts::h\",\"d\":\"cut of";
        let arr = parse_array(reply);
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["k"], "a.ts::g");
    }

    #[test]
    fn word_caps_hold() {
        let v = serde_json::json!({ "d": "one two three four five six seven eight nine ten eleven" });
        assert_eq!(word_cap(&v, "d", 3).as_deref(), Some("one two three"));
    }

    #[test]
    fn packing_respects_batch_limits() {
        let items: Vec<Item> = (0..50)
            .map(|i| Item { key: format!("k{i}"), header: "h".repeat(50), body: "b".repeat(500) })
            .collect();
        let batches = pack(items);
        assert!(batches.iter().all(|b| b.len() <= MAX_FNS_PER_BATCH));
        assert!(batches.len() >= 3);
    }
}
