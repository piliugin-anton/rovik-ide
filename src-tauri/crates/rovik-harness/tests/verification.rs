#![cfg(target_os = "linux")]
mod support;
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    baseline::{failure_signature, ExpectedFailure},
    controller::{Controller, Integration, TaskSetup},
    evidence::{Status, Verdict},
    fixtures,
    identity::{Digest, EvidenceKey},
    ledger::Ledger,
    plan::ApprovalRequirement,
    provider::{self, ModelRequest, ModelResponse, Provider, Role},
    runner::{self, Cancellation, ContainerBackend},
    state::State,
    wire, FailureCategory, Result,
};
use std::collections::BTreeSet;
struct Offline;
impl Provider for Offline {
    fn supports_role(&self, _: Role) -> bool {
        false
    }
    fn generate(&mut self, _: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        panic!("offline verification attempted a model call")
    }
}
struct Destination;
impl Integration for Destination {
    fn current_revision(&self, _: &str) -> Result<String> {
        Ok("base".into())
    }
}
fn approve(c: &Controller, setup: &TaskSetup, requirement: ApprovalRequirement) -> String {
    c.ledger
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
        .unwrap()
        .approval_id
}
#[test]
fn offline_provider_never_reserves_or_spends_a_model_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(std::path::Path::new("/bin/true")).unwrap(),
    };
    let setup = fixtures::quota_rust(&c.ledger, "offline", Digest::of(b"image"), "base").unwrap();
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    let request = ModelRequest {
        role: Role::Implementer,
        request_id: "offline-attempt".into(),
        provider_model_id: "unused".into(),
        context: rovik_harness::context::ContextBundle {
            schema_version: 1,
            items: vec![],
            omitted: vec![],
        },
        snapshot_digest: setup.base.digest().unwrap(),
        maximum_output_tokens: 1,
        timeout_ms: 1000,
        feedback: vec![],
    };
    assert_eq!(
        provider::accounted_call(
            &mut c.ledger,
            &setup.run_id,
            &request,
            provider::Reservation {
                tokens: 1,
                cost_microunits: 0
            },
            &mut Offline,
            &Cancellation::default()
        )
        .unwrap_err()
        .category,
        FailureCategory::Capability
    );
    assert!(c.ledger.operations(&setup.run_id).unwrap().is_empty());
    assert!(c
        .ledger
        .budgets(&setup.run_id)
        .unwrap()
        .values()
        .all(|account| account.spent == 0 && account.reserved == 0));
    let mut initial = setup.base.clone();
    let forbidden = c
        .ledger
        .store("source", "text/plain", b"unapproved compiler options", 0)
        .unwrap();
    initial.files.insert(
        "Cargo.toml".into(),
        rovik_harness::snapshot::FileEntry {
            mode: 0o100644,
            content: forbidden,
        },
    );
    let mut invalid = setup;
    invalid.initial_candidate = Some(initial);
    assert!(invalid.validate(&c.ledger).is_err());
}
#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native image"]
fn initial_candidates_run_without_models_and_fresh_verification_replaces_all_final_evidence() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(&support::docker_path()).unwrap(),
    };
    let mut setup = fixtures::quota_rust(
        &c.ledger,
        "verify-fresh",
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        "base",
    )
    .unwrap();
    setup.initial_candidate = Some(
        fixtures::snapshot(
            &c.ledger,
            "base",
            &[("implementations/quota/lib.rs", fixtures::QUOTA_RUST)],
        )
        .unwrap(),
    );
    setup.plan.review_requirements.push("final-review".into());
    setup
        .authority
        .actors
        .get_mut(Principal::current().unwrap().actor_id())
        .unwrap()
        .extend(["final-review".into(), "delivery".into()]);
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    for requirement in setup.mandatory_approvals().unwrap() {
        approve(&c, &setup, requirement);
    }
    let report = c
        .execute(
            &setup.run_id,
            &mut Offline,
            &Destination,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(report.state, State::Blocked, "{report:?}");
    assert_eq!(
        c.ledger.run(&setup.run_id).unwrap().restart_state,
        Some(State::Review)
    );
    let mut binding = c.ledger.run(&setup.run_id).unwrap().binding_digest.unwrap();
    for _ in 0..2 {
        let before = c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent;
        let verdict = c
            .verify_fresh(
                &Principal::current().unwrap(),
                &setup.run_id,
                &Destination,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(verdict.verdict, Verdict::Blocked); // Only the separate candidate review is pending.
        let next = c.ledger.run(&setup.run_id).unwrap().binding_digest.unwrap();
        assert_ne!(next, binding);
        let key: EvidenceKey = wire::decode(&c.ledger.read(&next).unwrap()).unwrap();
        assert!(key.verification_nonce.is_some());
        let results = c.ledger.admitted_results(&setup.run_id, &next).unwrap();
        assert_eq!(results.len(), setup.plan.required_final().count());
        assert!(results.iter().all(|result| result.status == Status::Pass));
        assert_eq!(
            c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent - before,
            setup.plan.required_final().count() as u64
        );
        binding = next;
    }
    assert_eq!(
        c.ledger.budgets(&setup.run_id).unwrap()["model_calls"].spent,
        0
    );
    for requirement in c.pending_approvals(&setup.run_id).unwrap() {
        approve(&c, &setup, requirement);
    }
    let report = c
        .execute(
            &setup.run_id,
            &mut Offline,
            &Destination,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(report.state, State::Accepted, "{report:?}");
    assert_eq!(
        c.verify(&setup.run_id, &Destination).unwrap().verdict,
        Verdict::Ready
    );
    assert!(c
        .verify_fresh(
            &Principal::current().unwrap(),
            &setup.run_id,
            &Destination,
            &Cancellation::default()
        )
        .is_err());
    let subject = c
        .ledger
        .store_json(
            "delivery",
            &serde_json::json!({"commit":"exact-delivery"}),
            runner::now_ms().unwrap(),
        )
        .unwrap()
        .digest;
    let called = std::cell::Cell::new(false);
    assert!(c
        .deliver_authorized(
            &Principal::current().unwrap(),
            &setup.run_id,
            &subject,
            &Destination,
            || {
                called.set(true);
                Ok(())
            }
        )
        .is_err());
    assert!(!called.get());
    let approval = approve(
        &c,
        &setup,
        ApprovalRequirement {
            subject_digest: subject.clone(),
            approval_type: "delivery".into(),
            obligation_ids: vec![],
        },
    );
    c.deliver_authorized(
        &Principal::current().unwrap(),
        &setup.run_id,
        &subject,
        &Destination,
        || {
            let connection =
                rusqlite::Connection::open(c.ledger.root().join("ledger.sqlite3")).unwrap();
            connection.busy_timeout(std::time::Duration::ZERO).unwrap();
            assert!(connection.execute_batch("BEGIN IMMEDIATE").is_err());
            Ok(())
        },
    )
    .unwrap();
    c.revoke_approval(
        &Principal::current().unwrap(),
        &setup.run_id,
        &approval,
        "delivery withdrawn",
    )
    .unwrap();
    assert!(c
        .deliver_authorized(
            &Principal::current().unwrap(),
            &setup.run_id,
            &subject,
            &Destination,
            || Ok(())
        )
        .is_err());

    let mut result = c
        .ledger
        .admitted_results(&setup.run_id, &binding)
        .unwrap()
        .remove(0);
    result.status = Status::Fail;
    result.failure_category = Some(FailureCategory::Implementation);
    result.exit_code = Some(42);
    result.passed_case_ids.clear();
    result.failed_case_ids.clear();
    result.executed_test_count = 0;
    let mut expectation = ExpectedFailure {
        check_id: result.check_id.clone(),
        configuration_digest: result.configuration_digest.clone(),
        case_ids: BTreeSet::new(),
        observed_signature: Some(failure_signature(&result).unwrap()),
        rationale: "reviewed compiler failure".into(),
    };
    assert!(expectation.matches(&result).unwrap());
    result.evidence = vec![Digest::of(b"different compiler diagnostic")];
    assert!(!expectation.matches(&result).unwrap());
    expectation.observed_signature = Some(failure_signature(&result).unwrap());
    result.failure_category = Some(FailureCategory::Infrastructure);
    assert!(!expectation.matches(&result).unwrap());
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native image"]
fn a_reviewed_replan_can_dispose_of_obsolete_failures_without_erasing_them() {
    use rovik_harness::{evidence::CheckResult, resolution::ReplannedFailure};
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(&support::docker_path()).unwrap(),
    };
    let mut setup = fixtures::quota_rust(
        &c.ledger,
        "replanned-failure",
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        "base",
    )
    .unwrap();
    setup.initial_candidate = Some(setup.base.clone());
    setup
        .budgets
        .extend([("replans".into(), 1), ("repairs".into(), 1)]);
    setup
        .authority
        .actors
        .get_mut(Principal::current().unwrap().actor_id())
        .unwrap()
        .extend(["replan".into(), "replan-resolution".into()]);
    c.create(&setup, runner::now_ms().unwrap()).unwrap();
    for requirement in setup.mandatory_approvals().unwrap() {
        approve(&c, &setup, requirement);
    }
    let failed = c
        .execute(
            &setup.run_id,
            &mut Offline,
            &Destination,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(failed.state, State::Blocked, "{failed:?}");
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["repairs"].spent, 0);
    let operation = c
        .ledger
        .operations(&setup.run_id)
        .unwrap()
        .into_iter()
        .find(|operation| operation.kind == "check")
        .unwrap();
    let prior: CheckResult =
        wire::decode(&c.ledger.read(&operation.output_digest.unwrap()).unwrap()).unwrap();
    assert_eq!(prior.status, Status::Fail);
    let mut replacement = setup.clone();
    replacement.initial_candidate = Some(
        fixtures::snapshot(
            &c.ledger,
            "base",
            &[("implementations/quota/lib.rs", fixtures::QUOTA_RUST)],
        )
        .unwrap(),
    );
    let old = replacement.plan.checks[0].check_id.clone();
    for check in &mut replacement.plan.checks {
        if check.check_id == old {
            check.check_id = "replacement-contract".into();
        }
    }
    replacement.baseline.expected_failures[0].check_id = "replacement-contract".into();
    let activation = c
        .stage_replan(&Principal::current().unwrap(), &replacement)
        .unwrap();
    for requirement in replacement.mandatory_approvals().unwrap() {
        approve(&c, &replacement, requirement);
    }
    approve(&c, &replacement, activation.clone());
    c.activate_replan(
        &Principal::current().unwrap(),
        &setup.run_id,
        &activation.subject_digest,
    )
    .unwrap();
    let blocked = c
        .execute(
            &setup.run_id,
            &mut Offline,
            &Destination,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(blocked.state, State::Blocked, "{blocked:?}");
    assert!(blocked
        .gate
        .unwrap()
        .reasons
        .contains(&"precondition:prior_failures_resolved".into()));
    let binding = c.ledger.run(&setup.run_id).unwrap().binding_digest.unwrap();
    let results = c.ledger.admitted_results(&setup.run_id, &binding).unwrap();
    let mut disposition = ReplannedFailure {
        previous_result_id: prior.result_id.clone(),
        replacement_input: activation.subject_digest,
        replacement_result_ids: results
            .iter()
            .map(|result| result.result_id.clone())
            .collect(),
        rationale: "The reviewed replacement gives the check its new contract identity".into(),
        residual_risk: "Only this declared configuration has been verified".into(),
    };
    let valid = disposition.replacement_result_ids.clone();
    disposition.replacement_result_ids.clear();
    assert!(c
        .resolve_replanned_failure(&Principal::current().unwrap(), &setup.run_id, &disposition)
        .is_err());
    disposition.replacement_result_ids = valid;
    let approval = c
        .resolve_replanned_failure(&Principal::current().unwrap(), &setup.run_id, &disposition)
        .unwrap();
    assert_eq!(
        c.verify(&setup.run_id, &Destination).unwrap().verdict,
        Verdict::Ready
    );
    c.revoke_approval(
        &Principal::current().unwrap(),
        &setup.run_id,
        &approval,
        "disposition withdrawn",
    )
    .unwrap();
    assert_eq!(
        c.verify(&setup.run_id, &Destination).unwrap().verdict,
        Verdict::Blocked
    );
    c.resolve_replanned_failure(&Principal::current().unwrap(), &setup.run_id, &disposition)
        .unwrap();
    c.resume(&Principal::current().unwrap(), &setup.run_id)
        .unwrap();
    assert_eq!(
        c.execute(
            &setup.run_id,
            &mut Offline,
            &Destination,
            &Cancellation::default()
        )
        .unwrap()
        .state,
        State::Accepted
    );
    assert_eq!(
        c.ledger.result(&setup.run_id, &prior.result_id).unwrap(),
        prior
    );
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["replans"].spent, 1);
}
