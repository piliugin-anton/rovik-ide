#![cfg(target_os = "linux")]
use rovik_harness::{
    approvals::Principal,
    controller::Controller,
    fixtures,
    identity::Digest,
    ledger::Ledger,
    profile::Language,
    provider::{ModelRequest, ModelResponse, Provider, Usage, WorkerTurn},
    runner::{Cancellation, ContainerBackend},
    study::{Arm, Stage, Study, StudyCase},
    wire, Result,
};
fn controller(path: &std::path::Path) -> Controller {
    Controller {
        ledger: Ledger::open(path).unwrap(),
        backend: ContainerBackend::local(std::path::Path::new("/bin/true")).unwrap(),
    }
}
fn study(c: &Controller) -> Study {
    let image = Digest::of(b"test-image");
    let mut cases = vec![];
    let mut worker = None;
    let mut budgets = None;
    for language in [
        Language::Rust,
        Language::C,
        Language::Cpp,
        Language::JavaScript,
        Language::TypeScript,
        Language::Python,
    ] {
        let id = format!("{language:?}");
        let config = match language {
            Language::Rust => fixtures::quota_rust(&c.ledger, &id, image.clone(), "base"),
            Language::C | Language::Cpp => {
                fixtures::quota_c_family(&c.ledger, &id, image.clone(), "base", language)
            }
            _ => fixtures::quota_runtime(&c.ledger, &id, image.clone(), "base", language),
        }
        .unwrap();
        worker = Some(config.worker.clone());
        budgets = Some(config.budgets.clone());
        let mut hidden = config.clone();
        let marker = c
            .ledger
            .store("source", "text/plain", b"HELDOUT_SECRET_SENTINEL", 0)
            .unwrap();
        hidden.oracle.files.insert(
            "independent-test-data.txt".into(),
            rovik_harness::snapshot::FileEntry {
                mode: 0o100644,
                content: marker,
            },
        );
        hidden.plan.oracle_digest = hidden.oracle.digest().unwrap();
        let public = c.ledger.store_json("operation", &config, 0).unwrap().digest;
        let heldout = c.ledger.store_json("operation", &hidden, 0).unwrap().digest;
        let request = c
            .ledger
            .store(
                "context",
                "text/plain",
                b"Repair exact capacity reservation without adding an abstraction.",
                0,
            )
            .unwrap()
            .digest;
        let instructions = c
            .ledger
            .store(
                "context",
                "text/plain",
                b"Use the repository's native conventions.",
                0,
            )
            .unwrap()
            .digest;
        cases.push(StudyCase {
            case_id: id,
            language,
            workload: "protocol-smoke; not a representative benchmark".into(),
            risk_group: "simple".into(),
            no_new_abstraction: true,
            stages: vec![Stage {
                setup: public,
                heldout,
                request,
                existing_instructions: instructions,
                existing_checks: config
                    .plan
                    .required_final()
                    .map(|check| check.check_id.clone())
                    .collect(),
            }],
        });
    }
    Study {
        schema_version: 1,
        study_id: "smoke".into(),
        dataset_version: "scripted-protocol-smoke-v1".into(),
        repetitions: 2,
        worker: worker.unwrap(),
        budgets: budgets.unwrap(),
        maximum_proposals: 1,
        maximum_duration_ms: 30_000,
        maximum_total_cost: 36_000_000,
        cases,
    }
}
#[derive(Default)]
struct Questions {
    calls: u64,
    baseline: u64,
    guided: u64,
}
impl Provider for Questions {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        self.calls += 1;
        assert!(!String::from_utf8(wire::canonical(request).unwrap())
            .unwrap()
            .contains("HELDOUT_SECRET_SENTINEL"));
        if request
            .context
            .items
            .iter()
            .any(|item| item.item_id == "contract")
        {
            self.guided += 1;
        } else {
            self.baseline += 1;
            assert!(!request
                .context
                .items
                .iter()
                .any(|item| item.item_id == "design" || item.item_id == "profiles"));
        }
        Ok(ModelResponse {
            request_id: request.request_id.clone(),
            provider_response_id: Some("scripted-smoke".into()),
            finish_reason: "stop".into(),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 10,
                cached_tokens: None,
                cost_microunits: Some(1),
            }),
            output: serde_json::to_string(&WorkerTurn {
                schema_version: 1,
                request_id: request.request_id.clone(),
                tool_requests: vec![],
                proposal: None,
                questions: vec!["Domain decision requires review".into()],
            })
            .unwrap(),
        })
    }
}
#[test]
fn study_preflight_rejects_unknown_spend_leaked_holdouts_and_missing_native_checks() {
    let dir = tempfile::tempdir().unwrap();
    let c = controller(&dir.path().join("state"));
    let mut data = study(&c);
    let principal = Principal::current().unwrap();
    assert_eq!(data.preflight(&c, &principal).unwrap(), 36_000_000);
    data.maximum_total_cost = 35_999_999;
    assert!(data.preflight(&c, &principal).is_err());
    data.maximum_total_cost = 36_000_000;
    let hidden = data.cases[0].stages[0].heldout.clone();
    data.cases[0].stages[0].heldout = data.cases[0].stages[0].setup.clone();
    assert!(data.preflight(&c, &principal).is_err());
    data.cases[0].stages[0].heldout = hidden;
    data.cases[0].stages[0]
        .existing_checks
        .insert("invented-check".into());
    assert!(data.preflight(&c, &principal).is_err());
}
#[test]
fn every_language_uses_equal_budgets_withheld_context_and_actual_provider_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let data = study(&c);
    let mut provider = Questions::default();
    let mut report = c
        .evaluate_study(
            &Principal::current().unwrap(),
            &data,
            &mut provider,
            &Cancellation::default(),
        )
        .unwrap();
    assert!(report.complete);
    assert_eq!(provider.calls, 36);
    assert_eq!(provider.baseline, 12);
    assert_eq!(provider.guided, 24);
    assert!(report
        .trials
        .iter()
        .all(|trial| trial.spent["model_calls"] == 1
            && trial.spent["cost_microunits"] == 1
            && !trial.accepted_by_arm
            && trial.heldout_correct.is_none()
            && trial.decision_requests == 1));
    assert!(report
        .trials
        .iter()
        .all(|trial| c.ledger.run(&trial.run_id).unwrap().state
            == rovik_harness::state::State::Stopped));
    let summary = c.summarize_study(&data, &report).unwrap();
    let mut edited = report.clone();
    edited.trials[0].duration_ms += 1;
    assert!(c.summarize_study(&data, &edited).is_err());
    let mut edited = report.clone();
    edited.trials[0].decision_requests += 1;
    assert!(c.summarize_study(&data, &edited).is_err());
    assert_eq!(summary.len(), 18);
    assert!(summary.iter().all(|group| group.success.count == 0
        && group.success.total == 2
        && group.cost_microunits == 2
        && group.cost_per_success.is_none()
        && group.escaped_defects.estimate.is_none()));
    assert!(c
        .evaluate_study(
            &Principal::current().unwrap(),
            &data,
            &mut provider,
            &Cancellation::default()
        )
        .is_err());
    assert_eq!(provider.calls, 36);
    let removed = report.trials.pop().unwrap();
    assert!(report.summarize(&data).is_err());
    report.trials.push(removed.clone());
    report.trials.push(removed);
    assert!(report.summarize(&data).is_err());
    report.trials.pop();
    report
        .trials
        .iter_mut()
        .find(|trial| trial.arm == Arm::Enforced)
        .unwrap()
        .reserved
        .insert("tokens".into(), 1);
    assert!(report.summarize(&data).is_err());
}

struct Unbilled {
    calls: u64,
}
impl Provider for Unbilled {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        self.calls += 1;
        Ok(ModelResponse {
            request_id: request.request_id.clone(),
            provider_response_id: None,
            output: "{}".into(),
            finish_reason: "stop".into(),
            usage: None,
        })
    }
}
#[test]
fn a_policy_block_keeps_the_exact_proposal_before_independent_native_judgment() {
    use rovik_harness::broker::{FileEdit, Proposal};
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let mut data = study(&c);
    data.cases.truncate(1);
    struct UnsafeProposal(u32);
    impl Provider for UnsafeProposal {
        fn generate(
            &mut self,
            request: &ModelRequest,
            cancel: &Cancellation,
        ) -> Result<ModelResponse> {
            let mut response = Questions::default().generate(request, cancel)?;
            self.0 += 1;
            if self.0 != 3 {
                return Ok(response);
            }
            response.output = serde_json::to_string(&WorkerTurn {
                schema_version: 1,
                request_id: request.request_id.clone(),
                tool_requests: vec![],
                questions: vec![],
                proposal: Some(Proposal {
                    base_digest: request.snapshot_digest.clone(),
                    changes: vec![FileEdit {
                        path: "implementations/quota/lib.rs".into(),
                        content: Some(format!("{}\nfn unchecked() {{ unsafe {{ std::hint::unreachable_unchecked() }} }}\n", fixtures::QUOTA_RUST)),
                        mode: 0o100644,
                    }],
                    summary: "Deliberately introduce an unsupported unsafe obligation".into(),
                    requirement_mapping: [
                        ("Q-1".into(), vec!["implementation".into()]),
                        ("Q-2".into(), vec!["implementation".into()]),
                        ("Q-3".into(), vec!["implementation".into()]),
                    ].into(),
                }),
            }).unwrap();
            Ok(response)
        }
    }
    let report = c
        .evaluate_study(
            &Principal::current().unwrap(),
            &data,
            &mut UnsafeProposal(0),
            &Cancellation::default(),
        )
        .unwrap();
    // The unavailable test backend leaves independent judgment/accounting unknown.
    // No fourth provider call is allowed, but the blocked proposal must remain auditable.
    assert!(!report.complete);
    assert_eq!(report.trials.len(), 3);
    assert_eq!(
        report
            .trials
            .iter()
            .filter(|trial| trial.arm == Arm::Enforced)
            .count(),
        1
    );
    for trial in report
        .trials
        .iter()
        .filter(|trial| trial.arm == Arm::Enforced)
    {
        assert!(!trial.accepted_by_arm);
        assert!(trial.candidate.is_some());
        assert!(trial.results.is_empty());
        assert!(
            trial
                .reason
                .as_ref()
                .is_some_and(|reason| reason.contains("unsafe")),
            "{trial:?}"
        );
        assert_eq!(
            c.current_candidate(&trial.run_id)
                .unwrap()
                .unwrap()
                .digest()
                .unwrap(),
            *trial.candidate.as_ref().unwrap()
        );
    }
    assert!(c.summarize_study(&data, &report).is_err());
}

#[test]
fn unknown_provider_usage_stops_the_whole_study_before_another_billable_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let data = study(&c);
    let mut provider = Unbilled { calls: 0 };
    let report = c
        .evaluate_study(
            &Principal::current().unwrap(),
            &data,
            &mut provider,
            &Cancellation::default(),
        )
        .unwrap();
    assert!(!report.complete);
    assert_eq!(provider.calls, 1);
    assert_eq!(report.trials.len(), 1);
    assert_eq!(report.trials[0].reserved["model_calls"], 1);
    assert!(report.summarize(&data).is_err());
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned runtime image"]
fn live_provider_protocol_records_real_native_escapes_under_weak_ci_and_rejection_under_enforcement(
) {
    use rovik_harness::{
        broker::Proposal, controller::TaskSetup, evidence::Status, pack_protocol::Request, packs,
        runtime::TestInput,
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let docker = std::env::var("ROVIK_HARNESS_DOCKER_BIN")
        .unwrap_or_else(|_| "/usr/local/bin/docker".into());
    let dir = tempfile::tempdir().unwrap();
    let mut c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(std::path::Path::new(&docker)).unwrap(),
    };
    let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
    let mut config =
        fixtures::quota_runtime(&c.ledger, "native-study", image, "base", Language::Python)
            .unwrap();
    let mut hidden = config.clone();
    let update = |setup: &mut TaskSetup, path: &str, bytes: &[u8]| {
        let mut bytes = bytes.to_vec();
        if path.starts_with("requests/") && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let content = c
            .ledger
            .store("source", "application/octet-stream", &bytes, 0)
            .unwrap();
        setup.oracle.files.insert(
            path.into(),
            rovik_harness::snapshot::FileEntry {
                mode: 0o100644,
                content,
            },
        );
    };
    let mut request: Request = wire::decode(
        &c.ledger
            .read(&config.oracle.files["requests/quota.json"].content.digest)
            .unwrap(),
    )
    .unwrap();
    let mut input: TestInput = serde_json::from_value(request.payload.clone()).unwrap();
    let hidden_case = "test_reserve[heldout-reserve-exact-17]".to_string();
    input.required_test_ids.insert(hidden_case.clone());
    request.payload = serde_json::to_value(input).unwrap();
    update(
        &mut hidden,
        "requests/quota.json",
        &wire::canonical(&request).unwrap(),
    );
    let mut cases: serde_json::Value = wire::decode(
        &c.ledger
            .read(&hidden.oracle.files["cases.json"].content.digest)
            .unwrap(),
    )
    .unwrap();
    let mut extra = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "reserve-exact")
        .unwrap()
        .clone();
    extra["id"] = "heldout-reserve-exact-17".into();
    extra["before"]["remaining"] = "17".into();
    extra["request"]["amount"] = "17".into();
    cases["cases"].as_array_mut().unwrap().push(extra);
    update(&mut hidden, "cases.json", &wire::canonical(&cases).unwrap());
    hidden.plan.checks[0]
        .expected_case_ids
        .push(hidden_case.clone());
    hidden.plan.checks[0].minimum_test_count = 8;
    hidden
        .suites
        .values_mut()
        .next()
        .unwrap()
        .insert(hidden_case.clone());
    hidden.baseline.expected_failures[0]
        .case_ids
        .insert(hidden_case);
    hidden.plan.oracle_digest = hidden.oracle.digest().unwrap();
    hidden.validate(&c.ledger).unwrap();
    let command = packs::runtime_test_command(["quota".into(), "example".into()].into()).unwrap();
    config.registry.commands = vec![command];
    config.environment.registry_digest = Digest::of_json(&config.registry).unwrap();
    let mut request: Request = wire::decode(
        &c.ledger
            .read(&config.oracle.files["requests/quota.json"].content.digest)
            .unwrap(),
    )
    .unwrap();
    let mut input: TestInput = serde_json::from_value(request.payload.clone()).unwrap();
    input.test_files = vec!["tests/test_example.py".into()];
    input.required_test_ids = ["test_example".into()].into();
    request.request_id = "example".into();
    request.payload = serde_json::to_value(input).unwrap();
    update(
        &mut config,
        "requests/example.json",
        &wire::canonical(&request).unwrap(),
    );
    update(&mut config,"tests/test_example.py",b"from implementations.quota.quota import reserve\ndef test_example():\n    assert reserve(5, 1)['remaining'] == 4\n");
    let mut example = config.plan.checks[0].clone();
    example.check_id = "public-example".into();
    example.expected_case_ids = vec!["test_example".into()];
    example.minimum_test_count = 1;
    example.obligation_ids.clear();
    example
        .validated_parameters
        .insert("request_id".into(), "example".into());
    config.plan.checks.push(example);
    config.plan.oracle_digest = config.oracle.digest().unwrap();
    config.validate(&c.ledger).unwrap();
    let source = c.ledger.store_json("operation", &config, 0).unwrap().digest;
    let heldout = c.ledger.store_json("operation", &hidden, 0).unwrap().digest;
    let request = c
        .ledger
        .store(
            "context",
            "text/plain",
            b"Repair exact-capacity reservations",
            0,
        )
        .unwrap()
        .digest;
    let instructions = c
        .ledger
        .store("context", "text/plain", b"Preserve the public API", 0)
        .unwrap()
        .digest;
    let data = Study {
        schema_version: 1,
        study_id: "native-protocol-smoke".into(),
        dataset_version: "scripted-native-smoke-v1; not model-quality evidence".into(),
        repetitions: 2,
        worker: config.worker.clone(),
        budgets: config.budgets.clone(),
        maximum_proposals: 1,
        maximum_duration_ms: 300_000,
        maximum_total_cost: 6_000_000,
        cases: vec![StudyCase {
            case_id: "boundary".into(),
            language: Language::Python,
            workload: "repair".into(),
            risk_group: "simple".into(),
            no_new_abstraction: true,
            stages: vec![Stage {
                setup: source,
                heldout,
                request,
                existing_instructions: instructions,
                existing_checks: ["public-example".into()].into(),
            }],
        }],
    };
    struct Wrong;
    impl Provider for Wrong {
        fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
            assert!(!String::from_utf8(wire::canonical(request).unwrap())
                .unwrap()
                .contains("heldout-reserve-exact-17"));
            Ok(ModelResponse {
                request_id: request.request_id.clone(),
                provider_response_id: Some("scripted-wrong-proposal".into()),
                finish_reason: "stop".into(),
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 10,
                    cached_tokens: None,
                    cost_microunits: Some(1),
                }),
                output: serde_json::to_string(&WorkerTurn {
                    schema_version: 1,
                    request_id: request.request_id.clone(),
                    tool_requests: vec![],
                    questions: vec![],
                    proposal: Some(Proposal {
                        base_digest: request.snapshot_digest.clone(),
                        changes: vec![],
                        summary: "Deliberately retain the boundary defect".into(),
                        requirement_mapping: [
                            ("Q-1".into(), vec!["implementation".into()]),
                            ("Q-2".into(), vec!["implementation".into()]),
                            ("Q-3".into(), vec!["implementation".into()]),
                        ]
                        .into(),
                    }),
                })
                .unwrap(),
            })
        }
    }
    let report = c
        .evaluate_study(
            &Principal::current().unwrap(),
            &data,
            &mut Wrong,
            &Cancellation::default(),
        )
        .unwrap();
    assert!(report.complete, "{report:?}");
    assert_eq!(report.trials.len(), 6);
    for trial in &report.trials {
        if trial.heldout_correct != Some(false) {
            for (run, ids) in [
                (trial.run_id.clone(), &trial.results),
                (format!("{}-heldout", trial.run_id), &trial.heldout_results),
            ] {
                for id in ids {
                    let result = c.ledger.result(&run, id).unwrap();
                    eprintln!("native observation: {result:?}");
                    for evidence in &result.evidence {
                        eprintln!(
                            "raw evidence: {}",
                            String::from_utf8_lossy(&c.ledger.read(evidence).unwrap())
                        );
                    }
                }
            }
        }
        assert_eq!(trial.heldout_correct, Some(false), "{trial:?}");
        assert_eq!(
            trial.accepted_by_arm,
            trial.arm != Arm::Enforced,
            "{trial:?}"
        );
        for id in &trial.heldout_results {
            let result = c
                .ledger
                .result(&format!("{}-heldout", trial.run_id), id)
                .unwrap();
            assert_eq!(result.status, Status::Fail);
            assert!(result
                .failed_case_ids
                .contains(&"test_reserve[heldout-reserve-exact-17]".into()));
        }
    }
    assert!(report
        .trials
        .iter()
        .all(|trial| c.ledger.run(&trial.run_id).unwrap().state
            == rovik_harness::state::State::Stopped));
    let summary = c.summarize_study(&data, &report).unwrap();
    assert!(summary
        .iter()
        .filter(|group| group.arm != Arm::Enforced)
        .all(|group| group.escaped_defects.count == 2));
    let mut forged = report.clone();
    forged.trials[0].spent.insert("cost_microunits".into(), 0);
    assert!(c.summarize_study(&data, &forged).is_err());
    let mut forged = report;
    forged.trials[0].heldout_correct = Some(true);
    assert!(c.summarize_study(&data, &forged).is_err());
}
