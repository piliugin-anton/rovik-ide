use crate::{browser_tools::BrowserState, filesystem::WatcherState, terminal::TerminalState};
use std::{collections::HashMap, sync::Mutex};
use tauri::Manager;

mod agent;
mod atomic_file;
mod chat_store;
mod browser_tools;
mod agent_tools;
mod filesystem;
mod terminal;
mod typings;
mod git;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(TerminalState::default())
        .manage(BrowserState::default())
        .manage(WatcherState {
            active_watcher: Mutex::new(None),
            gitignore: Mutex::new(None),
            id_to_path: Mutex::new(HashMap::new()),
        }) // Register our state manager
        .plugin(tauri_plugin_dialog::init())
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
            chat_store::load_chat_history,
            chat_store::save_chat_dialog,
            chat_store::delete_chat_dialog,
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
            agent::list_models,
            browser_tools::web_search,
            browser_tools::browse_url,
            browser_tools::browser_screenshot,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Don't leave a headless Chrome running after the IDE closes.
            if let tauri::RunEvent::Exit = event {
                let browser = app.state::<BrowserState>().inner().clone();
                tauri::async_runtime::block_on(browser.shutdown());
            }
        });
}
