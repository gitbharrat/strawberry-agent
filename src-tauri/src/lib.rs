//! Strawberry Agent — application entry point and wiring.

pub mod agent;
pub mod analysis;
mod commands;
pub mod db;
pub mod error;
mod files;
mod journal;
pub mod meg;
mod openrouter;
pub mod remote;
mod secrets;
mod settings;
mod shells;
pub mod smoke;
pub(crate) mod ssh;
pub mod state;
mod terminal;
mod tools;
mod workspace;

use state::AppState;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "strawberry_agent_lib=info,warn".into()),
        )
        .with_target(false)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let db = db::Db::open(&db::data_dir())?;
            let state = Arc::new(AppState::new(db)?);
            app.manage(state);

            // The window is created hidden and revealed once the webview has
            // painted, so the user never sees a white flash or an unstyled frame.
            if let Some(w) = app.get_webview_window("main") {
                let w2 = w.clone();
                tauri::async_runtime::spawn(async move {
                    // A short grace period is enough for the first paint; the
                    // frontend also calls `show` explicitly when it is ready.
                    tokio::time::sleep(std::time::Duration::from_millis(220)).await;
                    let _ = w2.show();
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Tear down PTYs and SSH channels deterministically instead of
                // leaving orphaned processes behind.
                if let Some(state) = window.try_state::<Arc<AppState>>() {
                    state.runs.cancel_all();
                    state.terminals.close_all();
                    state.shells.kill_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_settings,
            commands::set_settings,
            commands::api_key_status,
            commands::set_api_key,
            commands::clear_api_key,
            commands::verify_api_key,
            commands::get_credits,
            commands::list_models,
            commands::list_projects,
            commands::create_project,
            commands::update_project,
            commands::delete_project,
            commands::list_chats,
            commands::create_chat,
            commands::get_chat,
            commands::update_chat,
            commands::delete_chat,
            commands::list_messages,
            commands::list_tool_calls,
            commands::delete_message,
            commands::update_message,
            commands::search_messages,
            commands::attach_files,
            commands::attach_bytes,
            commands::list_attachments,
            commands::send_message,
            commands::retry_from,
            commands::run_subscribe,
            commands::run_steer,
            commands::cancel_run,
            commands::approve_tool,
            commands::answer_question,
            commands::get_plan,
            commands::compact_chat,
            commands::context_breakdown,
            commands::set_remote_workspace,
            commands::remote_complete_dir,
            commands::bg_shells,
            commands::bg_shell_kill,
            commands::active_runs,
            commands::running_chat_ids,
            commands::term_open_local,
            commands::term_open_ssh,
            commands::term_write,
            commands::term_resize,
            commands::term_close,
            commands::term_list,
            commands::list_ssh_hosts,
            commands::ssh_config_hosts,
            commands::save_ssh_host,
            commands::delete_ssh_host,
            commands::test_ssh_host,
            commands::forget_host_key,
            commands::list_tools,
            commands::workspace_status,
            commands::git_init,
            commands::fs_home,
            commands::fs_list,
            commands::fs_read,
            commands::fs_write,
            commands::fs_delete,
            commands::open_devtools,
            commands::analysis_get,
            commands::analysis_scan,
            commands::analysis_cancel,
            commands::graph_list,
            commands::graph_save,
            commands::graph_delete,
            commands::graph_set_artifact,
            commands::graph_bind,
            commands::graph_binding,
            commands::graph_clear_cache,
            commands::graph_run,
            commands::graph_refine,
            commands::graph_plan,
            commands::chat_changes,
            commands::list_artifacts,
            commands::list_file_changes,
            commands::change_diff,
            commands::revert_change,
            commands::revert_run,
            commands::revert_all_changes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Strawberry Agent");
}
