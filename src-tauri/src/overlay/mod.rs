//! Chats' overlays: each chat sees the project with its own changes on top,
//! and only the changes are stored. See docs/design/chat-isolation-plan.md.

pub mod fs;
pub mod tiers;

pub use fs::{OverlayChange, OverlayFs};

#[cfg(test)]
mod tests;
