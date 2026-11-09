//! Terminal controls execute in the backend and require the owning client.
use super::{encode, DesktopRequest};
use crate::{desktop_backend::DesktopBackend, desktop_session::Session, terminal};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Terminal operations keep the frontend's existing arguments and result shapes.
#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum TerminalRequest {
    /// Starts a local shell and its private output callback.
    Spawn {
        cwd: Option<String>,
        cols: Option<u16>,
        rows: Option<u16>,
        sink: String,
    },
    /// Starts a shell on a configured host, using that host's connection policy.
    SpawnRemote {
        host: String,
        cols: Option<u16>,
        rows: Option<u16>,
        sink: String,
    },
    /// Writes input to a terminal owned by this client.
    Write {
        id: String,
        data: String,
        sink: String,
    },
    /// Resizes a terminal owned by this client.
    Resize { id: String, cols: u16, rows: u16 },
    /// Closes one terminal; an already closed terminal is a no-op.
    Close { id: String },
}

impl TerminalRequest {
    pub(super) async fn dispatch(
        self,
        backend: &DesktopBackend,
        session: Option<&Arc<Session>>,
    ) -> Result<serde_json::Value, String> {
        let owner = session.ok_or("A client session is required for terminals")?;
        let context = |sink: &str| -> Result<terminal::Context, String> {
            owner.begin(sink)?;
            Ok(terminal::Context {
                state: backend.terminals.clone(),
                owner: owner.clone(),
                sink: sink.into(),
                data: backend.core.db.data_dir().to_path_buf(),
            })
        };
        match self {
            Self::Spawn {
                cwd,
                cols,
                rows,
                sink,
            } => {
                let result = terminal::spawn_terminal(context(&sink)?, cwd, cols, rows).await;
                if result.is_err() {
                    owner.cancel_sink(&sink);
                }
                encode(result?)
            }
            Self::SpawnRemote {
                host,
                cols,
                rows,
                sink,
            } => {
                let result = terminal::spawn_remote_terminal(
                    context(&sink)?,
                    &backend.hosts,
                    host,
                    cols,
                    rows,
                )
                .await;
                if result.is_err() {
                    owner.cancel_sink(&sink);
                }
                encode(result?)
            }
            Self::Write { id, data, sink } => {
                require_owner(owner, &id)?;
                let terminals = backend.terminals.clone();
                crate::desktop_task::start(owner, sink, move |_| async move {
                    super::encode_result(terminal::write_to_terminal(&terminals, id, data).await)
                })?;
                encode(())
            }
            Self::Resize { id, cols, rows } => {
                require_owner(owner, &id)?;
                encode(terminal::resize_terminal(
                    &backend.terminals,
                    id,
                    cols,
                    rows,
                )?)
            }
            Self::Close { id } => {
                owner.remove(&format!("terminal:{id}"));
                encode(())
            }
        }
    }
}

fn require_owner(owner: &Session, id: &str) -> Result<(), String> {
    if owner.owns(&format!("terminal:{id}")) {
        Ok(())
    } else {
        Err("This terminal is not open in this client session".into())
    }
}

impl From<TerminalRequest> for DesktopRequest {
    fn from(request: TerminalRequest) -> Self {
        Self::Terminal(Box::new(request))
    }
}
