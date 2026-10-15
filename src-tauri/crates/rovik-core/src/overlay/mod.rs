//! Chats' overlays: each chat sees the project with its own changes on top,
//! and only the changes are stored. See docs/design/chat-isolation-plan.md.

pub mod chats;
pub mod confine;
pub mod fs;
pub mod import;
pub mod land;
pub mod tiers;

#[cfg(test)]
mod tests;
