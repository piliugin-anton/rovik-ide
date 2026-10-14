use crate::agent_run::events::{AgentEvent, EVENT};
use crate::core::{Core, EventSink, Paths};
use crate::{filesystem::WatcherState, terminal::TerminalState};
use std::{collections::HashMap, sync::Arc, sync::Mutex};
use tauri::{Emitter, Manager};

mod agent;
mod agent_run;
mod agent_tools;
mod api_log;
mod atomic_file;
mod browser_tools;
mod chat_db;
mod chat_request;
mod clm;
mod code_search;
mod command_tool;
pub mod core;
mod edit_file;
mod file_guard;
mod filesystem;
mod formatter;
mod git;
mod git_sync;
/// The agent without a window, for measuring it (G00): `rovik-headless`.
#[cfg(any(test, feature = "headless"))]
pub mod headless;
mod lsp;
mod merge;
mod open_url;
mod opencode;
mod outline;
mod overlay;
mod process;
mod project_files;
mod prompt_cache;
mod project_profile;
mod read_file;
mod read_symbol;
mod reasoning_echo;
mod release_assets;
mod repo_map;
mod rt;
mod system_one;
mod terminal;
mod typings;
mod vfs;
mod workload;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // First, while the process is still single-threaded.
    process::adopt_login_shell_path();
    // Then the core's runtime (rt.rs), Tauri's too: one for the process.
    tauri::async_runtime::set(rt::handle().clone());
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .manage(TerminalState::default())
        .manage(lsp::LspState::default())
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
            let paths = Paths::new(app.path().app_data_dir()?, app.path().app_config_dir()?);
            let core = Core::new(paths, Arc::new(WindowEvents(app.handle().clone())))?;
            // The commands that take one of its parts get the core's.
            app.manage(core.db.clone());
            app.manage(core.agents.clone());
            app.manage(core.vfs.clone());
            app.manage(core.browser.clone());
            app.manage(core);
            // The main window, as tauri.conf.json describes it (`create`
            // is off there): built here to let its page read the clipboard,
            // which a config can't. Pasting an image into the chat input
            // needs that: WebKitGTK's paste event leaves images out (even
            // for Ctrl+V), so they're read through the clipboard API, which
            // it refuses without this; WebView2 asks for permission. The
            // input's own menu reads text that way too. macOS allows it.
            if let Some(main) = app.config().app.windows.iter().find(|w| w.label == "main") {
                tauri::WebviewWindowBuilder::from_config(app.handle(), main)?
                    .enable_clipboard_access()
                    .build()?;
            }
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
            filesystem::read_image_file,
            filesystem::write_file_content,
            filesystem::path_exists,
            filesystem::read_editorconfigs,
            formatter::formatter_for_file,
            formatter::format_file,
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
            chat_db::commands::chat_store_attachment,
            chat_db::commands::chat_save_composer_draft,
            chat_db::commands::chat_get_composer_draft,
            chat_db::commands::chat_set_model,
            chat_db::commands::chat_get_model,
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
            typings::node_modules::load_project_typings,
            typings::sources::load_project_sources,
            project_files::list_project_files,
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
            workload::workload_focus,
            overlay::chats::overlay_status,
            overlay::chats::overlay_file,
            overlay::chats::overlay_discard,
            overlay::chats::overlay_land,
            overlay::chats::overlay_undo,
            overlay::chats::overlay_conflict_file,
            overlay::chats::overlay_conflict_resolve,
            overlay::chats::overlay_conflicts_to_chat,
            overlay::chats::overlay_redo_group,
            overlay::chats::overlay_undo_merge,
            overlay::chats::overlay_unmark,
            workload::workload_set_slots,
            workload::workload_status,
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
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app.try_state::<Core>() {
                    rt::block_on(core.shutdown());
                }
            }
        });
}

/// The core's events, to every window (src/lib/agentRuns.ts listens).
struct WindowEvents(tauri::AppHandle);

impl EventSink for WindowEvents {
    fn agent(&self, event: &AgentEvent) {
        if let Err(e) = self.0.emit(EVENT, event) {
            eprintln!("agent: could not send an event: {e}");
        }
    }
}
