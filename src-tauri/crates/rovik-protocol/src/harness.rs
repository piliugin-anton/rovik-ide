//! Trusted IDE operations for the polyglot verification controller.
//!
//! Setup and approval documents are decoded against the controller's closed
//! schemas on the execution host. No operation accepts a verdict or state path.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A supported container implementation on the selected Linux execution host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    /// A local Docker daemon.
    Docker,
    /// Local rootless Podman with delegated cgroups.
    Podman,
}

/// An operator-selected executable; the controller checks host capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Engine {
    /// Container implementation.
    pub kind: EngineKind,
    /// Absolute executable path on the execution host.
    pub executable: String,
}

/// Desktop-authenticated source baseline and the exact shadow snapshot sent to a host.
/// This binding is derived by desktop routing, never supplied by the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceBinding {
    /// The actual project HEAD before synchronization, not the shadow commit.
    pub head: String,
    /// The synchronized candidate commit whose first parent is `head`.
    pub snapshot: String,
    /// Actual desktop index paths, preserving the distinction from untracked files.
    #[serde(default)]
    pub tracked_paths: Vec<String>,
}

/// User management actions, never exposed through the worker tool broker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Operation {
    /// Discover project inputs and reusable, reviewed configuration.
    Inspect {
        /// Explicitly selected untracked files to include in the snapshot.
        #[serde(default)]
        untracked: Vec<String>,
    },
    /// Import a prepared setup bundle without importing approvals or evidence.
    ImportSetup {
        /// Setup file on the selected execution host.
        path: String,
    },
    /// Select a prepared setup and execution engine for the reviewed tree.
    Configure {
        /// Registered setup identity returned by inspection.
        setup_id: String,
        /// Operator-selected local container client.
        engine: Engine,
        /// Snapshot the operator reviewed; concurrent edits invalidate selection.
        expected_snapshot_digest: String,
    },
    /// Start fresh, model-free verification of the exact current candidate.
    Verify {
        /// Explicit untracked-file selection.
        #[serde(default)]
        untracked: Vec<String>,
        /// Snapshot the user selected for verification.
        expected_snapshot_digest: String,
        /// The editor confirms it has saved all included buffers.
        saved_buffers: bool,
    },
    /// Read durable state and independently computed evidence freshness.
    Status {
        /// Controller-issued run identity.
        run_id: String,
    },
    /// Read the exact decisions still needed by a run.
    Approvals {
        /// Controller-issued run identity.
        run_id: String,
    },
    /// Record a concrete decision as the authenticated execution-host operator.
    Approve {
        /// Controller-issued run identity.
        run_id: String,
        /// Closed approval request; identity is supplied only by the host.
        request: Value,
    },
    /// Stop the controller's registered work.
    Cancel {
        /// Controller-issued run identity.
        run_id: String,
    },
    /// Continue a blocked or interrupted verification after its blockers clear.
    Resume {
        /// Controller-issued run identity.
        run_id: String,
    },
    /// Reconcile interrupted execution using registered host jobs and receipts.
    Recover {
        /// Controller-issued run identity.
        run_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_requires_an_explicit_saved_snapshot_and_rejects_authority_injection() {
        let request = serde_json::json!({
            "action":"verify", "expectedSnapshotDigest":"sha256:reviewed", "savedBuffers":true
        });
        assert!(serde_json::from_value::<Operation>(request.clone()).is_ok());
        for field in ["savedBuffers", "expectedSnapshotDigest"] {
            let mut missing = request.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Operation>(missing).is_err());
        }
        for (field, value) in [
            ("verdict", "ready"),
            ("stateRoot", "/project/state"),
            ("actorId", "root"),
        ] {
            let mut forged = request.clone();
            forged[field] = Value::String(value.into());
            assert!(serde_json::from_value::<Operation>(forged).is_err());
        }
    }
}
