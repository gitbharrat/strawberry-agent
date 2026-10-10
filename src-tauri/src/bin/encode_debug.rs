//! Prints the encoded history for a chat, exactly as a request would carry it,
//! plus a pairing audit. Debugging aid for provider 400s.

fn main() {
    let dir = std::env::args().nth(1).expect("usage: encode_debug <data-dir>");
    let db = strawberry_agent_lib::db::Db::open(std::path::Path::new(&dir)).expect("open db");
    let chats = db.list_chats(None).expect("chats");
    for chat in chats {
        let messages = db.list_messages(&chat.id).expect("messages");
        println!("=== chat {} ({} rows) ===", chat.title, messages.len());
        for m in &messages {
            println!(
                "  row seq={} role={} status={} call_id={:?} tool_calls={}",
                m.seq,
                m.role,
                m.status,
                m.tool_call_id,
                m.tool_calls.as_deref().map(|t| &t[..t.len().min(80)]).unwrap_or("-")
            );
        }
        let encoded =
            strawberry_agent_lib::agent::prompt::encode_history(&db, &messages, 200, true).expect("encode");

        let mut declared = std::collections::HashSet::new();
        for v in &encoded {
            let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            match role {
                "assistant" => {
                    let ids: Vec<String> = v
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|c| c.get("id").and_then(|i| i.as_str()))
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    for id in &ids {
                        declared.insert(id.clone());
                    }
                    println!("  out assistant calls={ids:?}");
                }
                "tool" => {
                    let id = v.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or("");
                    let ok = declared.contains(id);
                    println!("  out tool id={id} declared_before={ok}");
                    if !ok {
                        println!("  !!! ORPHAN {id}");
                    }
                }
                other => println!("  out {other}"),
            }
        }
    }
}
