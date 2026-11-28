use crate::{
    architecture::{self, ArchitecturePolicy, Component, Role, Visibility},
    controller::TaskSetup,
    evidence::{CheckResult, Status},
    identity::{Digest, EvidenceKey},
    ledger::Ledger,
    pack_protocol::{self, Edge, Graph, NativeInput, Node, Operation, Precision, Request},
    plan::{CheckSpec, Phase},
    runner::{self, ExecutionInput, Observation},
    snapshot::FileEntry,
};

struct Fixture {
    _directory: tempfile::TempDir,
    ledger: Ledger,
    setup: TaskSetup,
    request: Request,
    graph: Graph,
    spec: CheckSpec,
    key: EvidenceKey,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let mut setup =
            crate::fixtures::quota_rust(&ledger, "architecture", Digest::of(b"image"), "base")
                .unwrap();
        let input = NativeInput {
            snapshot_handle: "snapshot:active".into(),
            profile: setup.profiles[0].clone(),
            configuration_id: setup.profiles[0].configurations[0].id.clone(),
            forbidden_edges: vec![],
            required_test_ids: ["discovered-test".into()].into(),
            typescript_project: None,
            architecture: Some(ArchitecturePolicy {
                schema_version: 1,
                components: [
                    Component {
                        id: "domain".into(),
                        context_id: "domain".into(),
                        selectors: vec!["src/domain.rs".into()],
                        role: Role::Domain,
                        visibility: Visibility::Public,
                        pure: true,
                    },
                    Component {
                        id: "adapter".into(),
                        context_id: "io".into(),
                        selectors: vec!["src/adapter.rs".into()],
                        role: Role::Adapter,
                        visibility: Visibility::ContextPrivate,
                        pure: false,
                    },
                ]
                .into(),
                baseline_external_edges: Default::default(),
                approved_external_edges: Default::default(),
                approved_cycle_edges: Default::default(),
                include_test_dependencies: false,
                require_symbol_precision: false,
            }),
        };
        let request = Request {
            protocol_version: 1,
            request_id: "architecture".into(),
            operation: Operation::Index,
            payload: serde_json::to_value(&input).unwrap(),
        };
        let mut bytes = crate::wire::canonical(&request).unwrap();
        bytes.push(b'\n');
        let source = ledger
            .store(
                "source",
                "application/octet-stream",
                &bytes,
                runner::now_ms().unwrap(),
            )
            .unwrap();
        setup.oracle.files.insert(
            "requests/architecture.json".into(),
            FileEntry {
                mode: 0o100644,
                content: source,
            },
        );
        let command = crate::packs::graph_command(["architecture".into()].into()).unwrap();
        let configuration = input.profile.configurations[0].digest().unwrap();
        let spec = CheckSpec {
            check_id: "architecture".into(),
            configuration_digest: configuration.clone(),
            checker_digest: command.checker_digest.clone(),
            command_id: command.id.clone(),
            validated_parameters: [("request_id".into(), "architecture".into())].into(),
            phase: Phase::Final,
            required: true,
            obligation_ids: vec![],
            expected_case_ids: vec![],
            minimum_test_count: 0,
            success_exit_codes: vec![0],
            timeout_ms: 60_000,
            output_limit_bytes: 1024 * 1024,
            dependencies: vec![],
        };
        setup.registry.commands.push(command);
        setup.plan.checks.push(spec.clone());
        setup.plan.oracle_digest = setup.oracle.digest().unwrap();
        let now = runner::now_ms().unwrap();
        let key = EvidenceKey {
            verification_nonce: None,
            candidate_digest: setup.base.digest().unwrap(),
            contract_digest: Digest::of_json(&setup.contract).unwrap(),
            design_digest: Digest::of_json(&setup.design).unwrap(),
            policy_digest: Digest::of_json(&setup.policy).unwrap(),
            oracle_digest: setup.oracle.digest().unwrap(),
            plan_digest: Digest::of_json(&setup.plan).unwrap(),
            environment_digest: Digest::of_json(&setup.environment).unwrap(),
        };
        for (kind, value) in [
            ("contract", serde_json::to_value(&setup.contract).unwrap()),
            ("design", serde_json::to_value(&setup.design).unwrap()),
            ("snapshot", serde_json::to_value(&setup.base).unwrap()),
            ("snapshot", serde_json::to_value(&setup.oracle).unwrap()),
            ("policy", serde_json::to_value(&setup.policy).unwrap()),
            (
                "policy",
                serde_json::to_value(&setup.contract.scope).unwrap(),
            ),
        ] {
            ledger.store_json(kind, &value, now).unwrap();
        }
        let stored = ledger.store_json("operation", &setup, now).unwrap();
        let run = ledger
            .create_run(
                &setup.run_id,
                &stored.digest,
                &[("checks".into(), 32)].into(),
                now,
            )
            .unwrap();
        let binding = ledger.store_json("operation", &key, now).unwrap();
        ledger.bind(&run, &binding.digest, now).unwrap();
        let graph = Graph {
            precision: Precision::CargoResolved,
            nodes: [
                Node {
                    id: "domain".into(),
                    path: Some("src/domain.rs".into()),
                    kind: "package".into(),
                },
                Node {
                    id: "adapter".into(),
                    path: Some("src/adapter.rs".into()),
                    kind: "package".into(),
                },
            ]
            .into(),
            edges: vec![Edge {
                source: "domain".into(),
                target: "adapter".into(),
                kind: "normal".into(),
            }],
            unresolved: vec![],
            configuration_digest: configuration,
            compiler_commands: vec![],
            test_inventory: vec!["discovered-test".into()],
        };
        Self {
            _directory: directory,
            ledger,
            setup,
            request,
            graph,
            spec,
            key,
        }
    }
    fn bytes(&self, graph: &Graph) -> Vec<u8> {
        let mut bytes = crate::wire::canonical(&serde_json::json!({"protocol_version":1,"request_id":"architecture","ok":true,"result":{"language":"rust","graph":graph}})).unwrap();
        bytes.push(b'\n');
        bytes
    }
    fn admit(&mut self, graph: Graph, incomplete: bool, complete: bool) -> CheckResult {
        let now = runner::now_ms().unwrap();
        let input = self
            .ledger
            .store_json(
                "operation",
                &ExecutionInput {
                    evidence_key: self.key.clone(),
                    check: self.spec.clone(),
                },
                now,
            )
            .unwrap();
        let lease = self
            .ledger
            .claim(
                &self.setup.run_id,
                &uuid::Uuid::new_v4().to_string(),
                "check",
                &input.digest,
                "test",
                now,
                60_000,
                &[("checks".into(), 1)].into(),
            )
            .unwrap();
        let definition = self.setup.registry.get(&self.spec.command_id).unwrap();
        let observation = Observation {
            job_id: uuid::Uuid::new_v4().to_string(),
            candidate_digest: self.key.candidate_digest.clone(),
            oracle_digest: self.key.oracle_digest.clone(),
            environment_digest: self.key.environment_digest.clone(),
            configuration_digest: self.spec.configuration_digest.clone(),
            command_digest: Digest::of_json(definition).unwrap(),
            parameters: self.spec.validated_parameters.clone(),
            exit_code: Some(0),
            stdout: self.bytes(&graph),
            stderr: vec![],
            report: None,
            timed_out: false,
            cancelled: incomplete,
            output_exceeded: false,
            oom_killed: false,
            duration_ms: 1,
        };
        let result = runner::admit_observation(
            &mut self.ledger,
            &lease,
            &self.key,
            &self.spec,
            definition,
            observation,
            now,
        )
        .unwrap();
        if complete {
            let stored = self.ledger.store_json("result", &result, now).unwrap();
            self.ledger
                .complete(&lease, &stored.digest, &[("checks".into(), 1)].into(), now)
                .unwrap();
        }
        result
    }
}

#[test]
fn architectural_failures_retain_completed_authenticated_graphs_without_passing_authority() {
    let mut fixture = Fixture::new();
    let bytes = fixture.bytes(&fixture.graph);
    assert!(pack_protocol::verify_index(
        &fixture.request,
        &bytes,
        &fixture.spec.configuration_digest
    )
    .is_err());
    let observed = pack_protocol::verify_index_observation(
        &fixture.request,
        &bytes,
        &fixture.spec.configuration_digest,
    )
    .unwrap();
    assert!(observed
        .findings
        .iter()
        .any(|finding| finding.rule_id == "ARCH-002" && finding.path == ["domain", "adapter"]));
    let result = fixture.admit(fixture.graph.clone(), false, true);
    assert_eq!(result.status, Status::Fail);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.rule_id == "ARCH-002" && diagnostic.line.is_none()));
    let graphs =
        architecture::observed_graphs(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
            .unwrap();
    assert_eq!(graphs.len(), 1);
    assert_eq!(graphs[0].result.runner_receipt_id, result.runner_receipt_id);
    assert!(
        architecture::admitted_graphs(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
            .unwrap()
            .is_empty()
    );
    let mut wrong = fixture.key.clone();
    wrong.candidate_digest = Digest::of(b"other candidate");
    assert!(
        architecture::observed_graphs(&fixture.ledger, &fixture.setup.run_id, &wrong)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn incomplete_and_unfinished_checks_never_supply_architecture_feedback() {
    for (incomplete, complete) in [(true, true), (false, false)] {
        let mut fixture = Fixture::new();
        fixture.admit(fixture.graph.clone(), incomplete, complete);
        assert!(architecture::observed_graphs(
            &fixture.ledger,
            &fixture.setup.run_id,
            &fixture.key
        )
        .unwrap()
        .is_empty());
    }
}

#[test]
fn stale_receipt_generations_cannot_supply_advisory_graphs() {
    let mut fixture = Fixture::new();
    let result = fixture.admit(fixture.graph.clone(), false, true);
    fixture
        .ledger
        .connection
        .execute(
            "UPDATE operations SET generation=generation+1 WHERE operation_id=?1",
            [&result.operation_id],
        )
        .unwrap();
    assert!(
        architecture::observed_graphs(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
            .is_err()
    );
    assert!(
        architecture::feedback_results(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
            .is_err()
    );
}

#[test]
fn collector_validation_finishes_before_architecture_failures_can_publish_graphs() {
    for fault in ["inventory", "precision", "unresolved", "configuration"] {
        let mut fixture = Fixture::new();
        let mut graph = fixture.graph.clone();
        match fault {
            "inventory" => graph.test_inventory.clear(),
            "precision" => graph.precision = Precision::Lexical,
            "unresolved" => graph.unresolved.push("unknown import".into()),
            "configuration" => graph.configuration_digest = Digest::of(b"other configuration"),
            _ => unreachable!(),
        }
        assert!(
            pack_protocol::verify_index_observation(
                &fixture.request,
                &fixture.bytes(&graph),
                &fixture.spec.configuration_digest
            )
            .is_err(),
            "{fault}"
        );
        let result = fixture.admit(graph, false, true);
        assert_ne!(result.status, Status::Pass);
        assert!(!result.diagnostics.is_empty());
        assert!(
            architecture::observed_graphs(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
                .unwrap()
                .is_empty(),
            "{fault}"
        );
        assert_eq!(
            architecture::feedback_results(&fixture.ledger, &fixture.setup.run_id, &fixture.key)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn a_previous_controller_checker_cannot_be_presented_as_current_architecture_policy() {
    let mut fixture = Fixture::new();
    let previous = Digest::of(b"previous normalizer");
    fixture
        .setup
        .registry
        .commands
        .iter_mut()
        .find(|command| command.id == fixture.spec.command_id)
        .unwrap()
        .checker_digest = previous.clone();
    fixture
        .setup
        .plan
        .checks
        .iter_mut()
        .find(|check| check.check_id == fixture.spec.check_id)
        .unwrap()
        .checker_digest = previous;
    assert!(
        architecture::protected_inputs(&fixture.ledger, &fixture.setup)
            .unwrap_err()
            .message
            .contains("normalizer")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn exact_architecture_policy_is_mandatory_context_only_while_oracle_approval_is_current() {
    use crate::approvals::{ApprovalRequest, Principal};
    let fixture = Fixture::new();
    let ledger = &fixture.ledger;
    let setup = &fixture.setup;
    assert!(architecture::policy_context(ledger, setup, &setup.base).is_err());
    let now = runner::now_ms().unwrap();
    let principal = Principal::current().unwrap();
    let approval = ledger
        .grant(
            &principal,
            &setup.authority,
            ApprovalRequest {
                run_id: Some(setup.run_id.clone()),
                subject_digest: setup.oracle.digest().unwrap(),
                approval_type: "oracle".into(),
                policy_digest: Digest::of_json(&setup.policy).unwrap(),
                scope_digest: Digest::of_json(&setup.contract.scope).unwrap(),
                expires_ms: None,
            },
            now,
        )
        .unwrap();
    let items = architecture::policy_context(ledger, setup, &setup.base).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].mandatory);
    assert_eq!(items[0].trust, crate::context::Trust::ApprovedSpec);
    assert_eq!(
        items[0].text.as_bytes(),
        ledger.read(&items[0].artifact_digest).unwrap()
    );
    let controller = crate::controller::Controller {
        ledger: Ledger::open(ledger.root()).unwrap(),
        backend: crate::runner::ContainerBackend::management(),
    };
    let context = controller.context(setup, &setup.base).unwrap();
    assert!(context
        .items
        .iter()
        .any(|item| item.item_id == items[0].item_id
            && item.mandatory
            && item.text == items[0].text));
    assert!(crate::context::build(ledger, items, 1).is_err());
    ledger
        .revoke(
            &principal,
            &setup.authority,
            &approval.approval_id,
            "policy changed",
            now + 1,
        )
        .unwrap();
    assert!(architecture::policy_context(ledger, setup, &setup.base).is_err());
    assert!(controller.context(setup, &setup.base).is_err());
}
