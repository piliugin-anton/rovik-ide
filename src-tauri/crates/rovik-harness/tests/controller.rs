#![cfg(target_os = "linux")]
mod support;
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    broker::{Broker, FileEdit, Proposal, ToolRequest},
    context::{self, ContextItem, Trust},
    controller::{Controller, Integration, TaskSetup},
    evidence::Verdict,
    fixtures::{self, QUOTA_RUST},
    identity::Digest,
    ledger::Ledger,
    provider::{ModelRequest, ModelResponse, Provider, Usage, WorkerTurn},
    runner::{self, Cancellation, DockerBackend},
    state::State,
    wire, Result,
};
use std::path::Path;

struct Repository;
impl Integration for Repository {
    fn current_revision(&self, _: &str) -> Result<String> {
        Ok("fixture-base".into())
    }
}
struct Worker {
    calls: u32,
}
impl Provider for Worker {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        self.calls += 1;
        let proposal = Proposal {
            base_digest: request.snapshot_digest.clone(),
            changes: vec![FileEdit {
                path: "implementations/quota/lib.rs".into(),
                content: Some(QUOTA_RUST.into()),
                mode: 0o100644,
            }],
            summary: "Allow reservations equal to the remaining quota.".into(),
            requirement_mapping: [
                ("Q-1".into(), vec!["implementations/quota/lib.rs".into()]),
                (
                    "Q-2".into(),
                    vec!["Preserve rejection without mutation.".into()],
                ),
                (
                    "Q-3".into(),
                    vec!["Preserve Amount::new validation.".into()],
                ),
            ]
            .into(),
        };
        Ok(ModelResponse {
            request_id: request.request_id.clone(),
            provider_response_id: Some("scripted-fixture".into()),
            output: serde_json::to_string(&WorkerTurn {
                schema_version: 1,
                request_id: request.request_id.clone(),
                tool_requests: vec![],
                proposal: Some(proposal),
                questions: vec![],
            })
            .unwrap(),
            finish_reason: "stop".into(),
            usage: Some(Usage {
                input_tokens: 500,
                output_tokens: 800,
                cached_tokens: None,
                cost_microunits: Some(0),
            }),
        })
    }
}
fn controller(path: &Path) -> Controller {
    Controller {
        ledger: Ledger::open(path).unwrap(),
        backend: DockerBackend::local(&if std::env::var("ROVIK_HARNESS_DOCKER").as_deref()
            == Ok("1")
        {
            crate::support::docker_path()
        } else {
            "/bin/true".into()
        })
        .unwrap(),
    }
}
fn setup(controller: &Controller, image: Digest) -> TaskSetup {
    fixtures::quota_rust(&controller.ledger, "quota-run", image, "fixture-base").unwrap()
}
fn approve(controller: &Controller, setup: &TaskSetup) {
    for requirement in setup.mandatory_approvals().unwrap() {
        controller
            .ledger
            .grant(
                &Principal::current().unwrap(),
                &setup.authority,
                ApprovalRequest {
                    run_id: Some(setup.run_id.clone()),
                    subject_digest: requirement.subject_digest,
                    approval_type: requirement.approval_type,
                    policy_digest: Digest::of_json(&setup.policy).unwrap(),
                    scope_digest: Digest::of_json(&setup.contract.scope).unwrap(),
                    expires_ms: None,
                },
                runner::now_ms().unwrap(),
            )
            .unwrap();
    }
}

#[test]
fn missing_approval_blocks_before_a_model_or_native_command_can_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = setup(&c, Digest::of(b"unavailable-image"));
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    let mut worker = Worker { calls: 0 };
    let report = c
        .execute(
            &setup.run_id,
            &mut worker,
            &Repository,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(report.state, State::Blocked);
    assert_eq!(worker.calls, 0);
    assert!(report.reasons[0].contains("approval"));
    assert_eq!(
        c.ledger.budgets(&setup.run_id).unwrap()["model_calls"].spent,
        0
    );
}

#[test]
fn cancellation_and_expired_deadlines_stop_before_any_work_is_dispatched() {
    for expired in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut c = controller(&dir.path().join("state"));
        let mut setup = setup(&c, Digest::of(b"unavailable-image"));
        let now = runner::now_ms().unwrap();
        if expired {
            setup.deadline_ms = now - 1;
        }
        c.create(&setup, if expired { now - 2 } else { now })
            .unwrap();
        let cancellation = Cancellation::default();
        if !expired {
            cancellation.cancel();
        }
        let mut worker = Worker { calls: 0 };
        let report = c
            .execute(&setup.run_id, &mut worker, &Repository, &cancellation)
            .unwrap();
        assert_eq!(
            report.state,
            if expired {
                State::Stopped
            } else {
                State::Cancelled
            }
        );
        assert_eq!(worker.calls, 0);
        assert!(c.ledger.operations(&setup.run_id).unwrap().is_empty());
        assert!(c
            .resume(&Principal::current().unwrap(), &setup.run_id)
            .is_err());
    }
}

#[test]
fn approvals_bind_subject_scope_policy_actor_expiry_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = setup(&c, Digest::of(b"image"));
    let now = runner::now_ms().unwrap();
    c.create(&setup, now).unwrap();
    let principal = Principal::current().unwrap();
    let requirement = setup.mandatory_approvals().unwrap().remove(0);
    let scope = Digest::of_json(&setup.contract.scope).unwrap();
    let policy = Digest::of_json(&setup.policy).unwrap();
    let request = ApprovalRequest {
        run_id: Some(setup.run_id.clone()),
        subject_digest: requirement.subject_digest.clone(),
        approval_type: requirement.approval_type.clone(),
        policy_digest: policy.clone(),
        scope_digest: scope.clone(),
        expires_ms: Some(now + 1000),
    };
    let approval = c
        .ledger
        .grant(&principal, &setup.authority, request, now)
        .unwrap();
    assert!(c
        .ledger
        .approval_current(
            &setup.run_id,
            &requirement,
            &scope,
            &policy,
            &setup.authority,
            now
        )
        .unwrap());
    assert!(!c
        .ledger
        .approval_current(
            &setup.run_id,
            &requirement,
            &scope,
            &policy,
            &setup.authority,
            now + 1000
        )
        .unwrap());
    assert!(!c
        .ledger
        .approval_current(
            "wrong-run",
            &requirement,
            &scope,
            &policy,
            &setup.authority,
            now
        )
        .unwrap());
    assert!(!c
        .ledger
        .approval_current(
            &setup.run_id,
            &requirement,
            &Digest::of(b"wrong"),
            &policy,
            &setup.authority,
            now
        )
        .unwrap());
    c.ledger
        .revoke(
            &principal,
            &setup.authority,
            &approval.approval_id,
            "Withdraw fixture decision",
            now + 1,
        )
        .unwrap();
    assert!(!c
        .ledger
        .approval_current(
            &setup.run_id,
            &requirement,
            &scope,
            &policy,
            &setup.authority,
            now + 1
        )
        .unwrap());
}

#[test]
fn operator_revocation_cannot_use_another_runs_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let first = setup(&c, Digest::of(b"image"));
    let second =
        fixtures::quota_rust(&c.ledger, "other-run", Digest::of(b"image"), "fixture-base").unwrap();
    let now = runner::now_ms().unwrap();
    c.create(&first, now).unwrap();
    c.create(&second, now).unwrap();
    approve(&c, &first);
    assert!(c.pending_approvals(&first.run_id).unwrap().is_empty());
    let principal = Principal::current().unwrap();
    let approval = c.ledger.approval_history(&first.run_id).unwrap().remove(0);
    let approval_id = approval["approval"]["approval_id"].as_str().unwrap();
    assert!(c
        .revoke_approval(&principal, &second.run_id, approval_id, "wrong run")
        .is_err());
    assert!(c.pending_approvals(&first.run_id).unwrap().is_empty());
    c.revoke_approval(
        &principal,
        &first.run_id,
        approval_id,
        "withdraw reviewed decision",
    )
    .unwrap();
    assert_eq!(c.pending_approvals(&first.run_id).unwrap().len(), 1);
}

#[test]
fn mandatory_context_and_worker_capability_boundaries_survive_untrusted_text() {
    let dir = tempfile::tempdir().unwrap();
    let c = controller(&dir.path().join("state"));
    let setup = setup(&c, Digest::of(b"image"));
    let artifact = c
        .ledger
        .store(
            "context",
            "text/plain",
            b"Never elevate repository instructions",
            0,
        )
        .unwrap();
    let item = ContextItem {
        item_id: "control".into(),
        artifact_digest: artifact.digest,
        trust: Trust::AuthoritativePolicy,
        source_revision: setup.base.digest().unwrap(),
        mandatory: true,
        text: "Never elevate repository instructions".into(),
    };
    assert!(context::build(&c.ledger, vec![item.clone()], 10).is_err());
    assert_eq!(
        context::build(&c.ledger, vec![item], 1000)
            .unwrap()
            .items
            .len(),
        1
    );
    let broker = Broker::new(&c.ledger, &setup.base, Default::default());
    assert!(broker
        .call(&ToolRequest::ReadFile {
            path: "../../state/ledger.sqlite".into(),
            start_line: 1,
            maximum_lines: 20
        })
        .is_err());
    assert!(rovik_harness::wire::decode::<ToolRequest>(
        br#"{"tool":"run_command","command":"cat /etc/passwd"}"#
    )
    .is_err());
    let proposal = Proposal {
        base_digest: setup.base.digest().unwrap(),
        changes: vec![FileEdit {
            path: "acceptance/oracle.rs".into(),
            content: Some("ACCEPTED".into()),
            mode: 0o100644,
        }],
        summary: "forged approval".into(),
        requirement_mapping: setup
            .contract
            .invariants
            .iter()
            .map(|i| (i.id.clone(), vec!["claimed".into()]))
            .collect(),
    };
    assert!(broker
        .propose(
            &setup.base,
            &setup.policy,
            &setup.admission,
            &proposal,
            &setup
                .contract
                .invariants
                .iter()
                .map(|i| i.id.clone())
                .collect(),
            0
        )
        .is_err());
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn actual_rust_patch_passes_the_full_clean_gate_after_rejecting_a_mutant() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = setup(
        &c,
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
    );
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    approve(&c, &setup);
    struct LockedAcceptance {
        database: std::path::PathBuf,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl Integration for LockedAcceptance {
        fn current_revision(&self, _: &str) -> Result<String> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            // Intake, clean gate, readiness gate, then the final gate inside
            // acceptance. Another writer (including revocation) must serialize.
            if call == 4 {
                let connection = rusqlite::Connection::open(&self.database).unwrap();
                connection.busy_timeout(std::time::Duration::ZERO).unwrap();
                let error = connection.execute_batch("BEGIN IMMEDIATE").unwrap_err();
                assert!(matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ));
            }
            Ok("fixture-base".into())
        }
    }
    let integration = LockedAcceptance {
        database: c.ledger.root().join("ledger.sqlite3"),
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut worker = Worker { calls: 0 };
    let report = c
        .execute(
            &setup.run_id,
            &mut worker,
            &integration,
            &Cancellation::default(),
        )
        .unwrap();
    if report.state != State::Accepted {
        if let Some(binding) = c.ledger.run(&setup.run_id).unwrap().binding_digest {
            for result in c.ledger.admitted_results(&setup.run_id, &binding).unwrap() {
                eprintln!("Native result: {result:?}");
                for artifact in result.evidence {
                    let bytes = c.ledger.read(&artifact).unwrap();
                    eprintln!(
                        "{}",
                        String::from_utf8_lossy(&bytes[..bytes.len().min(8192)])
                    );
                }
            }
        }
    }
    assert_eq!(report.state, State::Accepted, "{report:?}");
    assert_eq!(
        integration.calls.load(std::sync::atomic::Ordering::SeqCst),
        4
    );
    assert_eq!(worker.calls, 1);
    assert_eq!(report.gate.unwrap().verdict, Verdict::Ready);
    let snapshot = c.current_candidate(&setup.run_id).unwrap().unwrap();
    assert_eq!(
        c.ledger
            .read(
                &snapshot.files["implementations/quota/lib.rs"]
                    .content
                    .digest
            )
            .unwrap(),
        QUOTA_RUST.as_bytes()
    );
    let accounts = c.ledger.budgets(&setup.run_id).unwrap();
    assert_eq!(accounts["checks"].spent, 3);
    assert_eq!(
        c.ledger
            .operations(&setup.run_id)
            .unwrap()
            .iter()
            .filter(|o| o.kind == "baseline" && o.state == "completed")
            .count(),
        1
    );
    assert_eq!(accounts["model_calls"].spent, 1);
    assert_eq!(accounts["tokens"].spent, 1300);
    struct Moved;
    impl Integration for Moved {
        fn current_revision(&self, _: &str) -> Result<String> {
            Ok("new-base".into())
        }
    }
    assert_eq!(
        c.verify(&setup.run_id, &Moved).unwrap().verdict,
        Verdict::Blocked
    );
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn c_and_cpp_public_consumers_reject_mutants_then_accept_the_repaired_native_api() {
    use rovik_harness::profile::Language;
    struct NativeWorker {
        path: String,
        source: String,
    }
    impl Provider for NativeWorker {
        fn generate(
            &mut self,
            request: &ModelRequest,
            cancel: &Cancellation,
        ) -> Result<ModelResponse> {
            let mut reply = Worker { calls: 0 }.generate(request, cancel)?;
            let mut turn: WorkerTurn = serde_json::from_str(&reply.output).unwrap();
            let proposal = turn.proposal.as_mut().unwrap();
            proposal.changes[0].path = self.path.clone();
            proposal.changes[0].content = Some(self.source.clone());
            reply.output = serde_json::to_string(&turn).unwrap();
            Ok(reply)
        }
    }
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    for language in [Language::C, Language::Cpp] {
        let dir = tempfile::tempdir().unwrap();
        let mut c = controller(&dir.path().join("state"));
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let setup =
            fixtures::quota_c_family(&c.ledger, "native-quota", image, "fixture-base", language)
                .unwrap();
        c.create(&setup, runner::now_ms().unwrap()).unwrap();
        approve(&c, &setup);
        let (path, source) = if language == Language::C {
            (
                "implementations/quota/quota.c",
                include_str!("../fixtures/reference/quota/c/quota.c"),
            )
        } else {
            (
                "implementations/quota/quota.hpp",
                include_str!("../fixtures/reference/quota/cpp/quota.hpp"),
            )
        };
        let report = c
            .execute(
                &setup.run_id,
                &mut NativeWorker {
                    path: path.into(),
                    source: source.into(),
                },
                &Repository,
                &Cancellation::default(),
            )
            .unwrap();
        if report.state != State::Accepted {
            let run = c.ledger.run(&setup.run_id).unwrap();
            if let Some(binding) = run.binding_digest {
                for result in c.ledger.admitted_results(&setup.run_id, &binding).unwrap() {
                    eprintln!("{result:?}");
                    for digest in result.evidence {
                        eprintln!(
                            "{}",
                            String::from_utf8_lossy(&c.ledger.read(&digest).unwrap())
                        );
                    }
                }
            }
        }
        assert_eq!(report.state, State::Accepted, "{language:?}: {report:?}");
        assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent, 3);
    }
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn a_worker_cannot_suppress_an_unsafe_risk_before_native_verification() {
    struct UnsafeWorker;
    impl Provider for UnsafeWorker {
        fn generate(
            &mut self,
            request: &ModelRequest,
            cancel: &Cancellation,
        ) -> Result<ModelResponse> {
            let mut response = Worker { calls: 0 }.generate(request, cancel)?;
            let mut turn: WorkerTurn = serde_json::from_str(&response.output).unwrap();
            turn.proposal.as_mut().unwrap().changes[0].content =
                Some(format!("{QUOTA_RUST}\nunsafe fn hidden_risk() {{}}\n"));
            response.output = serde_json::to_string(&turn).unwrap();
            Ok(response)
        }
    }
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = setup(
        &c,
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
    );
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    approve(&c, &setup);
    let report = c
        .execute(
            &setup.run_id,
            &mut UnsafeWorker,
            &Repository,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(report.state, State::Blocked);
    assert!(
        report
            .reasons
            .iter()
            .any(|r| r.contains("unrouted risk") && r.contains("unsafe")),
        "{report:?}"
    );
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent, 2);
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn bounded_repair_requires_failure_review_and_resume_reuses_only_completed_receipts() {
    use rovik_harness::operations::FailureResolution;
    struct RepairWorker {
        calls: u32,
        compiler_error: bool,
    }
    impl Provider for RepairWorker {
        fn generate(
            &mut self,
            request: &ModelRequest,
            cancel: &Cancellation,
        ) -> Result<ModelResponse> {
            self.calls += 1;
            let mut response = Worker { calls: 0 }.generate(request, cancel)?;
            if self.calls == 1 {
                let mut turn: WorkerTurn = serde_json::from_str(&response.output).unwrap();
                turn.proposal.as_mut().unwrap().changes[0].content = Some(if self.compiler_error {
                    format!("{QUOTA_RUST}\nfn invalid( {{\n")
                } else {
                    QUOTA_RUST.replace("requested > self.remaining", "requested >= self.remaining")
                });
                response.output = serde_json::to_string(&turn).unwrap();
            }
            Ok(response)
        }
    }
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    for compiler_error in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let mut c = controller(&root);
        let mut setup = setup(
            &c,
            Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        );
        setup.budgets.insert("repairs".into(), 1);
        c.create(&setup, runner::now_ms().unwrap()).unwrap();
        approve(&c, &setup);
        let mut provider = RepairWorker {
            calls: 0,
            compiler_error,
        };
        let report = c
            .execute(
                &setup.run_id,
                &mut provider,
                &Repository,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(report.state, State::Blocked, "{report:?}");
        assert_eq!(provider.calls, 2);
        assert!(
            report
                .reasons
                .iter()
                .any(|r| r.contains("prior_failures_resolved")),
            "{report:?}"
        );
        let mut previous = None;
        let mut replacement = None;
        for operation in c
            .ledger
            .operations(&setup.run_id)
            .unwrap()
            .into_iter()
            .filter(|o| o.kind == "check")
        {
            let result: rovik_harness::evidence::CheckResult = rovik_harness::wire::decode(
                &c.ledger.read(&operation.output_digest.unwrap()).unwrap(),
            )
            .unwrap();
            if result.status == rovik_harness::evidence::Status::Pass {
                replacement = Some(result.result_id);
            } else {
                previous = Some(result.result_id);
            }
        }
        let spent = c.ledger.budgets(&setup.run_id).unwrap();
        assert_eq!(spent["checks"].spent, 4);
        assert_eq!(spent["repairs"].spent, 1);
        drop(c);
        let mut c = controller(&root);
        let operator = Principal::current().unwrap();
        let approval=c.resolve_failure(&operator,&setup.run_id,&FailureResolution {previous_result_id:previous.unwrap(),replacement_result_id:replacement.unwrap(),reason:"Boundary comparison was repaired; the new exact candidate passed the protected cases.".into()}).unwrap();
        c.resume(&operator, &setup.run_id).unwrap();
        let report = c
            .execute(
                &setup.run_id,
                &mut provider,
                &Repository,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(report.state, State::Accepted, "{report:?}");
        assert_eq!(provider.calls, 2);
        assert_eq!(c.ledger.budgets(&setup.run_id).unwrap(), spent);
        c.revoke_approval(
            &operator,
            &setup.run_id,
            &approval,
            "Withdraw the historical failure disposition",
        )
        .unwrap();
        assert_eq!(
            c.verify(&setup.run_id, &Repository).unwrap().verdict,
            Verdict::Blocked
        );
    }
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned image with /bin/false"]
fn completed_infrastructure_failures_retry_once_without_calling_the_implementer() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let directory = tempfile::tempdir().unwrap();
    let mut c = controller(&directory.path().join("state"));
    let mut setup = setup(
        &c,
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
    );
    let command = &mut setup.registry.commands[0];
    command.executable = "/bin/false".into();
    command.argv_template.clear();
    setup.environment.registry_digest = Digest::of_json(&setup.registry).unwrap();
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    approve(&c, &setup);
    let mut worker = Worker { calls: 0 };
    let report = c
        .execute(
            &setup.run_id,
            &mut worker,
            &Repository,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(report.state, State::Blocked, "{report:?}");
    assert_eq!(worker.calls, 0);
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent, 2);
    let operations = c.ledger.operations(&setup.run_id).unwrap();
    assert_eq!(operations.len(), 2);
    for operation in operations {
        let result: rovik_harness::evidence::CheckResult =
            wire::decode(&c.ledger.read(&operation.output_digest.unwrap()).unwrap()).unwrap();
        assert!(result.infrastructure_retryable());
        assert_eq!(result.executed_test_count, 0);
    }
}
