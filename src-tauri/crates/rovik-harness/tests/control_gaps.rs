#![cfg(target_os = "linux")]
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    controller::Controller,
    evidence::Status,
    exceptions::{DispositionKind, PolicyException},
    fixtures,
    identity::{Digest, EvidenceKey},
    ledger::Ledger,
    normalizer,
    pack_protocol::{PackError, Response},
    plan::Disposition,
    profile::Language,
    review_bundle::SnapshotPatch,
    runner::{self, ContainerBackend},
    runtime,
    state::State,
    wire, FailureCategory,
};

fn controller(path: &std::path::Path) -> Controller {
    Controller {
        ledger: Ledger::open(path).unwrap(),
        backend: ContainerBackend::local(std::path::Path::new("/bin/true")).unwrap(),
    }
}

#[test]
fn compiler_failures_are_repairable_but_never_supply_negative_control_cases() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let setup = fixtures::quota_rust(&ledger, "compile", Digest::of(b"image"), "base").unwrap();
    let definition = &setup.registry.commands[0];
    let result = normalizer::normalize_command(definition, Some(42), b"", None, &[0]);
    assert_eq!(result.status, Status::Fail);
    assert_eq!(
        result.failure_category,
        Some(FailureCategory::Implementation)
    );
    assert_eq!(result.inventory.executed(), 0);
    assert!(result.inventory.failed.is_empty());
    let corrupt = normalizer::normalize_command(definition, Some(0), b"", Some(b"not JSON"), &[0]);
    assert_eq!(corrupt.status, Status::Error);
    assert_eq!(
        corrupt.failure_category,
        Some(FailureCategory::Infrastructure)
    );
}

#[test]
fn correlated_runtime_compilation_errors_keep_their_category_and_legacy_errors_block() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let setup = fixtures::quota_runtime(
        &ledger,
        "runtime",
        Digest::of(b"image"),
        "base",
        Language::TypeScript,
    )
    .unwrap();
    let request = wire::decode(
        &ledger
            .read(&setup.oracle.files["requests/quota.json"].content.digest)
            .unwrap(),
    )
    .unwrap();
    let mut reply = Response {
        protocol_version: 1,
        request_id: "quota".into(),
        ok: false,
        result: None,
        error: Some(PackError {
            code: "CANDIDATE_FAILURE".into(),
            message: "TypeScript compilation failed".into(),
            retryable: false,
            category: FailureCategory::Implementation,
        }),
    };
    let config = setup.profiles[0].configurations[0].digest().unwrap();
    let encode = |reply: &Response| {
        let mut bytes = wire::canonical(reply).unwrap();
        bytes.push(b'\n');
        bytes
    };
    let error = runtime::verify(&request, &encode(&reply), &config, Some(2)).unwrap_err();
    assert_eq!(
        normalizer::Normalized::from_error(&error).status,
        Status::Fail
    );
    reply.request_id = "wrong-request".into();
    let error = runtime::verify(&request, &encode(&reply), &config, Some(2)).unwrap_err();
    assert_eq!(
        normalizer::Normalized::from_error(&error).status,
        Status::Error
    );
    let legacy: PackError =
        wire::decode(br#"{"code":"OLD","message":"failure","retryable":false}"#).unwrap();
    assert_eq!(legacy.category, FailureCategory::Infrastructure);
}

#[test]
fn registered_exceptions_need_authentication_and_cannot_waive_another_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let mut setup =
        fixtures::quota_rust(&c.ledger, "exception", Digest::of(b"image"), "base").unwrap();
    setup.policy.obligations.insert("ARCH-EXAMPLE".into());
    let now = runner::now_ms().unwrap();
    let evidence = c
        .ledger
        .store_json(
            "report",
            &serde_json::json!({"alternative":"approved consumer checks"}),
            now,
        )
        .unwrap();
    let exception = PolicyException {
        schema_version: 1,
        exception_id: "temporary-alternative".into(),
        obligation_id: "ARCH-EXAMPLE".into(),
        kind: DispositionKind::Exception,
        scope: setup.contract.scope.clone(),
        owner: "maintainer".into(),
        rationale: "Use an explicitly reviewed alternative until the target collector is available"
            .into(),
        expires_ms: now + 60_000,
        configurations: [setup.plan.configurations[0].clone()].into(),
        replacement_evidence: vec![evidence.digest],
        residual_risk: Some("Only the declared consumers are exercised".into()),
        qualifies_acceptance: true,
    };
    let approval = exception.approval().unwrap();
    setup.exceptions.push(exception);
    setup.policy.facts.insert(
        "exceptions_digest".into(),
        Digest::of_json(&setup.exceptions).unwrap().to_string(),
    );
    setup.plan.policy_digest = Digest::of_json(&setup.policy).unwrap();
    setup.plan.approved_exceptions.push(Disposition {
        obligation_id: "ARCH-EXAMPLE".into(),
        evidence_digest: approval.subject_digest.clone(),
        approval: approval.clone(),
    });
    setup.plan.approval_requirements.push(approval.clone());
    let principal = Principal::current().unwrap();
    setup
        .authority
        .actors
        .get_mut(principal.actor_id())
        .unwrap()
        .insert("policy-exception".into());
    setup.validate(&c.ledger).unwrap();
    c.create(&setup, now).unwrap();
    assert!(c
        .pending_approvals(&setup.run_id)
        .unwrap()
        .contains(&approval));
    let granted = c
        .ledger
        .grant(
            &principal,
            &setup.authority,
            ApprovalRequest {
                run_id: Some(setup.run_id.clone()),
                subject_digest: approval.subject_digest.clone(),
                approval_type: approval.approval_type.clone(),
                policy_digest: Digest::of_json(&setup.policy).unwrap(),
                scope_digest: Digest::of_json(&setup.contract.scope).unwrap(),
                expires_ms: None,
            },
            now,
        )
        .unwrap();
    assert!(!c
        .pending_approvals(&setup.run_id)
        .unwrap()
        .contains(&approval));
    c.ledger
        .revoke(
            &principal,
            &setup.authority,
            &granted.approval_id,
            "replacement control withdrawn",
            now + 1,
        )
        .unwrap();
    assert!(c
        .pending_approvals(&setup.run_id)
        .unwrap()
        .contains(&approval));
    let mut second = setup.profiles[0].configurations[0].clone();
    second.id = "second-configuration".into();
    let second_digest = second.digest().unwrap();
    setup.profiles[0].configurations.push(second);
    setup.plan.configurations.push(second_digest.clone());
    let checks = setup.plan.checks.clone();
    setup
        .plan
        .checks
        .extend(checks.into_iter().map(|mut check| {
            check.configuration_digest = second_digest.clone();
            check
        }));
    assert!(setup
        .plan
        .validate(&setup.plan_registry().unwrap())
        .unwrap_err()
        .message
        .contains("uncovered obligations"));
    setup
        .plan
        .checks
        .iter_mut()
        .filter(|check| check.configuration_digest == second_digest)
        .for_each(|check| check.obligation_ids.push("ARCH-EXAMPLE".into()));
    setup
        .plan
        .validate(&setup.plan_registry().unwrap())
        .unwrap();
}

#[test]
fn review_exports_bind_patch_source_history_approvals_and_file_hashes() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = fixtures::quota_rust(&c.ledger, "export", Digest::of(b"image"), "base").unwrap();
    let now = runner::now_ms().unwrap();
    let run = c.create(&setup, now).unwrap();
    let mut candidate = fixtures::snapshot(
        &c.ledger,
        "base",
        &[("implementations/quota/lib.rs", fixtures::QUOTA_RUST)],
    )
    .unwrap();
    let asset = c
        .ledger
        .store_source(std::io::repeat(0xfe).take(65 * 1024 * 1024), now)
        .unwrap();
    candidate.files.insert(
        "image.png".into(),
        rovik_harness::snapshot::FileEntry {
            mode: 0o100644,
            content: asset,
        },
    );
    let context =
        rovik_harness::context::ranked_sources(&c.ledger, &candidate, "quota", &[], None, &[])
            .unwrap();
    assert!(
        context
            .iter()
            .all(|item| item.item_id != "source:image.png"),
        "binary assets belong to the snapshot, not model text"
    );
    c.ledger.store_json("snapshot", &candidate, now).unwrap();
    let key = EvidenceKey {
        verification_nonce: None,
        candidate_digest: candidate.digest().unwrap(),
        contract_digest: Digest::of_json(&setup.contract).unwrap(),
        design_digest: Digest::of_json(&setup.design).unwrap(),
        policy_digest: Digest::of_json(&setup.policy).unwrap(),
        oracle_digest: setup.oracle.digest().unwrap(),
        plan_digest: Digest::of_json(&setup.plan).unwrap(),
        environment_digest: Digest::of_json(&setup.environment).unwrap(),
    };
    let binding = c.ledger.store_json("operation", &key, now).unwrap();
    c.ledger.bind(&run, &binding.digest, now).unwrap();
    let destination = dir.path().join("bundle");
    let manifest = c.export_bundle(&setup.run_id, &destination).unwrap();
    assert_eq!(
        manifest.state,
        State::Intake,
        "export is not an acceptance decision"
    );
    for (path, digest) in manifest.files {
        assert_eq!(
            Digest::copy(
                std::fs::File::open(destination.join(path)).unwrap(),
                std::io::sink(),
                rovik_harness::artifacts::MAX_SOURCE_BYTES
            )
            .unwrap()
            .0,
            digest
        );
    }
    let patch: SnapshotPatch =
        wire::decode(&std::fs::read(destination.join("patch.json")).unwrap()).unwrap();
    assert_eq!(patch.apply(&setup.base).unwrap(), candidate);
    assert!(destination.join("evidence.json").is_file());
    assert!(
        c.export_bundle(&setup.run_id, &destination).is_err(),
        "an export cannot overwrite another bundle"
    );
}

#[test]
fn execution_exit_codes_distinguish_acceptance_cancellation_and_unfinished_work() {
    for (state, code) in [
        (State::Accepted, 0),
        (State::Blocked, 2),
        (State::Intake, 2),
        (State::Stopped, 3),
        (State::Rejected, 3),
        (State::Cancelled, 5),
    ] {
        assert_eq!(state.execution_exit_code(), code);
    }
}
