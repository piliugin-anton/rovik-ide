mod agent_tools;
mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::read_dir_entries,
            commands::read_file_content,
            commands::write_file_content,
            commands::load_ai_settings,
            commands::save_ai_settings,
            commands::send_chat_message,
            commands::load_chat_history,
            commands::save_chat_history,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
