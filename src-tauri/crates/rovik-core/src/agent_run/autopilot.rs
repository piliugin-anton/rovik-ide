//! Autopilot (docs/design/autopilot-plan.md): a chat mode in which one
//! message is a whole task. The agent plans it, the plan is reviewed, the
//! plan is implemented and the change checked, and the result delivered,
//! with nobody at the keyboard.
//!
//! A task is a pipeline of runs in one chat (D2), not one long run. Its
//! first run is a Plan-mode run, started by the user's message.

pub use rovik_protocol::autopilot::{AutopilotConfig, Budget, Effort};

/// The mode of a message sent in Autopilot (`meta.mode`, `StartRequest.mode`).
pub const MODE: &str = "autopilot";
