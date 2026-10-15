//! What the daemon does with a request: the core's operations through
//! `dispatch`, as the app does for its own chats (D3), and the Host group,
//! which only a daemon answers. The WebSocket and `POST /call` both come
//! here, so a call gets the same reply either way (4.1).

use crate::auth::Tokens;
use crate::host;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::Core;
use rovik_protocol::{Fail, FailCode, Request, Response};

/// The oldest app protocol the daemon serves (4.5).
pub const MIN_PROTOCOL: u32 = 1;

pub struct Daemon {
    pub core: Core,
    /// The computers that may connect (config.toml, read at start-up).
    pub tokens: Tokens,
    /// New on every start, so a client knows its `since` is another
    /// process's (4.3).
    pub boot: String,
    /// `--same-machine`: a chat runs in the folder its start names, for
    /// development until projects are carried to the server (T04).
    pub same_machine: bool,
    /// Why the virtual files aren't mounted, when they aren't.
    pub virtual_files_problem: Option<String>,
}

impl Daemon {
    pub async fn answer(&self, request: Request) -> Result<Response, Fail> {
        match request {
            Request::HostInfo => Ok(Response::HostInfo(host::info(self).await)),
            Request::AgentStart(mut start) => {
                if !self.same_machine {
                    return Err(Fail::new(
                        FailCode::BadRequest,
                        "This server can't run chats yet: its daemon runs them only with \
                         --same-machine, in the folder each start names.",
                    ));
                }
                // The daemon works in its own copy, never in an overlay (D8).
                start.isolate = Some(false);
                self.ensure_chat(&start.dialog_id).await?;
                rovik_core::dispatch(&self.core, Request::AgentStart(start)).await
            }
            request => rovik_core::dispatch(&self.core, request).await,
        }
    }

    /// A chat started here before chats can move here (M04) has no row
    /// yet: it gets one, so its messages can be stored.
    async fn ensure_chat(&self, dialog_id: &str) -> Result<(), Fail> {
        let id = dialog_id.to_string();
        self.core
            .db
            .write(move |c| {
                if store::get_dialog(c, &id)?.is_none() {
                    let now = store::now_ms();
                    store::create_dialog(
                        c,
                        &NewDialog {
                            id,
                            title: "Chat".into(),
                            created_at: now,
                            updated_at: now,
                            project_id: None,
                        },
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|e| Fail::new(FailCode::Internal, e))
    }
}
