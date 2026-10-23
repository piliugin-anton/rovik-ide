use crate::filesystem::WatcherState;
use crate::instances::Instances;
use rovik_client::hosts_page::HostsPage;
use rovik_client::provider_keys::ProviderKeys;
use rovik_client::secrets::{SecretError, SecretName, SecretStore};
use rovik_client::Remote;
use rovik_core::agent_run::events::{AgentEvent, EVENT};
use rovik_core::{chat_db, chat_request, process, rt, Core, EventSink, Paths};
use std::sync::Arc;
use tauri::{Emitter, Manager};

mod commands;
mod filesystem;
mod formatter;
mod git;
mod git_sync;
mod instances;
mod lsp;
mod open_url;
mod pinned_browser;
mod project_files;
mod remote_chats;
mod shared_state;
mod terminal;
mod typings;
mod updates;
mod util;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The agent's commands run sandboxed or not at all: on Windows, in WSL.
    rovik_core::overlay::confine::require();
    log_to_file();
    // First, while the process is still single-threaded.
    process::adopt_login_shell_path();
    // Then the core's runtime (rt.rs), Tauri's too: one for the process.
    tauri::async_runtime::set(rt::handle().clone());
    init_log();
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        // Rovik's own updates (updates.rs): its commands only, none of the
        // plugin's, which the window has no permission for.
        .plugin(updates::plugin())
        .manage(updates::Updates::default())
        .manage(terminal::terminals())
        .manage(lsp::servers())
        .manage(chat_request::ChatRequests::default())
        .manage(WatcherState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config = app.path().app_config_dir()?;
            let data = app.path().app_data_dir()?;
            // Rovik runs as several processes, a project open in one of
            // them only (instances.rs): asked by another, this one's window
            // comes to the front.
            let window = app.handle().clone();
            let instances = Instances::start(data.join("open-projects"), move || {
                log::info!("instances: another window asked this one to come to the front");
                if let Some(main) = window.get_webview_window("main") {
                    let _ = main.unminimize();
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            })?;
            // A folder on the command line opens in this window, unless
            // another one has it open: that one comes to the front, and
            // this one ends before it has a window.
            let folder = startup_folder();
            if let Some(path) = &folder {
                if instances.claim(Some(path), true, false).open_elsewhere {
                    std::process::exit(0);
                }
            }
            app.manage(instances);
            app.manage(Startup {
                folder,
                new_window: std::env::args().any(|a| a == NEW_WINDOW),
            });
            // What only one process at a time can have (the virtual files'
            // folder, ssh's sockets): each has its own, by its slot. And the
            // chats each is at work on, which the others leave be.
            rovik_core::os_lock::take_slot(&data.join("slots"));
            rovik_core::chat_hold::init(data.join("chats-at-work"));
            // The providers' API keys, in the keychain or the encrypted
            // file in its place, out of ai-settings.json (H06).
            let secrets = Arc::new(SecretStore::open(config.join("secrets.json")));
            // Settings → Network (rovik_core::net) before anything connects:
            // its file beside the AI settings, the proxies' passwords in the
            // keychain, each read once a route that signs in to it is used.
            let store = secrets.clone();
            rovik_core::net::init(
                config.join("network-settings.json"),
                Arc::new(move |proxy: &str| {
                    let name = SecretName::ProxyPassword {
                        proxy: proxy.to_string(),
                    };
                    match store.get(&name) {
                        Ok(password) => Ok(Some(password)),
                        Err(SecretError::Missing(_)) => Ok(None),
                        Err(e) => Err(e.to_string()),
                    }
                }),
            );
            app.manage(secrets.clone());
            let keys = ProviderKeys::new(config.join("ai-settings.json"), secrets.clone());
            app.manage(Arc::new(keys));
            let paths = Paths::new(data.clone(), config.clone());
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
            // The chats the database puts on hosts go there from the start.
            if let Err(e) = rt::block_on(remote.load()) {
                eprintln!("cloud: the chats on hosts couldn't be read: {e}");
            }
            // Settings → Cloud hosts (U01), and the hosts deployed there,
            // connected from the start.
            let hosts = Arc::new(HostsPage::new(
                config,
                data.join("ssh"),
                secrets,
                remote.clone(),
                env!("CARGO_PKG_VERSION"),
            ));
            app.manage(hosts.clone());
            app.manage(core.clone());
            app.manage(remote.clone());
            // A project is open in one window (instances.rs): its remote
            // chats' results, rows and events are that window's.
            let windows = app.handle().clone();
            remote.on_open_elsewhere(Arc::new(move |project| {
                windows
                    .try_state::<Instances>()
                    .is_some_and(|i| i.open_elsewhere(Some(project), None).is_some())
            }));
            // Another window may change the hosts' settings: their policies
            // are read again before one goes to a host.
            let page = Arc::downgrade(&hosts);
            remote.on_refresh(Arc::new(move || {
                if let Some(hosts) = page.upgrade() {
                    hosts.follow_policies();
                }
            }));
            // A project left for another while its chats were at work stays
            // this window's until they're done (instances.rs).
            let handle = app.handle().clone();
            let following = hosts.clone();
            rt::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    // The hosts another window added, deployed, changed or
                    // removed: this window's connections follow.
                    let hosts = following.clone();
                    let _ = rt::spawn_blocking(move || hosts.follow()).await;
                    let instances = handle.state::<Instances>();
                    let mut still = Vec::new();
                    for project in instances.left_at_work() {
                        if at_work_in(&core, project.clone()).await {
                            still.push(project);
                        }
                    }
                    instances.let_go_done(|p| still.contains(p));
                }
            });
            // Off the main thread: connecting reads the hosts' tokens from
            // the keychain, which may take a moment or ask the user, and
            // the window comes first.
            rt::spawn_blocking(move || {
                remote_chats::from_env(&remote);
                hosts.connect_all();
            });
            // The main window, as tauri.conf.json describes it (`create`
            // is off there): built here to let its page read the clipboard,
            // which a config can't. Pasting an image into the chat input
            // needs that: WebKitGTK's paste event leaves images out (even
            // for Ctrl+V), so they're read through the clipboard API, which
            // it refuses without this; WebView2 asks for permission. The
            // input's own menu reads text that way too. macOS allows it.
            if let Some(main) = app.config().app.windows.iter().find(|w| w.label == "main") {
                let window = tauri::WebviewWindowBuilder::from_config(app.handle(), main)?
                    .enable_clipboard_access()
                    .build()?;
                cascade(&window, rovik_core::os_lock::slot());
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
            terminal::spawn_remote_terminal,
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
            filesystem::ai_settings_stamp,
            commands::chat::chat_list_dialogs,
            commands::chat::chat_open_project,
            commands::chat::chats_at_work_elsewhere,
            commands::chat::chat_unsettled_sends,
            commands::chat::chat_read_plan,
            commands::chat::chat_answer_copy,
            commands::chat::chat_answer_moved,
            commands::chat::chat_decline_projects,
            commands::chat::chat_list_projects,
            commands::chat::chat_relink_project,
            commands::chat::chat_delete_project,
            commands::chat::chat_get_messages,
            commands::chat::chat_get_messages_around,
            commands::chat::chat_task_stages,
            commands::chat::chat_get_message_body,
            commands::chat::chat_create_dialog,
            commands::chat::chat_update_message,
            commands::chat::chat_rename_dialog,
            commands::chat::chat_delete_dialog,
            commands::chat::chat_resync_dialog,
            commands::chat::chat_get_dialog,
            commands::chat::cloud_move_preflight,
            commands::chat::cloud_move_chat,
            commands::chat::cloud_move_back,
            commands::chat::cloud_cancel_move,
            commands::hosts::hosts_list,
            commands::hosts::hosts_summaries,
            commands::hosts::hosts_save,
            commands::wsl::wsl_status,
            commands::wsl::wsl_setup,
            commands::wsl::wsl_install,
            commands::wsl::wsl_test,
            commands::wsl::wsl_update,
            commands::wsl::wsl_remove,
            commands::hosts::hosts_remove,
            commands::hosts::hosts_test,
            commands::hosts::hosts_trust_key,
            commands::hosts::hosts_trust_build,
            commands::hosts::hosts_set_policy,
            commands::hosts::hosts_set_autonomy,
            commands::hosts::hosts_test_delivery,
            commands::hosts::hosts_tasks,
            commands::hosts::hosts_deploy,
            commands::hosts::hosts_upgrade,
            commands::hosts::hosts_run_setup,
            commands::hosts::hosts_unlock,
            commands::hosts::hosts_removal,
            commands::hosts::hosts_remove_rovik,
            commands::hosts::hosts_details,
            commands::hosts::hosts_remove_keys,
            commands::hosts::hosts_clean_up,
            commands::hosts::hosts_limits,
            commands::hosts::hosts_set_limits,
            commands::chat::chat_search,
            commands::chat::chat_messages_answered,
            commands::chat::chat_messages_stored,
            commands::chat::chat_store_attachment,
            commands::chat::chat_save_composer_draft,
            commands::chat::chat_get_composer_draft,
            commands::chat::chat_set_model,
            commands::chat::chat_get_model,
            filesystem::load_last_project,
            filesystem::save_last_project,
            shared_state::shared_state_read,
            shared_state::shared_state_start,
            shared_state::shared_state_merge,
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
            git_sync::git_rebase,
            project_claim,
            project_check,
            startup_info,
            new_window,
            git_sync::git_conflict_file,
            git_sync::git_conflict_resolve,
            git::git_commit,
            git::git_hooks,
            git::git_hook_create,
            git::git_hook_make_executable,
            git::git_hook_delete,
            git::git_push,
            git::git_checkout,
            git::git_branches,
            git::git_status,
            git::git_base_content,
            git::git_revert_file,
            git::git_revert_folder,
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
            typings::load_project_definitions,
            typings::load_project_sources,
            typings::load_project_typings,
            typings::load_dependency_typings,
            project_files::list_project_files,
            lsp::lsp_start,
            lsp::lsp_send,
            lsp::lsp_stop,
            lsp::rust_analyzer::ra_find,
            lsp::rust_analyzer::ra_latest_release,
            lsp::rust_analyzer::ra_install,
            lsp::rust_analyzer::ra_rust_src_status,
            lsp::rust_analyzer::ra_install_rust_src,
            pinned_browser::browser_status,
            pinned_browser::browser_install,
            commands::agent::send_chat_message,
            commands::agent::cancel_chat_request,
            commands::agent::agent_configure,
            commands::agent::agent_start,
            commands::agent::agent_stop,
            commands::agent::agent_steer,
            commands::agent::agent_unsteer,
            commands::agent::agent_set_effort,
            commands::agent::agent_resolve_approval,
            commands::agent::agent_answer_questions,
            commands::agent::agent_status,
            commands::agent::chat_checklist,
            commands::agent::chat_dismiss_checklist,
            commands::agent::list_models,
            commands::agent::connector_probe,
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
            commands::overlay::overlay_sync_now,
            commands::overlay::overlay_sync_to_chat,
            commands::overlay::overlay_keep_older,
            commands::overlay::overlay_take_newer,
            commands::overlay::overlay_undo_merge,
            commands::overlay::overlay_unmark,
            commands::workload::workload_set_slots,
            commands::workload::workload_status,
            commands::network::network_settings,
            commands::network::network_save,
            commands::tools::outline_symbols,
            commands::vfs::vfs_status,
            commands::vfs::vfs_install_driver,
            commands::sandbox::sandbox_status,
            commands::sandbox::sandbox_install,
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
            updates::update_info,
            updates::update_check,
            updates::update_download,
            updates::update_install,
            updates::update_restart,
            updates::update_at_work,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                before_exit(app);
                // An update's restart (updates.rs), once all that's done.
                updates::relaunch(app);
            }
        });
}

/// What the app does as it ends: the core done with the project's chats,
/// then the project let go of, for another window or the next start. Also
/// before an update's installer ends the process at once (Windows), on a
/// thread off the runtime's.
fn before_exit(app: &tauri::AppHandle) {
    if let Some(core) = app.try_state::<Core>() {
        rt::block_on(core.shutdown());
    }
    // Only once the core is done with the project's chats.
    if let Some(instances) = app.try_state::<Instances>() {
        instances.release();
    }
}

/// Every window restores the one place saved (window-state): one started
/// while others run (its slot, 2 for the second) steps down from it, as
/// new windows do, instead of opening exactly over the window it came
/// from, which looked as if that one's project had closed.
fn cascade(window: &tauri::WebviewWindow, slot: usize) {
    if slot <= 1 || window.is_maximized().unwrap_or(false) {
        return;
    }
    let (Ok(at), Ok(scale)) = (window.outer_position(), window.scale_factor()) else {
        return;
    };
    let step = (32.0 * scale) as i32 * ((slot - 1) % 8) as i32;
    let (mut x, mut y) = (at.x + step, at.y + step);
    // Still on its screen: back to its corner otherwise.
    if let (Ok(Some(screen)), Ok(size)) = (window.current_monitor(), window.outer_size()) {
        let (origin, room) = (screen.position(), screen.size());
        if x + size.width as i32 > origin.x + room.width as i32 {
            x = origin.x + step / 2;
        }
        if y + size.height as i32 > origin.y + room.height as i32 {
            y = origin.y + step / 2;
        }
    }
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// How this window was started: the folder given on the command line
/// (`rovik-ide <folder>`), claimed, and whether it's another window
/// started from one (`new_window`).
struct Startup {
    folder: Option<std::path::PathBuf>,
    new_window: bool,
}

/// What `new_window` starts the other Rovik with: a window that starts
/// without a project (the last one is open in this one).
const NEW_WINDOW: &str = "--new-window";

/// Whether this process is at work on a chat of the project `project`
/// (None: of the chats without a project): a run, Autopilot's driver, a
/// stage waiting out a rate limit (rovik_core::chat_hold).
async fn at_work_in(core: &Core, project: Option<String>) -> bool {
    let held = rovik_core::chat_hold::held_here();
    if held.is_empty() {
        return false;
    }
    core.db
        .read(move |c| chat_db::projects::has_lead_chat(c, &held, project.as_deref()))
        .await
        .unwrap_or(false)
}

/// The first argument that's a folder: others (macOS's `-psn_…`, a
/// WebDriver's flags) aren't for Rovik.
fn startup_folder() -> Option<std::path::PathBuf> {
    std::env::args_os()
        .skip(1)
        .filter(|a| !a.to_string_lossy().starts_with('-'))
        .map(std::path::PathBuf::from)
        .find(|p| p.is_dir())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupInfo {
    /// The folder the command line asked this window to open, if any.
    folder: Option<String>,
    /// Started by another window's "New window": no project to restore.
    new_window: bool,
}

#[tauri::command]
fn startup_info(startup: tauri::State<'_, Startup>) -> StartupInfo {
    StartupInfo {
        folder: startup
            .folder
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        new_window: startup.new_window,
    }
}

/// Claims a project for this window before it opens (instances.rs; no
/// `path`: the chats without a project): when another window has it, or a
/// copy of it sharing its chats, it isn't opened here, and that window is
/// asked to come to the front (`focus_other`). The project open here stays
/// this window's while its chats are at work.
#[tauri::command]
async fn project_claim(
    app: tauri::AppHandle,
    core: tauri::State<'_, Core>,
    path: Option<String>,
    focus_other: bool,
) -> Result<instances::Claim, String> {
    let open = app.state::<Instances>().open_project();
    let keep_open = match open {
        Some(project) => at_work_in(&core, project).await,
        None => false,
    };
    rt::spawn_blocking(move || {
        let path = path.as_deref().map(std::path::Path::new);
        app.state::<Instances>().claim(path, focus_other, keep_open)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Whether another window has a project open (instances.rs), without
/// claiming it; that window is asked to come to the front (`focus_other`).
#[tauri::command]
async fn project_check(
    app: tauri::AppHandle,
    path: Option<String>,
    focus_other: bool,
) -> Result<instances::Claim, String> {
    rt::spawn_blocking(move || {
        let path = path.as_deref().map(std::path::Path::new);
        app.state::<Instances>().check(path, focus_other)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Starts another Rovik: a window of its own, for a project of its own.
#[tauri::command]
fn new_window() -> Result<(), String> {
    let mut cmd = another_rovik()?;
    let mut child = cmd
        .arg(NEW_WINDOW)
        .spawn()
        .map_err(|e| format!("Couldn't start another Rovik: {e}"))?;
    // Reaped if it ends first; it doesn't end with this one.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// A command that starts another Rovik as the desktop starts one: for "New
/// window", and for an update's restart (updates.rs).
///
/// - From an AppImage, the AppImage again, with the user's environment and
///   in the folder it was started from, as every program the app starts
///   (process::command). This binary would need the AppImage's environment
///   and folder; and the new AppImage's launcher adds its own to the
///   environment it's given, so given this process's, its terminals kept
///   this AppImage's folders, its libraries first.
/// - Else this binary, by the path it started from: on Linux a package's
///   update may have replaced it since, and the path the system gives for
///   it then ends in "(deleted)".
///
/// Out of this one's terminal, if it was started from one: neither stopped
/// by it nor ended with it (process::own_session). And holding none of its
/// files (process::close_inherited): this AppImage's mount goes once this
/// process ends.
fn another_rovik() -> Result<std::process::Command, String> {
    use std::process::Stdio;
    let mut cmd = match std::env::var_os("APPIMAGE") {
        Some(image) => process::command(image),
        None => {
            let binary = if cfg!(target_os = "linux") {
                tauri::utils::platform::current_exe()
            } else {
                std::env::current_exe()
            };
            std::process::Command::new(binary.map_err(|e| e.to_string())?)
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    process::own_session(&mut cmd);
    process::close_inherited(&mut cmd);
    Ok(cmd)
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

/// The `log` crate's messages (rovik-client's, mostly) on stderr, where
/// the app's own go. `ROVIK_LOG` sets the level (error, warn, info, debug,
/// trace); info without it.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// `ROVIK_LOG_FILE=<path>`: what the app writes to stderr (its log, and
/// every message printed there) is appended to that file instead. A Windows
/// release build is a GUI app, whose stderr goes nowhere: this is how its
/// log is seen (the end-to-end runs use it).
fn log_to_file() {
    let Some(path) = std::env::var_os("ROVIK_LOG_FILE") else {
        return;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        // SAFETY: points descriptor 2 at the file, whose own descriptor is
        // left open for the life of the process.
        unsafe { libc::dup2(file.into_raw_fd(), 2) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetStdHandle(std_handle: u32, handle: *mut std::ffi::c_void) -> i32;
        }
        // SAFETY: the file's handle, left open for the life of the process;
        // std looks the stderr handle up on each write.
        unsafe { SetStdHandle(STD_ERROR_HANDLE, file.into_raw_handle()) };
    }
}

fn init_log() {
    let level = std::env::var("ROVIK_LOG")
        .ok()
        .and_then(|level| level.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    if log::set_logger(&StderrLog).is_ok() {
        log::set_max_level(level);
    }
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
