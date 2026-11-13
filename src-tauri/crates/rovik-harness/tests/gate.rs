use rovik_harness::{
    evidence::{self, CheckResult, Status, TrustedPreconditions, Verdict},
    identity::{Digest, EvidenceKey},
    plan::{CheckSpec, Phase},
};

fn fixture() -> (EvidenceKey, CheckSpec, CheckResult, TrustedPreconditions) {
    let d = Digest::of(b"fixture");
    let key = EvidenceKey {
        candidate_digest: d.clone(),
        contract_digest: d.clone(),
        design_digest: d.clone(),
        policy_digest: d.clone(),
        oracle_digest: d.clone(),
        plan_digest: d.clone(),
        environment_digest: d.clone(),
    };
    let spec = CheckSpec {
        check_id: "tests".into(),
        configuration_digest: d.clone(),
        checker_digest: d.clone(),
        command_id: "test".into(),
        validated_parameters: Default::default(),
        phase: Phase::Final,
        required: true,
        obligation_ids: vec!["behavior".into()],
        expected_case_ids: vec!["success".into(), "failure".into()],
        minimum_test_count: 2,
        success_exit_codes: vec![0],
        timeout_ms: 1000,
        output_limit_bytes: 1000,
        dependencies: vec![],
    };
    let result = CheckResult {
        schema_version: 1,
        run_id: "run".into(),
        result_id: "result".into(),
        operation_id: "operation".into(),
        check_id: spec.check_id.clone(),
        configuration_digest: d.clone(),
        checker_digest: d.clone(),
        evidence_key: key.clone(),
        status: Status::Pass,
        failure_category: None,
        exit_code: Some(0),
        executed_test_count: 2,
        passed_case_ids: spec.expected_case_ids.clone(),
        failed_case_ids: vec![],
        skipped_case_ids: vec![],
        evidence: vec![d],
        runner_receipt_id: "receipt".into(),
        diagnostics: vec![],
    };
    let trusted = TrustedPreconditions {
        evidence_provenance_verified: true,
        artifacts_present_and_verified: true,
        scope_verified: true,
        plan_obligations_complete: true,
        no_blocking_unknowns: true,
        review_requirements_satisfied: true,
        approvals_current: true,
        integration_candidate_current: true,
        prior_failures_resolved: true,
        accounting_complete: true,
        run_eligible: true,
    };
    (key, spec, result, trusted)
}

#[test]
fn all_pass_requires_exact_fresh_evidence_for_the_complete_matrix() {
    let (key, spec, result, trusted) = fixture();
    let schema: serde_json::Value =
        rovik_harness::wire::decode(include_bytes!("../schemas/check-result.v1.schema.json"))
            .unwrap();
    assert_eq!(result.schema_version, 1);
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        result.schema_version
    );
    for version in [0, 2, 3, 99] {
        let mut unsupported = result.clone();
        unsupported.schema_version = version;
        assert!(unsupported.validate().is_err());
    }
    assert_eq!(
        evidence::evaluate(
            "run",
            &key,
            std::slice::from_ref(&spec),
            std::slice::from_ref(&result),
            &trusted
        )
        .verdict,
        Verdict::Ready
    );
    for field in [
        "candidate_digest",
        "contract_digest",
        "design_digest",
        "policy_digest",
        "oracle_digest",
        "plan_digest",
        "environment_digest",
    ] {
        let mut stale = serde_json::to_value(&result).unwrap();
        stale["evidence_key"][field] = serde_json::to_value(Digest::of(b"changed")).unwrap();
        let stale: CheckResult = serde_json::from_value(stale).unwrap();
        let decision =
            evidence::evaluate("run", &key, std::slice::from_ref(&spec), &[stale], &trusted);
        assert_eq!(decision.verdict, Verdict::Blocked, "{field}");
        assert!(decision.reasons[0].starts_with("stale-binding:"));
    }
    assert_eq!(
        evidence::evaluate("run", &key, &[], &[], &trusted).verdict,
        Verdict::Blocked
    );
    assert_eq!(
        evidence::evaluate("run", &key, std::slice::from_ref(&spec), &[], &trusted).verdict,
        Verdict::Blocked
    );
    assert_eq!(
        evidence::evaluate(
            "run",
            &key,
            &[spec.clone(), spec.clone()],
            std::slice::from_ref(&result),
            &trusted
        )
        .verdict,
        Verdict::Blocked
    );
    assert_eq!(
        evidence::evaluate("run", &key, &[spec], &[result.clone(), result], &trusted).verdict,
        Verdict::Blocked
    );
}

#[test]
fn no_bad_status_or_inconsistent_inventory_can_become_a_pass() {
    let (key, spec, result, trusted) = fixture();
    for status in [
        Status::Fail,
        Status::Error,
        Status::Timeout,
        Status::Unsupported,
        Status::NotApplicable,
    ] {
        let mut r = result.clone();
        r.status = status;
        assert_ne!(
            evidence::evaluate("run", &key, std::slice::from_ref(&spec), &[r], &trusted).verdict,
            Verdict::Ready
        );
    }
    for mutation in 0..11 {
        let mut r = result.clone();
        match mutation {
            0 => r.run_id = "other-run".into(),
            1 => r.checker_digest = Digest::of(b"other-checker"),
            2 => r.configuration_digest = Digest::of(b"other-config"),
            3 => r.evidence.clear(),
            4 => r.exit_code = Some(1),
            5 => r.executed_test_count = 0,
            6 => {
                r.passed_case_ids.pop();
                r.skipped_case_ids.push("failure".into());
            }
            7 => r.failed_case_ids.push("unexpected-failure".into()),
            8 => r.passed_case_ids.push("extra-observed-case".into()),
            9 => r.failed_case_ids.push("success".into()),
            10 => r.schema_version = 2,
            _ => unreachable!(),
        }
        assert_ne!(
            evidence::evaluate("run", &key, std::slice::from_ref(&spec), &[r], &trusted).verdict,
            Verdict::Ready,
            "mutation {mutation}"
        );
    }
}

#[test]
fn missing_trusted_preconditions_block_even_when_every_test_passes() {
    let (key, spec, result, _) = fixture();
    let decision = evidence::evaluate(
        "run",
        &key,
        &[spec],
        &[result],
        &TrustedPreconditions::default(),
    );
    assert_eq!(decision.verdict, Verdict::Blocked);
    assert_eq!(decision.reasons.len(), 11);
    for reason in [
        "approvals_current",
        "integration_candidate_current",
        "prior_failures_resolved",
        "accounting_complete",
        "run_eligible",
    ] {
        assert!(decision.reasons.contains(&format!("precondition:{reason}")));
    }
}

#[test]
fn compiler_checks_may_run_zero_behavioral_cases() {
    let (key, mut spec, mut result, trusted) = fixture();
    spec.minimum_test_count = 0;
    spec.expected_case_ids.clear();
    result.executed_test_count = 0;
    result.passed_case_ids.clear();
    assert_eq!(
        evidence::evaluate("run", &key, &[spec], &[result], &trusted).verdict,
        Verdict::Ready
    );
}
