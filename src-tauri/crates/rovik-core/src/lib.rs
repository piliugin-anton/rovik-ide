//! The agent and everything it calls, without Tauri. A host (the app, the
//! headless runner, and later the daemon) builds a [`Core`] and calls the
//! core's operations with it (docs/design/cloud-agent-dev-plan.md, D1, D2).

pub mod agent;
pub mod agent_run;
pub mod agent_tools;
mod anthropic;
mod api_log;
pub mod atomic_file;
pub mod browser_tools;
pub mod chat_db;
pub mod chat_hold;
pub mod chat_request;
pub mod clm;
pub mod code_search;
pub mod command_tool;
pub mod connectors;
pub mod core;
mod dispatch;
#[cfg(target_os = "linux")]
pub mod distro;
pub mod ecosystems;
mod edit_file;
pub mod file_guard;
#[cfg(target_os = "linux")]
pub mod listening;
pub mod merge;
pub mod net;
mod opencode;
pub mod os_lock;
pub mod outline;
pub mod overlay;
pub mod packs;
pub mod pinned_browser;
pub mod pinned_compose;
pub mod process;
pub mod project_profile;
pub mod prompt_cache;
mod read_file;
mod read_symbol;
mod reasoning_echo;
pub mod release_assets;
mod repo_map;
pub mod rt;
pub mod staging;
mod system_one;
/// Test support for this crate's tests and other crates' (the `testing`
/// feature): a scripted model, temporary folders, a `Core` on one.
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod text;
pub mod toolchains;
pub mod typings;
pub mod vfs;
pub mod workload;

pub use crate::core::{Core, EventSink, Paths};
pub use crate::dispatch::{dispatch, refusal};
