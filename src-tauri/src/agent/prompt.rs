//! System prompt assembly and conversation encoding.

use crate::db::{Attachment, Chat, Db, Message, Project};
use crate::error::Result;
use crate::settings::Settings;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const BASE_PROMPT: &str = r#"You are Strawberry Agent, a coding assistant running as a native desktop application on the user's own machine.

You have real tools. Use them instead of guessing:
- Read files before you edit them. `edit_file` requires an exact match, so a stale assumption will simply fail.
- Prefer `grep` and `glob` over reading files one by one to find something.
- Prefer `edit_file` over `write_file` for changes to a file that already exists.
- Run commands to verify your work when a check is cheap and meaningful.

How to communicate:
- Be direct. Skip preambles like "Great question" and closing summaries that restate what the user can already see.
- When you change code, say what changed and why, not how the tool call worked.
- Reference files as `path/to/file.rs:42` so the user can click through.
- If something is ambiguous in a way that changes the work, ask. Otherwise pick the sensible default and say which one you picked.
- Report failures honestly, with the actual error output."#;

/// Builds the system message: base behaviour, then project and chat overrides,
/// then a short environment block so the model does not have to ask what OS or
/// directory it is in.
pub fn system_prompt(settings: &Settings, project: Option<&Project>, chat: &Chat, tools_on: bool) -> String {
    use crate::settings::PermissionMode;
    let mut s = String::with_capacity(2048);
    s.push_str(BASE_PROMPT);

    if !tools_on {
        s.push_str("\n\nTools are currently disabled for this chat. Answer from the conversation alone and say so if you would need to inspect files.");
    } else {
        s.push_str(match settings.mode() {
            PermissionMode::ReadOnly => "\n\nThis chat is READ-ONLY. You can read, search and list files, but writes, commands and network requests will be refused. Investigate thoroughly and propose a concrete plan instead of trying to apply it.",
            PermissionMode::Ask => "\n\nEvery change needs the user's approval, so batch related edits and explain what you are about to do before you do it.",
            PermissionMode::AcceptEdits => "\n\nFile edits apply immediately; commands and network requests need the user's approval. Every edit is journalled and the user can revert it.",
            PermissionMode::Auto => "\n\nYou may edit files and run commands without asking — nothing will prompt automatically. The judgement is yours instead: before a genuinely destructive or hard-to-reverse step (deleting data or branches, force-pushing, dropping tables, mass rewrites, publishing or sending anything), pause and confirm with the ask_user tool. Everything else: just do it, prefer reversible steps, and verify before you move on.",
        });
        if settings.sandbox_files {
            s.push_str("\nFile access is confined to this chat's working directory.");
        }
    }

    for extra in [
        settings.system_prompt.as_deref(),
        project.and_then(|p| p.system_prompt.as_deref()),
        chat.system_prompt.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let extra = extra.trim();
        if !extra.is_empty() {
            s.push_str("\n\n");
            s.push_str(extra);
        }
    }

    // The living architecture map, when this workspace has one. It is the
    // model's mental index of the codebase — always injected so the agent
    // starts oriented instead of re-discovering the layout every chat.
    if let Some(cwd) = chat.cwd.as_deref().or(project.and_then(|p| p.root_path.as_deref())) {
        if let Some(map) = crate::analysis::prompt_context(std::path::Path::new(cwd)) {
            s.push_str("\n\n<architecture>\n");
            s.push_str(&map);
            s.push_str("\n</architecture>\nThe map above is auto-generated (see .strawberry/architecture.md) and refreshes after your edits. Trust it for orientation; verify specifics with tools before depending on them.");
        }
    }

    s.push_str("\n\n<environment>\n");
    s.push_str(&format!("platform: {}\n", std::env::consts::OS));
    s.push_str(&format!("arch: {}\n", std::env::consts::ARCH));
    if let Some(cwd) = chat.cwd.as_deref().or(project.and_then(|p| p.root_path.as_deref())) {
        match crate::remote::parse(cwd) {
            Some(t) => s.push_str(&format!(
                "working directory: {} — a REMOTE workspace over SSH. read_file/write_file/edit_file/list_dir/glob/grep/run_command and docker_* all operate on that machine; shell_*/tmux_* run locally.\n",
                t.root
            )),
            None => s.push_str(&format!("working directory: {cwd}\n")),
        }
    }
    if let Some(p) = project {
        s.push_str(&format!("project: {}\n", p.name));
    }
    s.push_str(&format!(
        "tmux: {}\n",
        if crate::tools::tmux_tools::available() {
            "available (tmux_* tools; sessions survive app restarts)"
        } else {
            "not installed"
        }
    ));
    s.push_str(&format!(
        "docker: {}\n",
        if crate::tools::docker::available() {
            "available (docker_ps / docker_logs / docker_exec / docker_inspect)"
        } else {
            "not detected on PATH"
        }
    ));
    s.push_str(&format!("date: {}\n", chrono::Utc::now().format("%Y-%m-%d")));
    s.push_str("</environment>");
    s
}

/// Encodes stored messages into the OpenAI wire format OpenRouter expects.
///
/// Correctness here is structural, not cosmetic. Providers reject a history
/// where a `tool` message references a `tool_call_id` that no preceding
/// assistant turn declared, or (for some providers) where an assistant turn
/// declares a call that never got a result. Both happen naturally: a run
/// cancelled between the model asking for a tool and the tool finishing leaves
/// exactly that shape in the database.
///
/// So this does three things beyond formatting:
///
///  * drops a failed assistant turn **together with** the tool results that
///    belong to it, rather than orphaning them;
///  * drops any `tool` message whose id was never declared;
///  * synthesises a result for a declared call that has none, so every pair is
///    closed.
///
/// Only the tail of the conversation is sent, and the window starts at a
/// boundary that keeps an assistant turn with the tool results it depends on.
pub fn encode_history(
    db: &Db,
    messages: &[Message],
    limit: usize,
    supports_images: bool,
) -> Result<Vec<Value>> {
    let start = trim_start(messages, limit);
    let window = &messages[start..];

    // Which call ids actually produced a result, and which turns are unusable.
    let mut results_by_id: HashMap<&str, &Message> = HashMap::new();
    for m in window {
        if m.role == "tool" {
            if let Some(id) = m.tool_call_id.as_deref() {
                results_by_id.insert(id, m);
            }
        }
    }

    // Tool results that asked to be SEEN (view_image, web_screenshot): the
    // freshest few get attached as real image parts right after their turn.
    // Only the recent tail qualifies — older images decay to path references,
    // so a long chat does not drag megabytes of base64 into every request.
    let vision = if supports_images { vision_results(window) } else { HashSet::new() };

    let mut out = Vec::with_capacity(window.len() + 1);
    // Call ids belonging to assistant turns we dropped; their results go too.
    let mut discarded: HashSet<String> = HashSet::new();
    // Call ids we emitted and still owe a result for.
    let mut emitted: HashSet<String> = HashSet::new();

    for m in window {
        match m.role.as_str() {
            "user" => {
                let attachments = db.attachments_for_message(&m.id)?;
                out.push(encode_user(m, &attachments, supports_images));
            }

            "assistant" => {
                let calls = m
                    .tool_calls
                    .as_deref()
                    .filter(|s| !s.is_empty() && *s != "[]")
                    .and_then(|tc| serde_json::from_str::<Value>(tc).ok());
                let call_ids = call_ids_of(calls.as_ref());

                // A turn that failed carries no information the model can use,
                // and its declared calls may never have run.
                if m.status == "error" || m.status == "interrupted" {
                    discarded.extend(call_ids);
                    continue;
                }

                let mut v = json!({ "role": "assistant" });
                if !m.content.is_empty() {
                    v["content"] = json!(m.content);
                }
                if let Some(mut parsed) = calls {
                    // Rows written before the Default fix carry "type":"";
                    // repair them on the way out so old chats stop tripping
                    // providers.
                    if let Some(arr) = parsed.as_array_mut() {
                        for call in arr {
                            let bad = call.get("type").and_then(Value::as_str).is_none_or(str::is_empty);
                            if bad {
                                call["type"] = json!("function");
                            }
                        }
                    }
                    v["tool_calls"] = parsed;
                }
                if v.get("content").is_none() && v.get("tool_calls").is_none() {
                    discarded.extend(call_ids);
                    continue;
                }

                out.push(v);

                // Close every call this turn declared, in the order declared.
                let mut images: Vec<String> = Vec::new();
                for id in call_ids {
                    match results_by_id.get(id.as_str()) {
                        Some(result) => {
                            out.push(tool_message(&id, &result.content));
                            if vision.contains(result.id.as_str()) {
                                if let Some(p) = crate::tools::inline_image(&result.content) {
                                    images.push(p);
                                }
                            }
                        }
                        None => out.push(tool_message(
                            &id,
                            "This tool call did not complete — the run was interrupted before it \
                             produced a result. Call it again if you still need it.",
                        )),
                    }
                    emitted.insert(id);
                }
                // After the turn's results are closed, hand over the pixels.
                if !images.is_empty() {
                    let mut parts = vec![json!({
                        "type": "text",
                        "text": "The image(s) from the tool results above, attached for visual inspection:"
                    })];
                    for p in &images {
                        if let Some(url) = image_part_url(p) {
                            parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                        }
                    }
                    if parts.len() > 1 {
                        out.push(json!({ "role": "user", "content": parts }));
                    }
                }
            }

            // Results are emitted alongside their assistant turn above. Anything
            // reaching here is either a duplicate or an orphan.
            "tool" => {}

            _ => {}
        }
    }

    let _ = (&discarded, &emitted);
    Ok(out)
}

fn tool_message(call_id: &str, content: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": call_id, "content": content })
}

/// How far back a viewed image stays attached as pixels, and how many at once.
const VISION_TAIL: usize = 10;
const MAX_VISION_IMAGES: usize = 3;
const MAX_VISION_BYTES: u64 = 6 * 1024 * 1024;

/// Ids of the tool-result messages whose images should ride along as parts:
/// the newest few markers inside the recent tail.
fn vision_results(window: &[Message]) -> HashSet<&str> {
    let cutoff = window.len().saturating_sub(VISION_TAIL);
    window[cutoff..]
        .iter()
        .filter(|m| m.role == "tool" && crate::tools::inline_image(&m.content).is_some())
        .rev()
        .take(MAX_VISION_IMAGES)
        .map(|m| m.id.as_str())
        .collect()
}

fn image_part_url(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_VISION_BYTES {
        return None;
    }
    data_url(path, crate::tools::image_mime(path))
}

/// Ids declared by an assistant turn's `tool_calls`, in order.
fn call_ids_of(calls: Option<&Value>) -> Vec<String> {
    calls
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Finds a safe index to start the window at: at or before `limit` messages
/// from the end, walking backwards past any tool messages and the assistant
/// turn that issued them.
fn trim_start(messages: &[Message], limit: usize) -> usize {
    if messages.len() <= limit {
        return 0;
    }
    let mut i = messages.len() - limit;
    while i > 0 && matches!(messages[i].role.as_str(), "tool") {
        i -= 1;
    }
    if i > 0 && messages[i].role == "assistant" && messages[i].tool_calls.is_some() {
        i -= 1;
    }
    i
}

fn encode_user(m: &Message, attachments: &[Attachment], supports_images: bool) -> Value {
    if attachments.is_empty() {
        return json!({ "role": "user", "content": m.content });
    }

    let mut parts: Vec<Value> = Vec::with_capacity(attachments.len() + 1);
    if !m.content.is_empty() {
        parts.push(json!({ "type": "text", "text": m.content }));
    }

    for a in attachments {
        match a.kind.as_str() {
            "image" if supports_images => match data_url(&a.path, &a.mime) {
                Some(url) => parts.push(json!({ "type": "image_url", "image_url": { "url": url } })),
                None => parts
                    .push(json!({ "type": "text", "text": format!("[could not read image {}]", a.name) })),
            },
            "image" => parts.push(json!({
                "type": "text",
                "text": format!("[image {} was attached, but the selected model cannot read images]", a.name),
            })),
            "pdf" => match data_url(&a.path, &a.mime) {
                Some(url) => parts.push(json!({
                    "type": "file",
                    "file": { "filename": a.name, "file_data": url }
                })),
                None => parts.push(json!({ "type": "text", "text": format!("[could not read {}]", a.name) })),
            },
            _ => {
                // Text-ish files are inlined so every model can use them,
                // regardless of file-input support.
                let body = std::fs::read_to_string(&a.path).unwrap_or_else(|e| format!("[unreadable: {e}]"));
                let clipped: String = body.chars().take(120_000).collect();
                parts.push(json!({
                    "type": "text",
                    "text": format!("<attachment name=\"{}\">\n{clipped}\n</attachment>", a.name),
                }));
            }
        }
    }

    json!({ "role": "user", "content": parts })
}

pub(crate) fn data_url(path: &str, mime: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

/// Prompt used to name a chat from its opening exchange.
pub fn title_prompt(user: &str, assistant: &str) -> Vec<Value> {
    let excerpt: String = user.chars().take(1500).collect();
    let reply: String = assistant.chars().take(600).collect();
    vec![
        json!({
            "role": "system",
            "content": "Write a title for this conversation: 2 to 5 words, sentence case, no quotes, no trailing punctuation. Name the specific subject, not the activity. Reply with the title alone."
        }),
        json!({ "role": "user", "content": format!("User: {excerpt}\n\nAssistant: {reply}") }),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            id: crate::db::new_id(),
            chat_id: "c".into(),
            seq: 0,
            role: role.into(),
            content: content.into(),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            model: None,
            status: "complete".into(),
            error: None,
            usage: None,
            run_id: None,
            created_at: 0,
        }
    }

    fn assistant_with_call(call_id: &str, status: &str) -> Message {
        let mut m = msg("assistant", "");
        m.status = status.into();
        m.tool_calls = Some(
            serde_json::json!([{
                "id": call_id,
                "type": "function",
                "function": { "name": "run_command", "arguments": "{}" }
            }])
            .to_string(),
        );
        m
    }

    fn tool_result(call_id: &str, content: &str) -> Message {
        let mut m = msg("tool", content);
        m.tool_call_id = Some(call_id.into());
        m
    }

    fn ids_in(history: &[Value]) -> Vec<(String, Option<String>)> {
        history
            .iter()
            .map(|v| {
                (
                    v["role"].as_str().unwrap().to_string(),
                    v.get("tool_call_id").and_then(Value::as_str).map(str::to_owned),
                )
            })
            .collect()
    }

    /// The provider-rejection case from the field: a run cancelled between the
    /// model declaring a call and the tool finishing must still produce a
    /// closed pair, and a result whose declaring turn was dropped must vanish
    /// with it.
    #[test]
    fn history_never_contains_orphaned_tool_ids() {
        let db = crate::db::Db::open(
            std::env::temp_dir().join(format!("sa-test-{}", crate::db::new_id())).as_path(),
        )
        .unwrap();

        let messages = vec![
            msg("user", "do things"),
            // Declared a call, then the run died: no result row exists.
            assistant_with_call("call_unfinished", "complete"),
            // A failed turn whose result row DOES exist — both must be dropped.
            assistant_with_call("call_failed", "error"),
            tool_result("call_failed", "output of failed turn"),
            msg("user", "continue"),
        ];

        let history = encode_history(&db, &messages, 100, false).unwrap();
        let rows = ids_in(&history);

        // Every tool row must directly reference a call declared by a
        // preceding assistant row.
        let mut declared: Vec<String> = Vec::new();
        for v in &history {
            if v["role"] == "assistant" {
                if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                    declared.extend(calls.iter().filter_map(|c| c["id"].as_str().map(str::to_owned)));
                }
            }
            if v["role"] == "tool" {
                let id = v["tool_call_id"].as_str().unwrap();
                assert!(declared.contains(&id.to_string()), "orphaned tool id {id} in {rows:?}");
            }
        }

        // The unfinished call got a synthesised result…
        assert!(rows.iter().any(|(r, id)| r == "tool" && id.as_deref() == Some("call_unfinished")));
        // …and the failed turn's result went away with its turn.
        assert!(!rows.iter().any(|(_, id)| id.as_deref() == Some("call_failed")));
    }
}
