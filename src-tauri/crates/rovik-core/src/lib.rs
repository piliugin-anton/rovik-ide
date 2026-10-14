//! The agent and everything it calls, without Tauri. A host (the app, the
//! headless runner, and later the daemon) builds a [`Core`] and calls the
//! core's operations with it (docs/design/cloud-agent-dev-plan.md, D1, D2).

pub mod agent;
pub mod agent_run;
pub mod agent_tools;
mod api_log;
pub mod atomic_file;
pub mod browser_tools;
pub mod chat_db;
pub mod chat_request;
pub mod clm;
pub mod code_search;
pub mod command_tool;
pub mod core;
mod edit_file;
pub mod file_guard;
/// The agent without a window, for measuring it (G00): `rovik-headless`.
pub mod headless;
pub mod merge;
mod opencode;
pub mod outline;
pub mod overlay;
pub mod process;
pub mod project_profile;
mod prompt_cache;
mod read_file;
mod read_symbol;
mod reasoning_echo;
pub mod release_assets;
mod repo_map;
pub mod rt;
mod system_one;
pub mod vfs;
pub mod workload;

pub use crate::core::{Core, EventSink, Paths};
