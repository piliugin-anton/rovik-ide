#![cfg(target_os = "linux")]
mod support;
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    broker::{FileEdit, Proposal},
    controller::{Controller, Integration},
    fixtures,
    identity::Digest,
    ledger::Ledger,
    pack_protocol::{Operation, Request, Response},
    profile::Language,
    provider::{ModelRequest, ModelResponse, Provider, Usage, WorkerTurn},
    runner::{self, Cancellation, DockerBackend},
    runtime::{self, TestInput, TestOutput},
    state::State,
    wire, Result,
};
const LANGUAGES: [Language; 3] = [Language::JavaScript, Language::TypeScript, Language::Python];

#[test]
fn pytest_failure_text_accepts_xml_escapes_but_not_external_entities() {
    let report = br#"<testsuites><testsuite tests="1" failures="1" errors="0" skipped="0"><testcase name="test_reserve[reserve-exact]"><failure message="comparison failed">assert amount &lt; remaining &amp; value &#62; 0</failure></testcase></testsuite></testsuites>"#;
    let parsed = rovik_harness::normalizer::parse_junit(report).unwrap();
    assert_eq!(parsed.failed, ["test_reserve[reserve-exact]"]);
    let bad = String::from_utf8(report.to_vec())
        .unwrap()
        .replace("&lt;", "&external;");
    assert!(rovik_harness::normalizer::parse_junit(bad.as_bytes()).is_err());
}
fn request(ledger: &Ledger, setup: &rovik_harness::controller::TaskSetup) -> Request {
    wire::decode(
        &ledger
            .read(&setup.oracle.files["requests/quota.json"].content.digest)
            .unwrap(),
    )
    .unwrap()
}
fn reply(request: &Request, result: &TestOutput) -> Vec<u8> {
    let response = Response {
        protocol_version: 1,
        request_id: request.request_id.clone(),
        ok: true,
        result: Some(serde_json::to_value(result).unwrap()),
        error: None,
    };
    let mut bytes = wire::canonical(&response).unwrap();
    bytes.push(b'\n');
    bytes
}
fn report(language: Language, cases: &[String]) -> String {
    let body = cases
        .iter()
        .map(|case| format!("<testcase name=\"{case}\"/>"))
        .collect::<String>();
    if language == Language::Python {
        format!(
            "<testsuite tests=\"{}\" failures=\"0\" errors=\"0\" skipped=\"0\">{body}</testsuite>",
            cases.len()
        )
    } else {
        format!("<testsuites>{body}<!-- tests {} --><!-- suites 0 --><!-- pass {} --><!-- fail 0 --><!-- cancelled 0 --><!-- skipped 0 --><!-- todo 0 --><!-- duration_ms 1 --></testsuites>", cases.len(), cases.len())
    }
}
#[test]
fn runtime_evidence_requires_protected_profiles_versions_cases_and_complete_reports() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    for language in LANGUAGES {
        let setup =
            fixtures::quota_runtime(&ledger, "runtime", Digest::of(b"image"), "base", language)
                .unwrap();
        setup.validate(&ledger).unwrap();
        let request = request(&ledger, &setup);
        let input = runtime::input(&request).unwrap();
        let native_input = rovik_harness::pack_protocol::NativeInput {
            snapshot_handle: input.snapshot_handle.clone(),
            profile: input.profile.clone(),
            configuration_id: input.configuration_id.clone(),
            forbidden_edges: vec![],
            required_test_ids: Default::default(),
            typescript_project: None,
        };
        let graph = rovik_harness::pack_protocol::Graph {
            precision: rovik_harness::pack_protocol::Precision::ConfiguredBuild,
            nodes: vec![],
            edges: vec![],
            unresolved: vec![],
            configuration_digest: input.validate().unwrap().digest().unwrap(),
            compiler_commands: vec![],
            test_inventory: vec![],
        };
        assert!(graph
            .validate(&native_input)
            .unwrap_err()
            .0
            .contains("precision"));
        let configuration = input.validate().unwrap().digest().unwrap();
        let versions = input
            .profile
            .facts
            .iter()
            .filter(|(key, _)| key.ends_with("_version"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let result = TestOutput {
            language,
            configuration_digest: configuration.clone(),
            versions,
            exit_code: 0,
            report: report(language, &setup.plan.checks[0].expected_case_ids),
        };
        assert_eq!(
            runtime::verify(&request, &reply(&request, &result), &configuration, Some(0))
                .unwrap()
                .inventory
                .executed(),
            7
        );
        let mut bad = result.clone();
        bad.versions.values_mut().for_each(|v| *v = "0.0.0".into());
        assert!(
            runtime::verify(&request, &reply(&request, &bad), &configuration, Some(0)).is_err()
        );
        let mut bad = result.clone();
        bad.report = report(language, &[]);
        assert!(
            runtime::verify(&request, &reply(&request, &bad), &configuration, Some(0)).is_err()
        );
        let mut bad = result.clone();
        bad.report = report(language, &setup.plan.checks[0].expected_case_ids[1..]);
        assert!(
            runtime::verify(&request, &reply(&request, &bad), &configuration, Some(0)).is_err()
        );
        assert!(runtime::verify(
            &request,
            &reply(&request, &result),
            &Digest::of(b"wrong-config"),
            Some(0)
        )
        .is_err());
        assert!(
            runtime::verify(&request, &reply(&request, &result), &configuration, Some(1)).is_err()
        );
        let mut altered = request.clone();
        altered.operation = Operation::Index;
        assert!(
            runtime::verify(&altered, &reply(&request, &result), &configuration, Some(0)).is_err()
        );
        let mut setup = setup.clone();
        setup.oracle.files.remove(&input.test_files[0]);
        setup.plan.oracle_digest = setup.oracle.digest().unwrap();
        assert!(setup.validate(&ledger).is_err());
    }
}
#[test]
fn runtime_profiles_and_build_inputs_cannot_silently_weaken_the_selected_policy() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    for language in LANGUAGES {
        let setup =
            fixtures::quota_runtime(&ledger, "runtime", Digest::of(b"image"), "base", language)
                .unwrap();
        let mut input: TestInput = runtime::input(&request(&ledger, &setup)).unwrap();
        input.profile.configurations[0].runtime = "browser".into();
        assert!(input.validate().is_err());
        input.profile = setup.profiles[0].clone();
        input.test_files = vec!["../candidate/test.mjs".into()];
        assert!(input.validate().is_err());
        input.test_files = vec![if language == Language::Python {
            "test.py"
        } else {
            "test.mjs"
        }
        .into()];
        if language == Language::TypeScript {
            input.profile.configurations[0].flags.clear();
            assert!(input.validate().is_err());
        }
    }
    for path in [
        "pyproject.toml",
        "requirements.lock",
        "requirements-dev.txt",
        "nested/tsconfig.build.json",
        ".npmrc",
        "pytest.ini",
        "conftest.py",
        "setup.cfg",
        "uv.lock",
        "pnpm-lock.yaml",
    ] {
        assert!(rovik_harness::snapshot::is_build_input(path), "{path}");
    }
}
struct Repository;
impl Integration for Repository {
    fn current_revision(&self, _: &str) -> Result<String> {
        Ok("fixture-base".into())
    }
}
struct Worker {
    path: String,
    source: String,
    calls: u32,
}
impl Provider for Worker {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        self.calls += 1;
        Ok(ModelResponse {
            request_id: request.request_id.clone(),
            provider_response_id: Some("scripted-runtime-fixture".into()),
            output: serde_json::to_string(&WorkerTurn {
                schema_version: 1,
                request_id: request.request_id.clone(),
                tool_requests: vec![],
                questions: vec![],
                proposal: Some(Proposal {
                    base_digest: request.snapshot_digest.clone(),
                    changes: vec![FileEdit {
                        path: self.path.clone(),
                        content: Some(self.source.clone()),
                        mode: 0o100644,
                    }],
                    summary: "Allow an exact reservation.".into(),
                    requirement_mapping: ["Q-1", "Q-2", "Q-3"]
                        .iter()
                        .map(|id| (id.to_string(), vec![self.path.clone()]))
                        .collect(),
                }),
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
#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned runtime image"]
fn all_runtime_languages_reject_behavioral_mutants_then_accept_through_the_full_controller() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    for language in LANGUAGES {
        let dir = tempfile::tempdir().unwrap();
        let mut controller = Controller {
            ledger: Ledger::open(&dir.path().join("state")).unwrap(),
            backend: DockerBackend::local(&support::docker_path()).unwrap(),
        };
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let setup = fixtures::quota_runtime(
            &controller.ledger,
            "quota-runtime",
            image,
            "fixture-base",
            language,
        )
        .unwrap();
        controller
            .create(&setup, runner::now_ms().unwrap())
            .unwrap();
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
        let (path, source) = fixtures::runtime_quota_source(language).unwrap();
        let mut worker = Worker {
            path: path.into(),
            source: source.into(),
            calls: 0,
        };
        let report = controller
            .execute(
                &setup.run_id,
                &mut worker,
                &Repository,
                &Cancellation::default(),
            )
            .unwrap();
        if report.state != State::Accepted {
            if let Some(binding) = controller.ledger.run(&setup.run_id).unwrap().binding_digest {
                for result in controller
                    .ledger
                    .admitted_results(&setup.run_id, &binding)
                    .unwrap()
                {
                    eprintln!("{result:?}");
                    for digest in result.evidence {
                        eprintln!(
                            "{}",
                            String::from_utf8_lossy(&controller.ledger.read(&digest).unwrap())
                        );
                    }
                }
            }
        }
        assert_eq!(report.state, State::Accepted, "{language:?}: {report:?}");
        assert_eq!(worker.calls, 1);
        assert_eq!(
            controller.ledger.budgets(&setup.run_id).unwrap()["checks"].spent,
            2
        );
    }
}
#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned runtime image"]
fn compiler_failures_wrong_versions_and_corrupt_packages_never_count_as_test_failures() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    for language in LANGUAGES {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("state")).unwrap();
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let mut setup =
            fixtures::quota_runtime(&ledger, "runtime-errors", image, "base", language).unwrap();
        let mut request = request(&ledger, &setup);
        if language == Language::TypeScript {
            let (path, source) = fixtures::runtime_quota_source(language).unwrap();
            let bad =
                format!("{source}\nconst invalid: number = 'must fail strict compilation';\n");
            setup.base.files.get_mut(path).unwrap().content = ledger
                .store(
                    "source",
                    "text/plain",
                    bad.as_bytes(),
                    runner::now_ms().unwrap(),
                )
                .unwrap();
        } else if language == Language::JavaScript {
            request.payload["profile"]["facts"]["node_version"] = "0.0.0".into();
            let serialized = format!("{}\n", serde_json::to_string(&request).unwrap());
            setup
                .oracle
                .files
                .get_mut("requests/quota.json")
                .unwrap()
                .content = ledger
                .store(
                    "source",
                    "application/json",
                    serialized.as_bytes(),
                    runner::now_ms().unwrap(),
                )
                .unwrap();
        } else {
            setup
                .dependencies
                .files
                .values_mut()
                .next()
                .unwrap()
                .content = ledger
                .store(
                    "source",
                    "application/octet-stream",
                    b"corrupt wheel",
                    runner::now_ms().unwrap(),
                )
                .unwrap();
            setup.environment.dependency_digest = setup.dependencies.digest().unwrap();
        }
        let spec = &setup.plan.checks[0];
        let observation = DockerBackend::local(&support::docker_path())
            .unwrap()
            .execute(
                &ledger,
                &setup.base,
                &setup.oracle,
                &setup.dependencies,
                &setup.environment,
                &setup.registry,
                spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert!(!observation.incomplete());
        assert_eq!(
            observation.exit_code(),
            Some(2),
            "{language:?}: {}\n{}",
            String::from_utf8_lossy(observation.stdout()),
            String::from_utf8_lossy(observation.stderr())
        );
        assert!(runtime::verify(
            &request,
            observation.stdout(),
            &spec.configuration_digest,
            observation.exit_code()
        )
        .is_err());
    }
}
