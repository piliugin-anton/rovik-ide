use crate::{filesystem::WatcherState, terminal::TerminalState};
use std::{collections::HashMap, sync::Mutex};

mod agent;
mod agent_tools;
mod filesystem;
mod terminal;
mod typings;
mod git;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(TerminalState::default())
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
            filesystem::load_chat_history,
            filesystem::save_chat_history,
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
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}