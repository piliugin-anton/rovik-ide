use crate::{filesystem::WatcherState, terminal::TerminalState};
use rovik_client::Remote;
use rovik_core::agent_run::events::{AgentEvent, EVENT};
use rovik_core::{chat_db, chat_request, process, rt, Core, EventSink, Paths};
use std::{collections::HashMap, sync::Arc, sync::Mutex};
use tauri::{Emitter, Manager};

mod commands;
mod filesystem;
mod formatter;
mod git;
mod git_sync;
mod lsp;
mod open_url;
mod project_files;
mod remote_chats;
mod terminal;
mod typings;

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
            // Chats on hosts (6.1): their requests go to the host, and its
            // events come back through the core's sink.
            let remote = Remote::new(core.clone());
            let window = app.handle().clone();
            remote.on_cloud(Arc::new(move |event| {
                if let Err(e) = window.emit(CLOUD_EVENT, &event) {
                    eprintln!("cloud: could not send an event: {e}");
                }
            }));
            {
                let _runtime = rt::handle().enter();
                remote_chats::from_env(&remote);
            }
            app.manage(core);
            app.manage(remote);
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
            let core = ctx.app_handle().state::<Core>().inner().clone();
            let path = request.uri().path().to_string();
            rt::spawn_blocking(move || {
                responder.respond(attachment_response(core.db.data_dir(), &path));
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
            commands::chat::chat_list_dialogs,
            commands::chat::chat_open_project,
            commands::chat::chat_read_plan,
            commands::chat::chat_answer_copy,
            commands::chat::chat_answer_moved,
            commands::chat::chat_decline_projects,
            commands::chat::chat_list_projects,
            commands::chat::chat_relink_project,
            commands::chat::chat_delete_project,
            commands::chat::chat_get_messages,
            commands::chat::chat_get_messages_around,
            commands::chat::chat_get_message_body,
            commands::chat::chat_create_dialog,
            commands::chat::chat_append_messages,
            commands::chat::chat_update_message,
            commands::chat::chat_rename_dialog,
            commands::chat::chat_delete_dialog,
            commands::chat::chat_search,
            commands::chat::chat_messages_answered,
            commands::chat::chat_store_attachment,
            commands::chat::chat_save_composer_draft,
            commands::chat::chat_get_composer_draft,
            commands::chat::chat_set_model,
            commands::chat::chat_get_model,
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
            commands::agent::send_chat_message,
            commands::agent::cancel_chat_request,
            commands::agent::agent_configure,
            commands::agent::agent_start,
            commands::agent::agent_stop,
            commands::agent::agent_set_effort,
            commands::agent::agent_resolve_approval,
            commands::agent::agent_answer_questions,
            commands::agent::agent_status,
            commands::agent::chat_checklist,
            commands::agent::chat_dismiss_checklist,
            commands::agent::list_models,
            commands::browser::web_search,
            commands::browser::browse_url,
            commands::browser::browser_screenshot,
            commands::tools::tool_list_dir,
            commands::tools::tool_search_code,
            commands::tools::search_in_files,
            commands::tools::tool_read_file_range,
            commands::tools::tool_write_file,
            commands::tools::tool_edit_file,
            commands::tools::report_editor_diagnostics,
            commands::project::project_profile,
            commands::project::load_project_settings,
            commands::project::save_project_settings,
            commands::agent::chat_context_estimate,
            commands::workload::workload_focus,
            commands::overlay::overlay_status,
            commands::overlay::overlay_file,
            commands::overlay::overlay_discard,
            commands::overlay::overlay_land,
            commands::overlay::overlay_undo,
            commands::overlay::overlay_conflict_file,
            commands::overlay::overlay_conflict_resolve,
            commands::overlay::overlay_conflicts_to_chat,
            commands::overlay::overlay_redo_group,
            commands::overlay::overlay_undo_merge,
            commands::overlay::overlay_unmark,
            commands::workload::workload_set_slots,
            commands::workload::workload_status,
            commands::tools::outline_symbols,
            commands::vfs::vfs_status,
            commands::vfs::vfs_install_driver,
            commands::vfs::vfs_mount,
            commands::vfs::vfs_unmount,
            commands::vfs::vfs_open_settings,
            commands::vfs::vfs_write_file,
            commands::vfs::vfs_read_file,
            commands::vfs::vfs_list,
            commands::vfs::vfs_remove,
            commands::vfs::vfs_revisions,
            commands::vfs::vfs_read_revision,
            commands::vfs::vfs_restore_revision,
            commands::vfs::vfs_set_history_limits,
            commands::vfs::vfs_subscribe,
            commands::vfs::vfs_unsubscribe,
            commands::clm::clm_status,
            commands::clm::clm_set_mode,
            commands::clm::clm_reset,
            commands::clm::clm_list_revisions,
            commands::clm::clm_read_revision,
            commands::clm::clm_restore_revision,
            commands::clm::clm_tool_read,
            commands::clm::clm_tool_edit,
            commands::clm::clm_tool_write,
            commands::clm::clm_recall_hint,
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

/// `chat-attachment://localhost/<sha256>`'s response: the attachment, or 404.
fn attachment_response(
    data_dir: &std::path::Path,
    uri_path: &str,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::Response;
    let not_found = || {
        Response::builder()
            .status(404)
            .body(Vec::new())
            .expect("static response")
    };
    match chat_db::attachments::read(data_dir, uri_path) {
        Some((mime, bytes)) => Response::builder()
            .status(200)
            .header("Content-Type", mime)
            // Content-addressed: the bytes for an id never change.
            .header("Cache-Control", "public, max-age=31536000, immutable")
            .body(bytes)
            .unwrap_or_else(|_| not_found()),
        None => not_found(),
    }
}

/// The hosts' own events, apart from the chats': connections and transfers.
const CLOUD_EVENT: &str = "cloud-event";

/// The core's events, to every window (src/lib/agentRuns.ts listens).
struct WindowEvents(tauri::AppHandle);

impl EventSink for WindowEvents {
    fn agent(&self, event: &AgentEvent) {
        if let Err(e) = self.0.emit(EVENT, event) {
            eprintln!("agent: could not send an event: {e}");
        }
    }
}
