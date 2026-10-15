//! What the app and the agent daemon send each other
//! (docs/design/cloud-agent-dev-plan.md, section 4): the frames of the
//! WebSocket, the requests and their replies, and the events. Serde types
//! only, so the app, the daemon and the core share one definition of every
//! shape, and nothing heavier than serde comes with it.
//!
//! The core's types that travel (a run's events, a start and its settings,
//! what a run waits on) are defined in the public modules here. The core
//! re-exports each where it used to be, so the core reads as before and the
//! wire can't drift from it.

pub mod agent;
pub mod chat;
pub mod clm;
pub mod diff;
pub mod events;
mod frame;
pub mod host;
pub mod permissions;
pub mod plan;
mod request;
pub mod subagents;

pub use frame::{Event, Fail, FailCode, Frame, Secret};
pub use request::{Request, Response};

/// The protocol's version (4.5). Additive changes keep it: new optional
/// fields, and new requests a client finds in `Frame::Welcome`'s features.
pub const PROTOCOL: u32 = 1;

/// The most a frame carries. Anything bigger goes over HTTP, so a big
/// transfer never holds up events and pings (4.1).
pub const MAX_FRAME: usize = 1 << 20;

/// The most a `POST /call` takes: a call too big for a frame (4.1).
pub const MAX_CALL: usize = 64 << 20;

#[cfg(test)]
mod tests;
