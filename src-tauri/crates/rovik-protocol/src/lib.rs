//! What the app and the agent daemon send each other
//! (docs/design/cloud-agent-dev-plan.md, section 4): the frames of the
//! WebSocket, the requests and their replies, and the events. Serde types,
//! so the app, the daemon and the core share one definition of every
//! shape, and nothing heavier than serde comes with it; and how Rovik's
//! releases are named and signed (`release`), which both sides check.
//!
//! The core's types that travel (a run's events, a start and its settings,
//! what a run waits on) are defined in the public modules here. The core
//! re-exports each where it used to be, so the core reads as before and the
//! wire can't drift from it.

pub mod agent;
pub mod autopilot;
pub mod chat;
pub mod clm;
pub mod connectors;
pub mod diff;
pub mod events;
mod frame;
pub mod host;
pub mod mcp;
pub mod network;
pub mod packs;
pub mod permissions;
pub mod plan;
pub mod release;
mod request;
pub mod subagents;
pub mod sync;
pub mod work;

pub use frame::{Event, Fail, FailCode, Frame, Secret};
pub use request::{Access, Request, Response, Target};

/// The protocol's version (4.5). Additive changes keep it: new optional
/// fields, and new requests a client finds in `Frame::Welcome`'s features.
pub const PROTOCOL: u32 = 1;

/// The most a frame carries. Anything bigger goes over HTTP, so a big
/// transfer never holds up events and pings (4.1).
pub const MAX_FRAME: usize = 1 << 20;

/// The most a `POST /call` takes: a call too big for a frame (4.1).
pub const MAX_CALL: usize = 64 << 20;

/// The most a `PUT /blobs/<sha256>` takes: an attached PDF's 20 MB (6.11).
pub const MAX_BLOB: usize = 20 << 20;

/// The most a `POST /chats/import` takes: the rows of a chat that moves
/// (6.8), which the daemon holds while it imports them.
pub const MAX_BUNDLE: usize = 512 << 20;

/// What a client asks `GET /forward/{port}` to become (its `Upgrade`
/// header): a plain byte stream to that port on the loopback of the user's
/// private network on a team server (D20), which a port of the user's
/// computer forwarded for a page carries.
pub const FORWARD_UPGRADE: &str = "rovik-forward";

/// What a daemon serves beyond what protocol 1 began with, named in its
/// `Welcome` (4.5). A client checks for one before it relies on it.
pub mod features {
    /// A message's images and PDFs: `BlobsMissing`, `/blobs`, and starts
    /// whose message names attachments, which a daemon without it would
    /// send to the model without them (K46).
    pub const ATTACHMENTS: &str = "attachments";
    /// The project transport (6.4–6.7): `/packs`, and the Work group as
    /// its tasks bring it.
    pub const PROJECTS: &str = "projects";
    /// The mirror and the move (6.3, 6.8): `ChatChangesSince`, and the
    /// requests and the bundle that move a chat as their tasks bring them.
    pub const MIRROR: &str = "mirror";
    /// The Host group beyond `HostInfo`, for the hosts page (U01):
    /// `KeysList`, `KeysRemove` and `Storage`.
    pub const HOST: &str = "host";
    /// "Clean up now" on the hosts page (R01): `CleanUp`.
    pub const CLEAN_UP: &str = "clean-up";
    /// A server that works on its own (autopilot-plan H02):
    /// `AutonomyConfigure`.
    pub const AUTONOMY: &str = "autonomy";
    /// MCP servers run on the daemon (mcp-plan-2.md, Q11): `McpTest`, and
    /// starts' `mcp_secrets`.
    pub const MCP: &str = "mcp";
}

#[cfg(test)]
mod tests;
