//! Idempotent trusted Autopilot intake. Creating a task never calls its model.
use super::*;
use rovik_protocol::{autopilot::VerifiedDeliverySettings, harness::ImplementationProvider};

/// Read-only readiness of immutable environment and protected acceptance assets.
pub struct AutopilotReadiness {
    /// Private application controller state root.
    pub state_root: PathBuf,
    /// Exact selected execution workspace.
    pub root: PathBuf,
    /// Registered owning project.
    pub project_id: String,
    /// Frozen setup and configuration identity.
    pub configuration: VerifiedDeliverySettings,
    /// Original cloud source reference, if remote.
    pub baseline_reference: Option<String>,
    /// Authenticated desktop source index, if remote.
    pub tracked_paths: Option<Vec<String>>,
    /// Probe the exact local image without downloading or running candidate code.
    pub check_image: bool,
}

/// Validate prepared assets without installers, test writers, approvals or model calls.
pub async fn validate_autopilot_assets(input: AutopilotReadiness) -> Result<Value> {
    crate::rt::spawn_blocking(move||{
        let mut project=IdeProject::open_for_project(&input.state_root,&input.root,&input.project_id)?;
        check(input.baseline_reference.is_some()==input.tracked_paths.is_some(),"Incomplete source binding.")?;
        project.baseline_reference=input.baseline_reference;
        project.tracked_paths=input.tracked_paths.map(|paths|paths.into_iter().collect());
        let configured=project.configuration()?.ok_or_else(||Error::new("Select the prepared verification setup first.".into()))?;
        check(configured.setup_id.as_str()==input.configuration.setup_id && Digest::of_json(&configured)?.as_str()==input.configuration.configuration_digest,"The task's selected verification setup changed; review a new task.")?;
        let setup=project.load_setup(&configured.setup_id)?;
        let ledger=project.ledger()?;
        setup.validate(&ledger)?;
        check(setup.deadline_ms>runner::now_ms()?,"The prepared task budget expired; review a new setup.")?;
        check(project.integration()?.snapshot(&ledger)?.digest()?==setup.base.digest()?,"The source baseline changed after setup review.")?;
        plan::require_unchanged_protected_inputs(&setup,&project.capture(&input.configuration.untracked.into_iter().collect())?)?;
        check(!setup.oracle.files.is_empty() && !setup.suites.is_empty() && setup.suites.values().all(|cases|!cases.is_empty()) && !setup.negative_control_cases.is_empty(),"Prepared protected tests need an explicit case inventory and negative control.")?;
        let backend=project.backend(&configured.engine)?;
        backend.require_environment(&setup.environment)?;
        if input.check_image {backend.validate_image(&setup.environment)?;}
        Ok(json!({"environmentDigest":Digest::of_json(&setup.environment)?,"oracleDigest":setup.oracle.digest()?,"caseCount":setup.suites.values().map(BTreeSet::len).sum::<usize>(),"imagePresent":input.check_image,"testsExecuted":false}))
    }).await.map_err(|e|Error::new(format!("Prepared-asset validation failed: {e}")))?
}

/// Authenticated execution context and frozen task settings, supplied by the driver.
pub struct AutopilotIntake {
    /// Controller-private application state root.
    pub state_root: PathBuf,
    /// Exact selected execution workspace.
    pub root: PathBuf,
    /// Registered owning project.
    pub project_id: String,
    /// Stable chat/task identity; retries cannot purchase a fresh budget.
    pub task_key: String,
    /// Saved plan on the selected execution host.
    pub plan_path: String,
    /// Setup and prices reviewed before the Autopilot task started.
    pub configuration: VerifiedDeliverySettings,
    /// Ephemeral credentials; only the safe binding is persisted.
    pub provider: ImplementationProvider,
    /// Authenticated cloud source baseline, when present.
    pub baseline_reference: Option<String>,
    /// Authenticated source index; never inferred from a shadow commit.
    pub tracked_paths: Option<Vec<String>>,
    /// Remaining whole-task token authorization after planning and preparation.
    pub tokens_remaining: Option<u64>,
    /// Remaining whole-task cost authorization in microunits.
    pub cost_remaining: Option<u64>,
    /// Whole-task deadline after previously spent time is deducted.
    pub deadline_ms: Option<u64>,
}

/// Validate the selected adapter before any ordinary planning or strict model call.
pub fn validate_implementation_provider(provider: &ImplementationProvider) -> Result<()> {
    plan::binding(provider).map(|_| ())
}

/// Prepare one durable reviewed intake or return the same run after interruption.
/// Concrete approvals remain required; this never starts generation or repairs.
pub async fn ensure_autopilot(input: AutopilotIntake) -> Result<Value> {
    crate::rt::spawn_blocking(move || ensure(input))
        .await
        .map_err(|e| Error::new(format!("Autopilot preparation failed: {e}")))?
}

fn ensure(input: AutopilotIntake) -> Result<Value> {
    let mut project =
        IdeProject::open_for_project(&input.state_root, &input.root, &input.project_id)?;
    check(
        input.baseline_reference.is_some() == input.tracked_paths.is_some(),
        "The trusted source binding is incomplete; inspect this project again.",
    )?;
    project.baseline_reference = input.baseline_reference;
    project.tracked_paths = input.tracked_paths.map(|paths| paths.into_iter().collect());
    project.integration()?;
    let selected = plan::binding(&input.provider)?;
    let key = Digest::of(input.task_key.as_bytes());
    let run_id = format!("ide-auto-{}", key.as_str().trim_start_matches("sha256:"));
    let mut database = project.database()?;
    database.execute_batch("CREATE TABLE IF NOT EXISTS autopilot_intakes(task_key TEXT PRIMARY KEY, run_id TEXT NOT NULL UNIQUE, setup_digest TEXT NOT NULL, input_json TEXT NOT NULL);")?;
    let existing: Option<(String, String)> = database
        .query_row(
            "SELECT setup_digest,input_json FROM autopilot_intakes WHERE task_key=?1",
            [key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let ledger = project.ledger()?;
    let (setup, run_input) = if let Some((setup_digest, json)) = existing {
        let setup: TaskSetup = wire::decode(&ledger.read(&Digest::parse(&setup_digest)?)?)?;
        let persisted: RunInput = wire::decode(json.as_bytes())?;
        project.require_run_context(&persisted)?;
        check(persisted.configuration.setup_id.as_str() == input.configuration.setup_id
            && persisted.configuration_digest.as_str() == input.configuration.configuration_digest
            && persisted.untracked == input.configuration.untracked.iter().cloned().collect()
            && persisted.plan.as_ref().is_some_and(|p| p.provider == selected),
            "The task's reviewed setup or provider changed. Resume its original binding; a new budget requires a new task.")?;
        (setup, persisted)
    } else {
        let configuration = project.configuration()?.ok_or_else(|| Error::new("Prepare and select protected acceptance criteria before starting strict Autopilot.".into()))?;
        check(
            configuration.setup_id.as_str() == input.configuration.setup_id
                && Digest::of_json(&configuration)?.as_str()
                    == input.configuration.configuration_digest,
            "The task's reviewed setup changed before implementation. Review a new Autopilot task.",
        )?;
        let untracked: BTreeSet<String> = input.configuration.untracked.iter().cloned().collect();
        let captured = project.capture(&untracked)?;
        let snapshot_digest = captured.digest()?;
        let saved = project.read_plan(&input.plan_path)?;
        let mut setup =
            project.implementation_setup(&configuration, captured, &saved, &selected)?;
        setup.run_id = run_id.clone();
        setup.plan.run_id = run_id.clone();
        for (unit, cap) in [
            ("tokens", input.tokens_remaining),
            ("cost_microunits", input.cost_remaining),
        ] {
            if let Some(cap) = cap {
                let total = setup.budgets.get_mut(unit).ok_or_else(|| {
                    Error::new(format!("The reviewed task omits its {unit} budget."))
                })?;
                *total = (*total).min(cap);
            }
        }
        if let Some(deadline) = input.deadline_ms {
            setup.deadline_ms = setup.deadline_ms.min(deadline);
        }
        plan::reserve_provider_limits(
            &mut setup.worker,
            &input.configuration.rates,
            &setup.budgets,
        )?;
        if let Some(reviewer) = &mut setup.reviewer {
            plan::reserve_provider_limits(reviewer, &input.configuration.rates, &setup.budgets)?;
        }
        setup.policy.facts.insert(
            "ide.model_limits".into(),
            Digest::of_json(&(&setup.worker, &setup.reviewer, &setup.budgets))?.to_string(),
        );
        setup.policy.facts.insert(
            "ide.autopilot_deadline".into(),
            setup.deadline_ms.to_string(),
        );
        setup.plan.policy_digest = Digest::of_json(&setup.policy)?;
        let persisted = RunInput {
            plan: Some(plan::PlanBinding {
                path: saved.path,
                digest: saved.digest,
                provider: selected.clone(),
            }),
            execution_root: project.root.clone(),
            baseline_reference: project.baseline_reference.clone(),
            tracked_paths: project.tracked_paths.clone(),
            configuration_digest: Digest::of_json(&configuration)?,
            configuration,
            snapshot_digest,
            untracked,
        };
        setup
            .authority
            .authorize(&Principal::current()?, "operate")?;
        setup.validate(&ledger)?;
        let stored = ledger.store_json("operation", &setup, runner::now_ms()?)?;
        // Snapshot publication writes the private index, so perform it before
        // taking this cross-process journal transaction. A concurrent winner's
        // exact immutable intake is reused instead of replacing its budget.
        let transaction =
            database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute("INSERT OR IGNORE INTO autopilot_intakes(task_key,run_id,setup_digest,input_json) VALUES(?1,?2,?3,?4)", params![key.as_str(),run_id,stored.digest.as_str(),serde_json::to_string(&persisted).map_err(|e|Error::new(e.to_string()))?])?;
        let (digest, json): (String, String) = transaction.query_row(
            "SELECT setup_digest,input_json FROM autopilot_intakes WHERE task_key=?1",
            [key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.commit()?;
        (
            wire::decode(&ledger.read(&Digest::parse(&digest)?)?)?,
            wire::decode(json.as_bytes())?,
        )
    };
    check(
        setup.run_id == run_id,
        "The durable Autopilot run binding is inconsistent.",
    )?;
    project.require_run_context(&run_input)?;
    check(run_input.plan.as_ref().is_some_and(|p|p.provider==selected)
        && run_input.configuration.setup_id.as_str()==input.configuration.setup_id
        && run_input.configuration_digest.as_str()==input.configuration.configuration_digest,
        "A concurrent preparation selected different task inputs. Reopen the original reviewed task.")?;
    let transaction =
        database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut controller = project.controller(Some(&run_input.configuration.engine))?;
    // create_run is atomic. A crash after it leaves the same setup and budgets;
    // a retry only fills the private index, never recreates accounting.
    match controller.setup(&run_id) {
        Ok(current) => check(
            Digest::of_json(&current)? == Digest::of_json(&setup)?,
            "The controller's task intake changed unexpectedly.",
        )?,
        Err(_) => {
            controller.create(&setup, runner::now_ms()?)?;
        }
    }
    transaction.execute(
        "INSERT OR IGNORE INTO runs(id,input,created_ms) VALUES(?1,?2,?3)",
        params![
            run_id,
            serde_json::to_string(&run_input).map_err(|e| Error::new(e.to_string()))?,
            i64::try_from(runner::now_ms()?)
                .map_err(|_| Error::new("Task timestamp overflow.".into()))?
        ],
    )?;
    transaction.commit()?;
    project.status(&run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_environment_and_corrupt_protected_oracle_never_report_readiness() {
        let (directory, original) = super::super::tests::project();
        let state_root = directory.path().join("readiness-state");
        let project =
            IdeProject::open_for_project(&state_root, &original.root, "readiness-project").unwrap();
        let missing = directory.path().join("missing-container");
        let configuration = super::super::tests::prepared(
            &project,
            Digest::of(b"image"),
            missing.to_str().unwrap(),
        );
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO configuration(singleton,value) VALUES(1,?1)",
                [serde_json::to_string(&configuration).unwrap()],
            )
            .unwrap();
        let input = || AutopilotReadiness {
            state_root: state_root.clone(),
            root: project.root.clone(),
            project_id: "readiness-project".into(),
            configuration: VerifiedDeliverySettings {
                setup_id: configuration.setup_id.to_string(),
                configuration_digest: Digest::of_json(&configuration).unwrap().to_string(),
                untracked: vec![],
                rates: rovik_protocol::harness::ModelRates {
                    input: 0,
                    cached_input: 0,
                    output: 0,
                },
            },
            baseline_reference: None,
            tracked_paths: None,
            check_image: true,
        };
        let missing_environment = validate_autopilot_assets(input()).await.unwrap_err();
        assert!(
            missing_environment
                .message
                .contains("container client is missing")
                || missing_environment.message.contains("requires Linux"),
            "{missing_environment}"
        );
        let setup = project.load_setup(&configuration.setup_id).unwrap();
        let oracle = setup.oracle.files.values().next().unwrap();
        std::fs::remove_file(
            project
                .state
                .join("artifacts")
                .join(oracle.content.digest.hex()),
        )
        .unwrap();
        let missing_oracle = validate_autopilot_assets(input()).await.unwrap_err();
        assert_ne!(
            missing_oracle.message, missing_environment.message,
            "oracle integrity must fail before environment readiness"
        );
    }

    #[test]
    fn journal_replays_the_same_intake_after_index_loss_without_resetting_spent_budget() {
        use rovik_harness::{
            context::ContextBundle,
            provider::{ModelRequest, ModelResponse, Role, Usage, WorkerTurn},
        };

        let (directory, original) = super::super::tests::project();
        let state_root = directory.path().join("autopilot-state");
        let project = IdeProject::open_for_project(
            &state_root,
            &original.root,
            "registered-autopilot-project",
        )
        .unwrap();
        let image = Digest::of(b"pinned-image");
        #[cfg(target_os = "linux")]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            let executable = directory.path().join("inspection-only-client");
            // Only image metadata is scripted. This fixture cannot create a
            // container, run a checker, or produce accepted execution evidence.
            std::fs::write(
                &executable,
                format!(
                    r#"#!/bin/sh
set -eu
if [ "$#" -ne 6 ] ||
   [ "$1" != '--host=unix:///var/run/docker.sock' ] ||
   [ "$2" != image ] ||
   [ "$3" != inspect ] ||
   [ "$4" != --format ] ||
   [ "$5" != '{{{{.Id}}}}' ] ||
   [ "$6" != '{image}' ]; then
    printf '%s\n' 'This fixture permits only pinned image inspection.' >&2
    exit 64
fi
printf '%s\n' inspected >> "$0.calls"
printf '%s\n' '{image}'
"#
                ),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            executable
        };
        #[cfg(not(target_os = "linux"))]
        let executable = std::env::current_exe().unwrap();
        let mut configuration =
            super::super::tests::prepared(&project, image, executable.to_str().unwrap());
        let mut prepared = project.load_setup(&configuration.setup_id).unwrap();
        prepared.budgets.insert("tokens".into(), 256_000);
        configuration.setup_id = Digest::parse(
            project.register_setup(prepared).unwrap()["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let configured = project.dispatch(Operation::Configure {
            setup_id: configuration.setup_id.to_string(),
            engine: configuration.engine.clone(),
            expected_snapshot_digest: configuration.snapshot_digest.to_string(),
        });
        if !cfg!(target_os = "linux") {
            assert!(
                configured.unwrap_err().message.contains("requires Linux"),
                "unsupported hosts must reject authenticated execution"
            );
            return;
        }
        configured.unwrap();
        std::fs::write(
            project.root.join("plan.md"),
            "# Plan\nRepair the exact-capacity boundary.\n",
        )
        .unwrap();
        let provider:ImplementationProvider=serde_json::from_value(json!({"profile":{"id":"fixture","label":"Fixture","model":"fixture-scripted","baseUrl":"http://localhost:8000/v1","apiKey":"never-store-this-key"},"rates":{"input":0,"cachedInput":0,"output":0}})).unwrap();
        let settings = VerifiedDeliverySettings {
            setup_id: configuration.setup_id.to_string(),
            configuration_digest: Digest::of_json(&configuration).unwrap().to_string(),
            untracked: vec![],
            rates: provider.rates.clone(),
        };
        let input = || AutopilotIntake {
            state_root: state_root.clone(),
            root: project.root.clone(),
            project_id: "registered-autopilot-project".into(),
            task_key: "chat/task".into(),
            plan_path: "plan.md".into(),
            configuration: settings.clone(),
            provider: provider.clone(),
            baseline_reference: None,
            tracked_paths: None,
            tokens_remaining: Some(200_000),
            cost_remaining: None,
            deadline_ms: None,
        };
        let first = ensure(input()).unwrap();
        assert_eq!(first["status"], "blocked");
        let run = first["runId"].as_str().unwrap();
        assert_eq!(first["budgets"]["tokens"]["limit"], 200_000);
        assert!(!first["pendingApprovals"].as_array().unwrap().is_empty());
        assert!(first["candidateDigest"].is_null());
        assert!(first["checks"].as_array().unwrap().is_empty());
        let mut ledger = project.ledger().unwrap();
        let now = runner::now_ms().unwrap();
        let model_request = ModelRequest {
            role: Role::Implementer,
            request_id: "paid-attempt".into(),
            provider_model_id: provider.profile.model.clone(),
            context: ContextBundle {
                schema_version: 1,
                items: vec![],
                omitted: vec![],
            },
            snapshot_digest: Digest::parse(first["snapshotDigest"].as_str().unwrap()).unwrap(),
            maximum_output_tokens: 1,
            timeout_ms: 60_000,
            feedback: vec![],
        };
        let request = ledger.store_json("context", &model_request, now).unwrap();
        let lease = ledger
            .claim(
                run,
                &model_request.request_id,
                "model",
                &request.digest,
                "test-controller",
                now,
                60_000,
                &[
                    ("model_calls".into(), 1),
                    ("tokens".into(), 10),
                    ("cost_microunits".into(), 0),
                ]
                .into(),
            )
            .unwrap();
        // A question-only model response records usage without a candidate,
        // native check receipt, approval, or acceptance authority.
        let turn = WorkerTurn {
            schema_version: 1,
            request_id: model_request.request_id.clone(),
            tool_requests: vec![],
            proposal: None,
            questions: vec!["Which reviewed domain boundary applies?".into()],
        };
        turn.validate(&model_request).unwrap();
        let receipt = ledger
            .store_json(
                "provider-response",
                &ModelResponse {
                    request_id: model_request.request_id.clone(),
                    provider_response_id: Some("accounting-fixture".into()),
                    output: serde_json::to_string(&turn).unwrap(),
                    finish_reason: "stop".into(),
                    usage: Some(Usage {
                        input_tokens: 2,
                        output_tokens: 1,
                        cached_tokens: Some(0),
                        cost_microunits: Some(0),
                    }),
                },
                now,
            )
            .unwrap();
        assert!(ledger
            .complete(
                &lease,
                &receipt.digest,
                &[
                    ("model_calls".into(), 1),
                    ("tokens".into(), 3),
                    ("cost_microunits".into(), 0),
                ]
                .into(),
                now,
            )
            .unwrap());
        let accounted = ledger.budgets(run).unwrap();
        assert_eq!(accounted["tokens"].limit, 200_000);
        assert_eq!(accounted["tokens"].spent, 3);
        assert_eq!(accounted["tokens"].reserved, 0);
        assert_eq!(accounted["model_calls"].spent, 1);
        // The controller was created, but a process died before its IDE index row.
        project
            .database()
            .unwrap()
            .execute("DELETE FROM runs WHERE id=?1", [run])
            .unwrap();
        std::fs::write(
            project.root.join("plan.md"),
            "A later edit cannot buy a fresh budget.\n",
        )
        .unwrap();
        let mut again = input();
        again.provider.profile.api_key = "rotated-credential".into();
        again.tokens_remaining = Some(1);
        let resumed = ensure(again).unwrap();
        assert_eq!(resumed["runId"], first["runId"]);
        assert_eq!(resumed["status"], "stale");
        assert_eq!(resumed["budgets"]["tokens"]["limit"], 200_000);
        assert_eq!(resumed["budgets"]["tokens"]["spent"], 3);
        assert!(resumed["candidateDigest"].is_null());
        assert!(resumed["checks"].as_array().unwrap().is_empty());
        assert_eq!(ledger.budgets(run).unwrap(), accounted);
        let stored: String = project
            .database()
            .unwrap()
            .query_row("SELECT input_json FROM autopilot_intakes", [], |r| r.get(0))
            .unwrap();
        assert!(!stored.contains("never-store-this-key") && !stored.contains("rotated-credential"));
        let mut changed = input();
        changed.provider.profile.model = "another-model".into();
        assert!(ensure(changed)
            .unwrap_err()
            .message
            .contains("provider changed"));
        #[cfg(target_os = "linux")]
        assert_eq!(
            std::fs::read_to_string(executable.with_extension("calls")).unwrap(),
            "inspected\n",
            "only Configure's pinned image preflight may reach the container client"
        );
    }
}
