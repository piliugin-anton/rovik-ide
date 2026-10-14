//! The Tauri commands of the core's operations. Each is a thin wrapper: it
//! takes what Tauri hands a command (the app's `Core`, the arguments by
//! name, a channel or the request's body) and calls the core function of
//! the same name. The core stays free of Tauri, and other hosts can serve
//! the same operations (docs/design/cloud-agent-dev-plan.md, D2 and C04).

pub mod agent;
pub mod browser;
pub mod chat;
pub mod clm;
pub mod overlay;
pub mod project;
pub mod tools;
pub mod vfs;
pub mod workload;
