//! What the daemon does with a request: the core's operations through
//! `dispatch`, as the app does for its own chats (D3), and the Host group,
//! which only a daemon answers. The WebSocket and `POST /call` both come
//! here, so a call gets the same reply either way (4.1).

use crate::auth::Tokens;
use crate::events::Events;
use crate::host;
use crate::store::Store;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::Core;
use rovik_protocol::agent::StartRequest;
use rovik_protocol::{Fail, FailCode, Request, Response};
use std::sync::Arc;

/// The oldest app protocol the daemon serves (4.5).
pub const MIN_PROTOCOL: u32 = 1;

pub struct Daemon {
    pub core: Core,
    /// The computers that may connect (config.toml, read at start-up).
    pub tokens: Tokens,
    /// Where the core's events go, and where clients subscribe to them.
    pub events: Arc<Events>,
    /// Each chat's computer and settings, each computer's configuration.
    pub store: Arc<Store>,
    /// `--same-machine`: a chat runs in the folder its start names, for
    /// development until projects are carried to the server (T04).
    pub same_machine: bool,
    /// Why the virtual files aren't mounted, when they aren't.
    pub virtual_files_problem: Option<String>,
}

impl Daemon {
    /// `computer`'s request: the computer the token named.
    pub async fn answer(&self, computer: &str, request: Request) -> Result<Response, Fail> {
        match request {
            Request::HostInfo => Ok(Response::HostInfo(host::info(self).await)),
            Request::AgentStart(start) => self.start(computer, start).await,
            // Only this computer's configuration changes (D16).
            Request::AgentConfigure(config) => self
                .store
                .configure(computer, config)
                .map(|()| Response::Done)
                .map_err(|e| Fail::new(FailCode::Internal, e)),
            request => rovik_core::dispatch(&self.core, request).await,
        }
    }

    /// A start (6.2): the store keeps its keys apart and its settings, the
    /// chat becomes the computer's, and the run follows the computer's
    /// configuration (`config_for`), never another's.
    async fn start(&self, computer: &str, mut start: StartRequest) -> Result<Response, Fail> {
        if !self.same_machine {
            return Err(Fail::new(
                FailCode::BadRequest,
                "This server can't run chats yet: its daemon runs them only with \
                 --same-machine, in the folder each start names.",
            ));
        }
        self.store.started(computer, &start)?;
        start.config = None;
        // The daemon works in its own copy, never in an overlay (D8).
        start.isolate = Some(false);
        self.ensure_chat(&start.dialog_id).await?;
        let dialog_id = start.dialog_id.clone();
        let started = rovik_core::dispatch(&self.core, Request::AgentStart(start)).await;
        if !matches!(started, Ok(Response::RunId(Some(_)))) {
            self.store.ended(&dialog_id);
        }
        started
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
