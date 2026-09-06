use crate::filesystem::{WatcherDebouncer, WatcherState};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

mod agent;
mod agent_tools;
mod filesystem;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let debouncer = Arc::new(WatcherDebouncer::new(2500));
    tauri::Builder::default()
        .manage(WatcherState {
            active_watcher: Mutex::new(None),
            debouncer,
            gitignore: Mutex::new(None),
            id_to_path: Mutex::new(HashMap::new()),
        }) // Register our state manager
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            filesystem::start_file_watch,
            filesystem::read_dir_entries,
            filesystem::read_file_content,
            filesystem::write_file_content,
            filesystem::load_ai_settings,
            filesystem::save_ai_settings,
            filesystem::load_chat_history,
            filesystem::save_chat_history,
            filesystem::get_path_id,
            filesystem::create_file,
            filesystem::create_dir,
            filesystem::delete_path,
            filesystem::rename_path,
            filesystem::move_path,
            filesystem::copy_path,
            agent::send_chat_message,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
