use crate::{browser_tools::BrowserState, filesystem::WatcherState, terminal::TerminalState};
use std::{collections::HashMap, sync::Mutex};
use tauri::Manager;

mod agent;
mod atomic_file;
mod chat_db;
mod chat_request;
mod code_search;
mod browser_tools;
mod agent_tools;
mod command_tool;
mod system_one;
mod review_log;
mod filesystem;
mod terminal;
mod typings;
mod git;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(TerminalState::default())
        .manage(BrowserState::default())
        .manage(chat_request::ChatRequests::default())
        .manage(WatcherState {
            active_watcher: Mutex::new(None),
            gitignore: Mutex::new(None),
            id_to_path: Mutex::new(HashMap::new()),
        }) // Register our state manager
        // Must be the first plugin: a second launch focuses this window
        // instead of opening the chat database a second time.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let db = chat_db::DbState::open(&dir).map_err(|e| format!("Chat database: {e}"))?;
            app.manage(db);
            Ok(())
        })
        // Screenshots: chat-attachment://localhost/<sha256> (Windows:
        // http://chat-attachment.localhost/<sha256>).
        .register_asynchronous_uri_scheme_protocol("chat-attachment", |ctx, request, responder| {
            let db = ctx.app_handle().state::<chat_db::DbState>().inner().clone();
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(chat_db::attachments::serve(db.data_dir(), &path));
            });
        })
        .invoke_handler(tauri::generate_handler![
            terminal::spawn_terminal,
            terminal::write_to_terminal,
            terminal::resize_terminal,
            terminal::close_terminal,
            filesystem::start_file_watch,
            filesystem::read_dir_entries,
            filesystem::read_file_content,
            filesystem::write_file_content,
            filesystem::load_ai_settings,
            filesystem::save_ai_settings,
            chat_db::commands::chat_list_dialogs,
            chat_db::commands::chat_get_messages,
            chat_db::commands::chat_get_messages_around,
            chat_db::commands::chat_get_message_body,
            chat_db::commands::chat_create_dialog,
            chat_db::commands::chat_append_messages,
            chat_db::commands::chat_update_message,
            chat_db::commands::chat_rename_dialog,
            chat_db::commands::chat_delete_dialog,
            chat_db::commands::chat_search,
            chat_db::commands::chat_messages_answered,
            filesystem::load_last_project,
            filesystem::save_last_project,
            filesystem::get_path_id,
            filesystem::create_file,
            filesystem::create_dir,
            filesystem::delete_path,
            filesystem::rename_path,
            filesystem::move_path,
            filesystem::copy_path,
            git::git_init,
            git::git_fetch,
            git::git_pull,
            git::git_commit,
            git::git_push,
            git::git_checkout,
            git::git_branches,
            git::git_status,
            typings::nodejs::load_nodejs_project_typings,
            agent::send_chat_message,
            chat_request::cancel_chat_request,
            agent::list_models,
            browser_tools::web_search,
            browser_tools::browse_url,
            browser_tools::browser_screenshot,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            code_search::search_in_files,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
            command_tool::tool_run_command,
            system_one::system_one_request,
            review_log::log_ai_review_started,
            review_log::log_ai_review_finished,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Don't leave a headless Chrome running after the IDE closes.
            if let tauri::RunEvent::Exit = event {
                let browser = app.state::<BrowserState>().inner().clone();
                tauri::async_runtime::block_on(browser.shutdown());
                if let Some(db) = app.try_state::<chat_db::DbState>() {
                    db.mark_clean_exit();
                }
            }
        });
}
