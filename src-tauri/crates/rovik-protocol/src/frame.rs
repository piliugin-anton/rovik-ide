//! What travels on the WebSocket at `GET /rpc` (4.1): JSON text frames, in
//! both directions, of at most `MAX_FRAME` bytes each.

use crate::events::AgentEvent;
use crate::work::{ChatResult, ChatSync};
use crate::{Request, Response};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Frame {
    /// Client → daemon, first frame. `device` names the computer, and
    /// `token` is its token (D5).
    Hello {
        protocol: u32,
        client: String,
        device: String,
        token: Secret,
    },
    /// Daemon → client. `boot` changes on every daemon start, so a client
    /// knows its `since` is from another process.
    Welcome {
        protocol: u32,
        min_protocol: u32,
        daemon: String,
        boot: String,
        features: Vec<String>,
    },
    Call {
        id: u64,
        req: Request,
    },
    Reply {
        id: u64,
        res: Result<Response, Fail>,
    },
    /// After `Subscribe`: the events of this computer's chats, numbered
    /// per daemon process.
    Event {
        seq: u64,
        ev: Event,
    },
    /// `since`: the boot and the `seq` of the last event the client has.
    Subscribe {
        since: Option<(String, u64)>,
    },
    /// `complete` is false when the events after `since` are gone: the
    /// client catches up from the database instead (4.3).
    Subscribed {
        replayed: u64,
        complete: bool,
    },
    Ping,
    Pong,
}

/// What the daemon tells a client about its chats (4.3).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    /// A run's event, as the app emits it as `agent-event`.
    Agent(AgentEvent),
    /// Rows of the chat or its subagents' chats changed, up to the daemon's
    /// revision `rev`: its mirror pulls them (6.3).
    ChatChanged { dialog_id: String, rev: i64 },
    /// A chat's result is committed (6.5).
    ResultReady(ChatResult),
    /// A sync brought the project's changes to a chat's worktree, or waits
    /// (6.7).
    SyncState(ChatSync),
    /// About the host rather than a chat.
    HostNotice { kind: String, message: String },
    /// In place of an event too big for a frame (4.1): catch this chat up.
    Resync { dialog_id: String },
    /// A command of the chat's, on a team server, asks to open `url`, a
    /// page of one of its servers on `port` of the user's private network
    /// there (D20): the app forwards its own `port` on 127.0.0.1 to that
    /// one (`/forward/{port}`), opens the page (in a browser tab of the
    /// editor, or the default browser), and says how it went (`PageOpened`
    /// with `id`).
    PageWanted {
        dialog_id: String,
        id: String,
        url: String,
        port: u16,
    },
    /// Nothing listens on `port` of the user's private network any more,
    /// which the app forwarded for a page of the chat's (`PageWanted`): it
    /// stops forwarding its own.
    PageClosed { dialog_id: String, port: u16 },
    /// A run or Test on the host needs an MCP server on the loopback of
    /// the user's computer, which the host can't reach (mcp-plan-2.md,
    /// Q11): the app takes the POST (`McpRelayTake` with `id`), makes it,
    /// and sends its answer back as it comes (`McpRelayPart`).
    McpRelayWanted { id: String },
}

/// Why a request failed (4.1). The app turns it into the error text its
/// command returns: `message`, which is written for the user.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Fail {
    pub code: FailCode,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FailCode {
    /// The chat's host can't be reached.
    Unreachable,
    /// The chat is owned elsewhere (4.2).
    NotOwner,
    Busy,
    /// The two sides' protocols don't fit together (4.5).
    Version,
    BadRequest,
    /// The reply is over `MAX_FRAME`: repeat the call through `POST /call`.
    TooLarge,
    /// The operation itself failed; `message` is its error, as the app's
    /// command returns it for a local chat.
    Internal,
}

impl Fail {
    pub fn new(code: FailCode, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fail {}

/// A token or a key. `Debug` doesn't show it, so a frame that gets logged
/// can't leak it (plan rule 9), and its memory is wiped when it's dropped.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}
