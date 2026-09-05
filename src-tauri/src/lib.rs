use crate::filesystem::WatcherState;
use std::sync::Mutex;

mod agent;
mod agent_tools;
mod filesystem;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(WatcherState {
            active_watcher: Mutex::new(None),
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
            agent::send_chat_message,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
