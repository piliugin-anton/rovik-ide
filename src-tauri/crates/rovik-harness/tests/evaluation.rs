mod support;
use rovik_harness::{
    evaluation::{self, Evaluation, Observation, Outcome, Strategy},
    identity::Digest,
    profile::Language,
};
#[test]
fn repeated_evaluation_keeps_language_regressions_and_uncertainty_visible() {
    let bank = evaluation::reference_bank();
    let environment = Digest::of(b"environment");
    let mut observations = vec![];
    for case in &bank.cases {
        for repetition in 0..bank.repetitions {
            for strategy in [
                Strategy::CompileOnly,
                Strategy::ExampleOnly,
                Strategy::ProtectedContract,
            ] {
                observations.push(Observation {
                    case_id: case.case_id.clone(),
                    repetition,
                    strategy,
                    outcome: if !case.expected_correct && strategy == Strategy::ProtectedContract {
                        Outcome::Rejected
                    } else {
                        Outcome::Accepted
                    },
                    executed_cases: case.required_cases,
                    duration_ms: 1,
                });
            }
        }
    }
    let mut report = Evaluation {
        schema_version: 1,
        bank_digest: Digest::of_json(&bank).unwrap(),
        environment_digest: environment.clone(),
        methodology: "native-verifier-ablation; no model calls".into(),
        observations,
    };
    let summary = report.summarize(&bank, &environment).unwrap();
    assert_eq!(summary.len(), 9);
    assert!(summary
        .iter()
        .filter(|s| s.strategy == Strategy::ProtectedContract)
        .all(|s| s.metrics.false_accepts == 0));
    let bad = bank
        .cases
        .iter()
        .find(|c| c.language == Language::C && !c.expected_correct)
        .unwrap();
    report
        .observations
        .iter_mut()
        .find(|r| r.case_id == bad.case_id && r.strategy == Strategy::ProtectedContract)
        .unwrap()
        .outcome = Outcome::Accepted;
    let summary = report.summarize(&bank, &environment).unwrap();
    assert_eq!(
        summary
            .iter()
            .find(|s| s.language == Language::C && s.strategy == Strategy::ProtectedContract)
            .unwrap()
            .metrics
            .false_accepts,
        1
    );
    assert_eq!(
        summary
            .iter()
            .find(|s| s.language == Language::Rust && s.strategy == Strategy::ProtectedContract)
            .unwrap()
            .metrics
            .false_accepts,
        0
    );
    let removed = report.observations.pop().unwrap();
    assert!(report.summarize(&bank, &environment).is_err());
    report.observations.push(removed.clone());
    report.observations.push(removed);
    assert!(report.summarize(&bank, &environment).is_err());
    report.observations.pop();
    assert!(report
        .summarize(&bank, &Digest::of(b"other image"))
        .is_err());
    report
        .observations
        .iter_mut()
        .find(|r| r.strategy == Strategy::ProtectedContract && r.outcome == Outcome::Accepted)
        .unwrap()
        .executed_cases = 0;
    assert!(report.summarize(&bank, &environment).is_err());
    assert!(report
        .summarize(&bank, &Digest::of(b"other image"))
        .is_err());
}
#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn heldout_native_contracts_report_repeated_false_accepts_by_language_and_strategy() {
    use rovik_harness::{
        fixtures,
        identity::EvidenceKey,
        ledger::Ledger,
        normalizer, packs,
        registry::{CommandRegistry, Parser},
        runner::{self, Cancellation, DockerBackend, ExecutionInput},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut setup = fixtures::quota_rust(
        &ledger,
        "evaluation",
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        "bank-v1",
    )
    .unwrap();
    let bank = evaluation::reference_bank();
    let bank_text = serde_json::to_string(&bank).unwrap();
    let mut command = packs::graph_command(["evaluation".into()].into()).unwrap();
    command.executable = "/usr/bin/python3".into();
    command.argv_template = vec!["/oracle/run.py".into()];
    command.parameters.clear();
    command.parser = Parser::ExitCode;
    command.checker_digest = normalizer::checker_digest(command.parser);
    command.report_file = Some("/outputs/evaluation.json".into());
    let registry = CommandRegistry {
        commands: vec![command.clone()],
    };
    setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
    setup.environment.toolchain_digest=ledger.store_json("environment",&serde_json::json!({"image":setup.environment.image_digest,"rustc":"1.85.1","gcc":"12","c":"17","cpp":"23","target":"linux-x86_64"}),runner::now_ms().unwrap()).unwrap().digest;
    let environment = Digest::of_json(&setup.environment).unwrap();
    let environment_text = serde_json::json!({"digest":environment}).to_string();
    let oracle = fixtures::snapshot(
        &ledger,
        "protected-heldout-bank-v1",
        &[
            ("bank.json", &bank_text),
            ("environment.json", &environment_text),
            ("run.py", include_str!("../fixtures/evaluation/run.py")),
        ],
    )
    .unwrap();
    let mut spec = setup.plan.checks.remove(0);
    spec.command_id = command.id.clone();
    spec.checker_digest = command.checker_digest;
    spec.expected_case_ids.clear();
    spec.minimum_test_count = 0;
    spec.timeout_ms = command.limits.timeout_ms;
    spec.output_limit_bytes = command.limits.output_bytes;
    spec.configuration_digest=Digest::of_json(&serde_json::json!({"bank":Digest::of_json(&bank).unwrap(),"rust_edition":"2021","c_standard":"17","cpp_standard":"23","target":"linux-x86_64"})).unwrap();
    // Store the bank, observations and authenticated native receipt together.
    let now = runner::now_ms().unwrap();
    let request = ledger.store_json("evaluation", &bank, now).unwrap();
    let run = ledger
        .create_run(
            "bank-run",
            &request.digest,
            &[("checks".into(), 1)].into(),
            now,
        )
        .unwrap();
    let key = EvidenceKey {
        candidate_digest: ledger
            .store_json("snapshot", &setup.base, now)
            .unwrap()
            .digest,
        contract_digest: request.digest.clone(),
        design_digest: request.digest.clone(),
        policy_digest: ledger.store_json("policy", &registry, now).unwrap().digest,
        oracle_digest: ledger.store_json("snapshot", &oracle, now).unwrap().digest,
        plan_digest: ledger.store_json("plan", &spec, now).unwrap().digest,
        environment_digest: ledger
            .store_json("environment", &setup.environment, now)
            .unwrap()
            .digest,
    };
    let binding = ledger.store_json("operation", &key, now).unwrap();
    ledger.bind(&run, &binding.digest, now).unwrap();
    let input = ledger
        .store_json(
            "operation",
            &ExecutionInput {
                evidence_key: key.clone(),
                check: spec.clone(),
            },
            now,
        )
        .unwrap();
    let lease = ledger
        .claim(
            "bank-run",
            "native-bank",
            "evaluation",
            &input.digest,
            "fixture",
            now,
            1_000_000,
            &[("checks".into(), 1)].into(),
        )
        .unwrap();
    let observed = DockerBackend::local(&crate::support::docker_path())
        .unwrap()
        .execute_leased(
            &ledger,
            &setup.base,
            &oracle,
            &setup.dependencies,
            &setup.environment,
            &registry,
            &spec,
            &Cancellation::default(),
            &lease,
        )
        .unwrap();
    assert_eq!(
        observed.exit_code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(observed.stderr())
    );
    assert!(!observed.incomplete());
    let evaluation: Evaluation = rovik_harness::wire::decode(observed.report().unwrap()).unwrap();
    let strata = evaluation.summarize(&bank, &environment).unwrap();
    for stratum in &strata {
        assert_eq!(stratum.metrics.trials, 12, "{stratum:?}");
        assert_eq!(stratum.metrics.errors, 0, "{stratum:?}");
        assert_eq!(stratum.metrics.unsupported, 0);
        assert_eq!(stratum.metrics.false_rejects, 0, "{stratum:?}");
        assert_eq!(
            stratum.metrics.false_accepts,
            if stratum.strategy == Strategy::ProtectedContract {
                0
            } else {
                6
            },
            "{stratum:?}"
        );
    }
    let now = runner::now_ms().unwrap();
    let result = runner::admit_observation(
        &mut ledger,
        &lease,
        &key,
        &spec,
        &registry.commands[0],
        observed,
        now,
    )
    .unwrap();
    let result_ref = ledger.store_json("result", &result, now).unwrap();
    ledger
        .complete(
            &lease,
            &result_ref.digest,
            &[("checks".into(), 1)].into(),
            now,
        )
        .unwrap();
    assert_eq!(
        ledger
            .admitted_results("bank-run", &binding.digest)
            .unwrap()
            .len(),
        1
    );
    ledger.store_json("evaluation", &evaluation, now).unwrap();
    if let Ok(path) = std::env::var("ROVIK_HARNESS_EVALUATION_OUTPUT") {
        let report = serde_json::json!({"schema_version":1,"methodology":evaluation.methodology,"bank":bank,"bank_digest":evaluation.bank_digest,"environment":setup.environment,"oracle_digest":key.oracle_digest,"native_result":result,"strata":strata,"observations":evaluation.observations,"model_calls":0,"model_cost_microunits":0});
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
