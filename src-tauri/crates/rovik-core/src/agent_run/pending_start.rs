//! Stop remains effective while an accepted start is still resolving its chat.
use super::{AgentState, StopSignal};
use std::sync::Arc;

const STOPPED: &str = "The agent start was stopped. Send the request again to retry.";

pub(super) struct Starting {
    users: usize,
    stop: Arc<StopSignal>,
}

/// Keeps a start cancellable before its run has registered its controls.
/// Nested routing and preparation share the same signal until they return.
pub struct PendingStart {
    state: AgentState,
    dialog_id: String,
    pub(super) stop: Arc<StopSignal>,
}

impl PendingStart {
    /// Rejects a start stopped while it was resolving its project or waiting.
    pub fn check(&self) -> Result<(), String> {
        if self.stop.is_set() {
            Err(STOPPED.into())
        } else {
            Ok(())
        }
    }

    pub(super) async fn wait<T>(
        &self,
        operation: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        let mut stopped = self.stop.subscribe();
        self.check()?;
        tokio::select! {
            biased;
            _ = stopped.changed() => Err(STOPPED.into()),
            result = operation => {
                self.check()?;
                Ok(result)
            }
        }
    }
}

impl Drop for PendingStart {
    fn drop(&mut self) {
        let mut inner = self.state.lock();
        if let Some(starting) = inner.starting.get_mut(&self.dialog_id) {
            starting.users -= 1;
            if starting.users == 0 {
                inner.starting.remove(&self.dialog_id);
            }
        }
    }
}

impl AgentState {
    /// Marks an accepted start before routing or database waits can let Stop pass it.
    pub fn begin_start(&self, dialog_id: &str) -> PendingStart {
        let mut inner = self.lock();
        let starting = inner
            .starting
            .entry(dialog_id.into())
            .or_insert_with(|| Starting {
                users: 0,
                stop: Arc::new(StopSignal::new()),
            });
        starting.users += 1;
        PendingStart {
            state: self.clone(),
            dialog_id: dialog_id.into(),
            stop: starting.stop.clone(),
        }
    }

    /// Cancels accepted starts still resolving a local or remote destination.
    pub fn stop_starting(&self, dialog_id: &str) -> bool {
        let inner = self.lock();
        match inner.starting.get(dialog_id) {
            Some(starting) => {
                starting.stop.set();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_starts_share_stop_but_a_later_start_gets_a_fresh_signal() {
        let state = AgentState::default();
        let routed = state.begin_start("chat");
        let preparing = state.begin_start("chat");
        let other = state.begin_start("other");
        assert!(state.stop_starting("chat"));
        assert!(routed.check().is_err());
        assert!(preparing.check().is_err());
        assert!(other.check().is_ok());
        drop(routed);
        assert!(state.begin_start("chat").check().is_err());
        drop(preparing);
        assert!(!state.stop_starting("chat"));
        assert!(state.begin_start("chat").check().is_ok());
    }

    #[test]
    fn stopped_start_cannot_register_and_registration_keeps_its_stop_signal() {
        let state = AgentState::default();
        let chat = uuid::Uuid::new_v4().to_string();
        let starting = state.begin_start(&chat);
        assert!(state.stop_starting(&chat));
        assert!(state
            .register_with_stop(&chat, None, "m", None, Some(starting.stop.clone()))
            .is_none());
        drop(starting);
        let starting = state.begin_start(&chat);
        let controls = state
            .register_with_stop(&chat, None, "m", None, Some(starting.stop.clone()))
            .unwrap();
        assert!(state.stop_starting(&chat));
        assert!(controls.stop.is_set());
    }
}
