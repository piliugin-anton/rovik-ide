use super::*;
use crate::{
    broker::{Broker, ToolRequest},
    contract::ContractRegistry,
    controller::{Controller, TaskSetup},
    evidence::{CheckResult, Diagnostic, Status},
    fixtures,
    identity::EvidenceKey,
    provider::{ModelRequest, ModelResponse, Provider, Role},
    roles::{self, RoleInput},
    runner::{self, Cancellation, ContainerBackend},
    snapshot::Snapshot,
};
use std::collections::BTreeMap;

const SECRET: &str = "blocked-source-marker-07f67902";
const VISIBLE: &str = "visible-source-marker-54f0d663";

fn fixture(path: &std::path::Path) -> (Controller, TaskSetup, Snapshot) {
    let mut controller = Controller {
        ledger: Ledger::open(&path.join("state")).unwrap(),
        backend: ContainerBackend::management(),
    };
    let mut setup = fixtures::quota_rust(
        &controller.ledger,
        "read-policy",
        Digest::of(b"fixture-image"),
        "base",
    )
    .unwrap();
    setup.policy.deny_read = vec!["private/**".into(), ".secret".into()];
    setup.plan.policy_digest = Digest::of_json(&setup.policy).unwrap();
    let mut candidate = setup.base.clone();
    candidate.files.extend(
        fixtures::snapshot(
            &controller.ledger,
            "base",
            &[
                ("private/credentials.txt", SECRET),
                (".secret", SECRET),
                ("private-other/public.txt", VISIBLE),
            ],
        )
        .unwrap()
        .files,
    );
    let now = runner::now_ms().unwrap();
    controller
        .ledger
        .store_json("contract", &setup.contract, now)
        .unwrap();
    controller
        .ledger
        .store_json("design", &setup.design, now)
        .unwrap();
    controller
        .ledger
        .store_json("policy", &setup.policy, now)
        .unwrap();
    let input = controller
        .ledger
        .store_json("operation", &setup, now)
        .unwrap();
    controller
        .ledger
        .create_run(&setup.run_id, &input.digest, &setup.budgets, now)
        .unwrap();
    (controller, setup, candidate)
}

#[test]
fn restricted_sources_never_enter_ranked_context_or_broker_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (controller, setup, candidate) = fixture(dir.path());
    let original = candidate.digest().unwrap();
    let ranked = ranked_sources(
        &controller.ledger,
        &candidate,
        SECRET,
        &["private/credentials.txt".into(), ".secret".into()],
        Some(&setup.contract.scope),
        &setup.policy.deny_read,
    )
    .unwrap();
    let serialized = serde_json::to_string(&ranked).unwrap();
    assert!(!serialized.contains(SECRET));
    assert!(!serialized.contains("private/credentials.txt"));
    assert!(serialized.contains(VISIBLE));
    assert!(ranked.iter().all(|item| item.source_revision == original));

    let log = controller
        .ledger
        .store(
            "report",
            "text/plain",
            SECRET.as_bytes(),
            runner::now_ms().unwrap(),
        )
        .unwrap();
    let broker = Broker::new(
        &controller.ledger,
        &candidate,
        [log.digest.clone()].into(),
        &setup.policy.deny_read,
    );
    for path in ["private/credentials.txt", ".secret"] {
        for request in [
            ToolRequest::ReadFile {
                path: path.into(),
                start_line: 1,
                maximum_lines: 10,
            },
            ToolRequest::InspectSymbol {
                path: path.into(),
                symbol: "marker".into(),
            },
        ] {
            let error = broker.call(&request).unwrap_err();
            assert!(error.message.contains("restricted"));
            assert!(!error.message.contains(SECRET));
        }
    }
    for scope in ["**", "private/**", "private/credentials.txt", ".secret"] {
        let result = broker
            .call(&ToolRequest::SearchCode {
                query: "marker".into(),
                scope: scope.into(),
                maximum_results: 100,
            })
            .unwrap();
        assert!(!result.to_string().contains(SECRET));
        assert!(!result.to_string().contains("private/credentials.txt"));
        assert_eq!(
            result["hits"].as_array().unwrap().len(),
            usize::from(scope == "**")
        );
    }
    let visible = broker
        .call(&ToolRequest::ReadFile {
            path: "private-other/public.txt".into(),
            start_line: 1,
            maximum_lines: 10,
        })
        .unwrap();
    assert!(visible.to_string().contains(VISIBLE));
    assert!(broker
        .call(&ToolRequest::ReadDiagnostics {
            digest: log.digest,
            maximum_bytes: 1024,
        })
        .is_err());

    // Exclusion is a model capability restriction; native verification still
    // receives the exact complete snapshot and can validate every original byte.
    assert_eq!(candidate.digest().unwrap(), original);
    candidate.validate(&controller.ledger, true).unwrap();
    let hidden = &candidate.files["private/credentials.txt"].content.digest;
    assert_eq!(controller.ledger.read(hidden).unwrap(), SECRET.as_bytes());
}

struct InspectRequest {
    called: bool,
}
impl Provider for InspectRequest {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        self.called = true;
        let serialized = serde_json::to_string(request).unwrap();
        assert!(!serialized.contains(SECRET));
        assert!(!serialized.contains("private/credentials.txt"));
        assert!(serialized.contains(VISIBLE));
        Err(Error::new("inspection complete".into()))
    }
}

#[test]
fn implementer_and_independent_roles_receive_only_readable_source() {
    let dir = tempfile::tempdir().unwrap();
    let (mut controller, setup, candidate) = fixture(dir.path());
    let context = controller.context(&setup, &candidate).unwrap();
    let index = context
        .items
        .iter()
        .find(|item| item.item_id == "source-index")
        .unwrap();
    let paths: Vec<String> = serde_json::from_str(&index.text).unwrap();
    assert!(!paths.contains(&"private/credentials.txt".into()));
    assert!(!paths.contains(&".secret".into()));
    assert!(paths.contains(&"private-other/public.txt".into()));
    let mut provider = InspectRequest { called: false };
    let error = controller
        .implement_with_context(
            &setup,
            &mut provider,
            &Cancellation::default(),
            candidate.clone(),
            vec![],
            context,
        )
        .unwrap_err();
    assert!(provider.called);
    assert!(error.message.contains("inspection complete"));

    let registry = ContractRegistry {
        profiles: setup
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect(),
        configurations: setup
            .profiles
            .iter()
            .flat_map(|profile| &profile.configurations)
            .map(|configuration| configuration.digest().unwrap())
            .collect(),
        suites: setup.suites.clone(),
        constraints: setup.constraint_names.clone(),
        risks: setup.risk_names.clone(),
    };
    let artifacts = BTreeMap::from([("policy".into(), Digest::of_json(&setup.policy).unwrap())]);
    let input = RoleInput {
        run_id: &setup.run_id,
        role: Role::DomainAnalyst,
        snapshot: &candidate,
        registry: &registry,
        artifacts: &artifacts,
        limits: &setup.worker,
        deadline_ms: setup.deadline_ms,
    };
    let mut provider = InspectRequest { called: false };
    let error = roles::generate(
        &mut controller.ledger,
        &input,
        &mut provider,
        &Cancellation::default(),
    )
    .unwrap_err();
    assert!(provider.called);
    assert!(error.message.contains("inspection complete"));
}

#[test]
fn native_feedback_hides_unattributed_source_without_changing_verification_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let (controller, setup, _) = fixture(dir.path());
    let digest = Digest::of(SECRET.as_bytes());
    let result = CheckResult {
        schema_version: 1,
        run_id: setup.run_id.clone(),
        result_id: "result".into(),
        operation_id: "operation".into(),
        check_id: "native-tests".into(),
        configuration_digest: digest.clone(),
        checker_digest: digest.clone(),
        evidence_key: EvidenceKey {
            verification_nonce: None,
            candidate_digest: digest.clone(),
            contract_digest: digest.clone(),
            design_digest: digest.clone(),
            policy_digest: digest.clone(),
            oracle_digest: digest.clone(),
            plan_digest: digest.clone(),
            environment_digest: digest.clone(),
        },
        status: Status::Fail,
        failure_category: Some(crate::FailureCategory::Implementation),
        exit_code: Some(1),
        executed_test_count: 1,
        passed_case_ids: vec![],
        failed_case_ids: vec![SECRET.into()],
        skipped_case_ids: vec![],
        evidence: vec![digest],
        runner_receipt_id: "receipt".into(),
        diagnostics: vec![Diagnostic {
            rule_id: "assertion".into(),
            message: SECRET.into(),
            // A public test may print hidden source, so file filtering alone is insufficient.
            file: Some("tests/public.rs".into()),
            line: Some(1),
        }],
    };
    result.validate().unwrap();
    let original = serde_json::to_value(&result).unwrap();
    let feedback = check_feedback(&setup.policy, &result).unwrap();
    assert!(!feedback.to_string().contains(SECRET));
    assert_eq!(feedback["check_id"], "native-tests");
    assert_eq!(feedback["status"], "fail");
    assert_eq!(feedback["executed_test_count"], 1);
    let mut unrestricted = setup.policy;
    unrestricted.deny_read.clear();
    assert_eq!(check_feedback(&unrestricted, &result).unwrap(), original);
    let stored = controller
        .ledger
        .store_json("report", &result, runner::now_ms().unwrap())
        .unwrap();
    let persisted: CheckResult =
        crate::wire::decode(&controller.ledger.read(&stored.digest).unwrap()).unwrap();
    assert_eq!(persisted, result);
    assert!(persisted.repairable());
}
