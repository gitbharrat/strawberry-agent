//! Headless end-to-end harness.
//!
//! Drives the real agent loop — same runner, same tools, same journal the UI
//! uses — against a scratch directory, and prints the event stream as it
//! arrives. It exists so the whole path can be exercised without clicking
//! through the window, and so a broken tool loop fails a command rather than a
//! demo.
//!
//! Run with: `cargo run --bin smoke -- "<prompt>" [workdir]`

use crate::agent::{runner, AgentEvent};
use crate::db::Db;
use crate::error::Result;
use crate::state::AppState;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run(prompt: &str, workdir: PathBuf, model: Option<String>) -> Result<()> {
    std::fs::create_dir_all(&workdir)?;
    println!("workdir : {}", workdir.display());

    // A scratch database so a smoke run never touches real chat history.
    let db = Db::open(&crate::db::data_dir().join("smoke"))?;
    let state = Arc::new(AppState::new(db)?);

    let mut settings = state.settings();
    if let Some(m) = model {
        settings.default_model = m;
    }
    // The point of the harness is to watch tools run, so nothing may block on a
    // click that will never come.
    settings.permission_mode = "auto".into();
    settings.max_steps = 12;
    state.apply_settings(settings.clone())?;

    println!("model   : {}", settings.default_model);
    println!("key     : {}", if state.has_api_key() { "present" } else { "MISSING" });
    if !state.has_api_key() {
        return Err(crate::error::AppError::MissingApiKey);
    }

    let chat = state.db.create_chat(
        None,
        "Smoke test",
        Some(&settings.default_model),
        Some(&workdir.display().to_string()),
    )?;
    state.db.insert_message(&crate::db::NewMessage {
        chat_id: chat.id.clone(),
        role: "user".into(),
        content: prompt.to_string(),
        status: "complete".into(),
        ..Default::default()
    })?;

    let run_id = crate::db::new_id();
    state.db.create_run(&run_id, &chat.id, &settings.default_model)?;
    let handle = state.runs.start(&run_id, &chat.id);

    let mut rx = handle.tx.subscribe();
    let printer = tokio::spawn(async move {
        let mut chars = 0usize;
        while let Ok(ev) = rx.recv().await {
            match ev {
                AgentEvent::Delta { text, .. } => {
                    chars += text.len();
                    print!("{text}");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                AgentEvent::StepStarted { step } => println!("\n\n── step {step} ──"),
                AgentEvent::ToolStarted { name, args, .. } => {
                    println!("\n  → {name} {}", compact(&args.to_string()));
                }
                AgentEvent::ToolFinished { name, ok, summary, duration_ms, .. } => {
                    println!(
                        "  {} {name} ({duration_ms}ms) {}",
                        if ok { "✓" } else { "✗" },
                        compact(&summary)
                    );
                }
                AgentEvent::Usage { prompt_tokens, completion_tokens, cost } => {
                    println!("\n  usage: {prompt_tokens} in / {completion_tokens} out / ${cost:.5}");
                }
                AgentEvent::Retry { attempt, delay_ms, reason } => {
                    println!("\n  retry {attempt} in {delay_ms}ms: {reason}");
                }
                AgentEvent::Notice { level, text } => println!("\n  [{level}] {text}"),
                AgentEvent::RunFinished { status, error, .. } => {
                    println!("\n\n── {status} ── ({chars} chars streamed)");
                    if let Some(e) = error {
                        println!("error: {e}");
                    }
                    break;
                }
                _ => {}
            }
        }
    });

    runner::execute(runner::RunContext { state: state.clone(), handle, run_id, chat_id: chat.id.clone() })
        .await;

    let _ = printer.await;

    let changes = state.db.chat_file_changes_summary(&chat.id)?;
    println!("\nfiles touched: {}", if changes.is_empty() { "none".into() } else { changes.join(", ") });
    Ok(())
}

fn compact(s: &str) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() > 110 {
        format!("{}…", one.chars().take(110).collect::<String>())
    } else {
        one
    }
}
