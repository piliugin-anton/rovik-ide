//! The Tauri commands of the core's operations. Each is a thin wrapper: it
//! takes what Tauri hands a command (the app's `Core`, the arguments by
//! name, a channel or the request's body) and calls the core function of
//! the same name. The core stays free of Tauri, and other hosts can serve
//! the same operations (docs/design/cloud-agent-dev-plan.md, D2 and C04).
//!
//! What can be done to a chat (the Agent, Chat and CLM groups) is a
//! `Request`: the core's `dispatch` answers it, as it does on the daemon
//! (D3), or the chat's host does when the chat is there (`Remote`, 6.1).

use rovik_client::Remote;
use rovik_core::Core;
use rovik_protocol::{Fail, Request, Response};

pub mod agent;
pub mod browser;
pub mod chat;
pub mod clm;
pub mod hosts;
pub mod network;
pub mod overlay;
pub mod project;
pub mod sandbox;
pub mod tools;
pub mod vfs;
pub mod workload;
pub mod workspace;
pub mod wsl;

/// A chat's operation, served by `dispatch` here: its reply as the type the
/// command returns, or the error text the command returns.
async fn call<T>(core: &Core, request: Request) -> Result<T, String>
where
    T: TryFrom<Response, Error = Fail>,
{
    answer(rovik_core::dispatch(core, request).await)
}

/// A chat's operation, served where the chat is (here, or on its host):
/// its reply as the type the command returns, or the error text.
async fn route<T>(remote: &Remote, dialog_id: &str, request: Request) -> Result<T, String>
where
    T: TryFrom<Response, Error = Fail>,
{
    answer(remote.route(dialog_id, request).await)
}

/// A reply as the type the command returns, or the error text it returns.
fn answer<T>(reply: Result<Response, Fail>) -> Result<T, String>
where
    T: TryFrom<Response, Error = Fail>,
{
    reply.and_then(T::try_from).map_err(|fail| fail.message)
}
