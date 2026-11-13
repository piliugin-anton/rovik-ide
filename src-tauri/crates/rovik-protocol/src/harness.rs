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

/// Whether this user message requests a new verified Autopilot task.
/// Explicit message mode wins over a host's default; internal stage prompts are excluded.
pub fn starts_verified_autopilot(
    request: &crate::agent::StartRequest,
    configuration: &crate::autopilot::AutopilotConfig,
) -> bool {
    if !configuration.require_verified_delivery {
        return false;
    }
    let crate::agent::StartKind::Send { message } = &request.start else {
        return false;
    };
    if message
        .meta
        .as_ref()
        .is_some_and(|meta| meta.get("harness").is_some())
    {
        return false;
    }
    message
        .meta
        .as_ref()
        .and_then(|meta| meta.get("mode"))
        .and_then(Value::as_str)
        .or(request.mode.as_deref())
        == Some("autopilot")
}

/// Explicit billing rates in microunits per million tokens, including deliberate zero rates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRates {
    /// Uncached input rate.
    pub input: u64,
    /// Cached input rate.
    pub cached_input: u64,
    /// Output rate.
    pub output: u64,
}

/// Ephemeral selected IDE provider. Credentials are never persisted with runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImplementationProvider {
    /// Existing IDE model selection; Debug redacts its API key.
    pub profile: crate::agent::ModelProfile,
    /// Explicit reviewed billing rates.
    pub rates: ModelRates,
}

/// An advisory preparation role, with no approval or candidate-write capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftRole {
    /// Propose domain requirements.
    Domain,
    /// Propose responsibility and reuse decisions.
    Architect,
    /// Propose protected acceptance tests.
    Oracle,
}

/// Preserve the effective strict mode in the durable user message before task creation.
pub fn normalize_verified_autopilot(
    request: &mut crate::agent::StartRequest,
    configuration: &crate::autopilot::AutopilotConfig,
) -> Result<bool, String> {
    let starts = starts_verified_autopilot(request, configuration);
    if starts {
        if let crate::agent::StartKind::Send { message } = &mut request.start {
            let meta = message.meta.get_or_insert_with(|| serde_json::json!({}));
            let fields = meta
                .as_object_mut()
                .ok_or("The task's message metadata must be an object.")?;
            if fields.get("mode").is_some_and(|mode| !mode.is_string()) {
                return Err("The task's message mode must be a string.".into());
            }
            fields
                .entry("mode")
                .or_insert_with(|| serde_json::json!("autopilot"));
        }
    }
    Ok(starts)
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
    /// Prepare the exact accepted patch and destination for separate delivery approval.
    PrepareApplication {
        /// Durable accepted controller run.
        run_id: String,
    },
    /// Apply only the authenticated, reviewed candidate to its bound execution workspace.
    ApplyApplication {
        /// Durable accepted controller run.
        run_id: String,
        /// Exact prepared application artifact reviewed by the operator.
        subject_digest: String,
        /// The IDE saved all editor buffers before requesting destination writes.
        saved_buffers: bool,
    },
    /// Reconcile an interrupted private application journal under the same approval.
    RecoverApplication {
        /// Durable accepted controller run.
        run_id: String,
        /// Exact prepared application artifact.
        subject_digest: String,
        /// The IDE saved all editor buffers before recovering destination writes.
        saved_buffers: bool,
    },
    /// Explicitly allow later host synchronization after the operator preserves applied changes.
    ReleaseApplication {
        /// Run whose host output is protected.
        run_id: String,
        /// Exact application receipt being acknowledged.
        subject_digest: String,
        /// Operator confirms the host changes were preserved or manually integrated.
        acknowledged: bool,
    },
    /// Transport-only query: applied or interrupted output must not be overwritten by synchronization.
    ApplicationState,
    /// Read the exact saved plan on the execution host for review.
    InspectPlan {
        /// In-project saved plan path.
        plan_path: String,
    },
    /// Build a prepared setup from reviewed guided configuration.
    PrepareWorkspace {
        /// Closed WorkspaceSetupInput decoded by the host builder.
        input: Value,
    },
    /// Register a verified implementation; exact decisions require approval before execution.
    ImplementPlan {
        /// In-project saved plan path.
        plan_path: String,
        /// Exact plan bytes the operator reviewed.
        expected_plan_digest: String,
        /// Exact source tree the operator reviewed.
        expected_snapshot_digest: String,
        /// Selected untracked source files.
        #[serde(default)]
        untracked: Vec<String>,
        /// All selected editor buffers have been saved.
        saved_buffers: bool,
        /// Operator confirmed that protected criteria cover this plan.
        plan_criteria_reviewed: bool,
        /// Ephemeral selected model and explicit billing rates.
        provider: ImplementationProvider,
    },
    /// Resume implementation with credentials supplied again, preserving its model binding and budget.
    ResumeImplementation {
        /// Durable controller run identity.
        run_id: String,
        /// Same provider/model/rates; a rotated API key is allowed.
        provider: ImplementationProvider,
    },
    /// Generate a bounded advisory draft against the run's exact plan and source.
    DraftPlan {
        /// Durable implementation intake paying for the draft.
        run_id: String,
        /// Advisory role to invoke.
        role: DraftRole,
        /// Ephemeral selected provider, matching the registered run binding.
        provider: ImplementationProvider,
    },
    /// Review the exact candidate changes without applying them to the checkout.
    ReviewChanges {
        /// Durable controller run identity.
        run_id: String,
    },
    /// Export an immutable review bundle into controller-private storage.
    ExportReview {
        /// Durable controller run identity.
        run_id: String,
    },
    /// Prepare an unreferenced accepted commit; never update a checkout or branch.
    PrepareDelivery {
        /// Durable accepted run identity.
        run_id: String,
        /// Explicit destination branch.
        destination_reference: String,
        /// Expected destination commit.
        expected_head: String,
        /// Reviewed commit message.
        message: String,
    },
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

impl Operation {
    /// Whether this action must observe current synchronized source bytes and index selection.
    /// Both desktop routing and the daemon use this classification before capturing inputs.
    pub fn requires_current_source(&self) -> bool {
        match self {
            Self::PrepareApplication { .. }
            | Self::ApplyApplication { .. }
            | Self::InspectPlan { .. }
            | Self::PrepareWorkspace { .. }
            | Self::ImplementPlan { .. }
            | Self::ResumeImplementation { .. }
            | Self::DraftPlan { .. }
            | Self::ReviewChanges { .. }
            | Self::ExportReview { .. }
            | Self::PrepareDelivery { .. }
            | Self::Inspect { .. }
            | Self::Configure { .. }
            | Self::Verify { .. }
            | Self::Status { .. }
            | Self::Resume { .. } => true,
            Self::ApplicationState
            | Self::ReleaseApplication { .. }
            | Self::RecoverApplication { .. }
            | Self::ImportSetup { .. }
            | Self::Approvals { .. }
            | Self::Approve { .. }
            | Self::Cancel { .. }
            | Self::Recover { .. } => false,
        }
    }

    /// Actions allowed to observe or reconcile host output while desktop synchronization is blocked.
    pub fn observes_applied_workspace(&self) -> bool {
        matches!(
            self,
            Self::Status { .. }
                | Self::Inspect { .. }
                | Self::Approvals { .. }
                | Self::ReviewChanges { .. }
                | Self::ExportReview { .. }
                | Self::PrepareApplication { .. }
                | Self::ApplyApplication { .. }
                | Self::RecoverApplication { .. }
        )
    }

    /// Capture and recovery actions serialize with ordinary execution-workspace writes.
    pub fn requires_workspace_lock(&self) -> bool {
        self.requires_current_source() || matches!(self, Self::RecoverApplication { .. })
    }

    /// The host capability required before serializing this action or forwarding model credentials.
    pub fn required_feature(&self) -> &'static str {
        match self {
            Self::PrepareApplication { .. }
            | Self::ApplyApplication { .. }
            | Self::RecoverApplication { .. }
            | Self::ReleaseApplication { .. }
            | Self::ApplicationState => crate::features::POLYGLOT_APPLICATION,
            Self::InspectPlan { .. }
            | Self::PrepareWorkspace { .. }
            | Self::ImplementPlan { .. }
            | Self::ResumeImplementation { .. }
            | Self::DraftPlan { .. }
            | Self::ReviewChanges { .. }
            | Self::ExportReview { .. }
            | Self::PrepareDelivery { .. } => crate::features::POLYGLOT_IMPLEMENTATION,
            Self::Inspect { .. }
            | Self::ImportSetup { .. }
            | Self::Configure { .. }
            | Self::Verify { .. }
            | Self::Status { .. }
            | Self::Approvals { .. }
            | Self::Approve { .. }
            | Self::Cancel { .. }
            | Self::Resume { .. }
            | Self::Recover { .. } => crate::features::POLYGLOT_HARNESS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn implementation_provider() -> Value {
        serde_json::json!({
            "profile":{"id":"provider::model", "label":"Model", "model":"model",
                "baseUrl":"https://model.example/v1", "apiKey":"ephemeral-test-key"},
            "rates":{"input":1,"cachedInput":0,"output":2}
        })
    }

    #[test]
    fn every_implementation_operation_requires_current_source_and_a_capable_host() {
        let provider = implementation_provider();
        let requests = [
            serde_json::json!({"action":"inspectPlan","planPath":"plans/change.md"}),
            serde_json::json!({"action":"prepareWorkspace","input":{}}),
            serde_json::json!({"action":"implementPlan","planPath":"plans/change.md", "expectedPlanDigest":"plan", "expectedSnapshotDigest":"source", "untracked":[], "savedBuffers":true,"planCriteriaReviewed":true,"provider":provider}),
            serde_json::json!({"action":"resumeImplementation","runId":"run","provider":provider}),
            serde_json::json!({"action":"draftPlan","runId":"run","role":"architect","provider":provider}),
            serde_json::json!({"action":"reviewChanges","runId":"run"}),
            serde_json::json!({"action":"exportReview","runId":"run"}),
            serde_json::json!({"action":"prepareDelivery","runId":"run", "destinationReference":"refs/heads/main", "expectedHead":"head", "message":"Reviewed changes"}),
        ];
        for request in requests {
            let operation: Operation = serde_json::from_value(request).unwrap();
            assert!(operation.requires_current_source(), "{operation:?}");
            assert_eq!(
                operation.required_feature(),
                crate::features::POLYGLOT_IMPLEMENTATION
            );
            let encoded = serde_json::to_vec(&operation).unwrap();
            assert_eq!(
                serde_json::from_slice::<Operation>(&encoded).unwrap(),
                operation
            );
            assert!(!format!("{operation:?}").contains("ephemeral-test-key"));
        }
    }

    #[test]
    fn reviewed_application_requires_its_own_capability_and_explicit_write_confirmation() {
        let requests = [
            serde_json::json!({"action":"prepareApplication","runId":"run"}),
            serde_json::json!({"action":"applyApplication","runId":"run","subjectDigest":"subject","savedBuffers":true}),
            serde_json::json!({"action":"recoverApplication","runId":"run","subjectDigest":"subject","savedBuffers":true}),
            serde_json::json!({"action":"releaseApplication","runId":"run","subjectDigest":"subject","acknowledged":true}),
            serde_json::json!({"action":"applicationState"}),
        ];
        for request in requests {
            let operation: Operation = serde_json::from_value(request.clone()).unwrap();
            assert_eq!(
                operation.required_feature(),
                crate::features::POLYGLOT_APPLICATION
            );
            if let Some(field) = if request.get("savedBuffers").is_some() {
                Some("savedBuffers")
            } else if request.get("acknowledged").is_some() {
                Some("acknowledged")
            } else {
                None
            } {
                let mut missing = request;
                missing.as_object_mut().unwrap().remove(field);
                assert!(serde_json::from_value::<Operation>(missing).is_err());
            }
        }
        assert!(Operation::RecoverApplication {
            run_id: "run".into(),
            subject_digest: "subject".into(),
            saved_buffers: true
        }
        .requires_workspace_lock());
        assert!(!Operation::RecoverApplication {
            run_id: "run".into(),
            subject_digest: "subject".into(),
            saved_buffers: true
        }
        .requires_current_source());
    }

    #[test]
    fn private_job_cancellation_and_exact_decisions_do_not_require_current_source() {
        let operations = [
            Operation::Cancel {
                run_id: "run".into(),
            },
            Operation::Recover {
                run_id: "run".into(),
            },
            Operation::Approvals {
                run_id: "run".into(),
            },
            Operation::Approve {
                run_id: "run".into(),
                request: Value::Null,
            },
        ];
        for operation in operations {
            assert!(!operation.requires_current_source());
            assert_eq!(
                operation.required_feature(),
                crate::features::POLYGLOT_HARNESS
            );
        }
        assert!(Operation::Status {
            run_id: "run".into()
        }
        .requires_current_source());
    }

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
