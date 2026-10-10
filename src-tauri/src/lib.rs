use crate::{browser_tools::BrowserState, filesystem::WatcherState, terminal::TerminalState};
use std::{collections::HashMap, sync::Mutex};
use tauri::Manager;

mod agent;
mod agent_run;
mod atomic_file;
mod file_guard;
mod merge;
mod edit_file;
mod read_file;
mod read_symbol;
mod outline;
mod repo_map;
mod lsp;
mod project_profile;
mod chat_db;
mod chat_request;
mod api_log;
mod code_search;
mod browser_tools;
mod agent_tools;
mod command_tool;
mod system_one;
mod opencode;
mod reasoning_echo;
mod filesystem;
mod terminal;
mod typings;
mod vfs;
mod workload;
mod overlay;
mod clm;
mod git;
mod git_sync;
mod process;
mod open_url;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // First, while the process is still single-threaded.
    process::adopt_login_shell_path();
    tauri::Builder::default()
        .manage(TerminalState::default())
        .manage(lsp::LspState::default())
        .manage(BrowserState::default())
        .manage(chat_request::ChatRequests::default())
        .manage(agent_run::AgentState::default())
        .manage(vfs::VfsState::default())
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
            workload::init(&dir);
            overlay::chats::init(&dir);
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
            filesystem::path_exists,
            filesystem::dir_entry_count,
            filesystem::load_ai_settings,
            filesystem::save_ai_settings,
            chat_db::commands::chat_list_dialogs,
            chat_db::commands::chat_open_project,
            chat_db::commands::chat_answer_copy,
            chat_db::commands::chat_answer_moved,
            chat_db::commands::chat_decline_projects,
            chat_db::commands::chat_list_projects,
            chat_db::commands::chat_relink_project,
            chat_db::commands::chat_delete_project,
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
            chat_db::commands::chat_save_composer_draft,
            chat_db::commands::chat_get_composer_draft,
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
            git_sync::git_pull,
            git_sync::git_pull_config,
            git_sync::git_operation_continue,
            git_sync::git_operation_abort,
            git_sync::git_conflict_file,
            git_sync::git_conflict_resolve,
            git::git_commit,
            git::git_push,
            git::git_checkout,
            git::git_branches,
            git::git_status,
            git::git_base_content,
            git::git_revert_file,
            git::git_create_branch,
            git::git_delete_branch,
            git::git_stash_push,
            git::git_stash_list,
            git::git_stash_pop,
            git::git_stash_drop,
            git::git_stash_show,
            git::git_stash_file_content,
            git::git_stash_file_preview,
            git::git_discard_all,
            git::git_log,
            git::git_commit_show,
            git::git_commit_file_preview,
            typings::nodejs::load_nodejs_project_typings,
            typings::sources::load_project_sources,
            typings::sources::list_project_files,
            lsp::lsp_start,
            lsp::lsp_send,
            lsp::lsp_stop,
            lsp::rust_analyzer::ra_find,
            lsp::rust_analyzer::ra_latest_release,
            lsp::rust_analyzer::ra_install,
            lsp::rust_analyzer::ra_rust_src_status,
            lsp::rust_analyzer::ra_install_rust_src,
            agent::send_chat_message,
            chat_request::cancel_chat_request,
            agent_run::agent_configure,
            agent_run::agent_start,
            agent_run::agent_stop,
            agent_run::agent_set_effort,
            agent_run::agent_resolve_approval,
            agent_run::agent_answer_questions,
            agent_run::agent_status,
            agent_run::chat_checklist,
            agent_run::chat_dismiss_checklist,
            agent::list_models,
            browser_tools::web_search,
            browser_tools::browse_url,
            browser_tools::browser_screenshot,
            agent_tools::tool_list_dir,
            agent_tools::tool_search_code,
            code_search::search_in_files,
            agent_tools::tool_read_file_range,
            agent_tools::tool_write_file,
            agent_tools::tool_edit_file,
            agent_tools::report_editor_diagnostics,
            project_profile::project_profile,
            project_profile::load_project_settings,
            project_profile::save_project_settings,
            agent::chat_context_estimate,
            command_tool::tool_run_command,
            workload::workload_focus,
            overlay::chats::overlay_status,
            overlay::chats::overlay_file,
            overlay::chats::overlay_discard,
            overlay::chats::overlay_land,
            overlay::chats::overlay_undo,
            workload::workload_set_slots,
            outline::outline_symbols,
            vfs::vfs_status,
            vfs::vfs_install_driver,
            vfs::vfs_mount,
            vfs::vfs_unmount,
            vfs::vfs_open_settings,
            vfs::vfs_write_file,
            vfs::vfs_read_file,
            vfs::vfs_list,
            vfs::vfs_remove,
            vfs::vfs_revisions,
            vfs::vfs_read_revision,
            vfs::vfs_restore_revision,
            vfs::vfs_set_history_limits,
            vfs::vfs_subscribe,
            vfs::vfs_unsubscribe,
            clm::clm_status,
            clm::clm_set_mode,
            clm::clm_reset,
            clm::clm_list_revisions,
            clm::clm_read_revision,
            clm::clm_restore_revision,
            clm::clm_tool_read,
            clm::clm_tool_edit,
            clm::clm_tool_write,
            clm::clm_recall_hint,
            open_url::open_url,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Don't leave a headless Chrome running after the IDE closes.
            if let tauri::RunEvent::Exit = event {
                let browser = app.state::<BrowserState>().inner().clone();
                tauri::async_runtime::block_on(browser.shutdown());
                // Unmount the virtual files, or the folder stays a dead mount.
                let _ = app.state::<vfs::VfsState>().unmount();
                overlay::chats::close_all();
                if let Some(db) = app.try_state::<chat_db::DbState>() {
                    db.mark_clean_exit();
                }
            }
        });
}
