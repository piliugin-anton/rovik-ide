use rovik_harness::{
    evidence::Status,
    identity::Digest,
    normalizer::{self, Inventory},
    plan::{CheckKind, CheckSpec, Phase},
    registry::{CommandDefinition, CommandRegistry, Limits, Parameter, Parser},
    wire,
};
use std::collections::BTreeMap;

fn command() -> CommandDefinition {
    CommandDefinition {
        schema_version: 1,
        id: "build".into(),
        executable: "/usr/bin/cmake".into(),
        argv_template: vec![
            "--build".into(),
            "{build}".into(),
            "--parallel".into(),
            "{jobs}".into(),
        ],
        parameters: [
            (
                "build".into(),
                Parameter::Path {
                    root: "/build".into(),
                },
            ),
            (
                "jobs".into(),
                Parameter::Integer {
                    minimum: 1,
                    maximum: 4,
                },
            ),
        ]
        .into(),
        cwd: "/build".into(),
        environment: BTreeMap::new(),
        kind: CheckKind::Static,
        parser: Parser::ExitCode,
        checker_digest: normalizer::checker_digest(Parser::ExitCode),
        report_file: None,
        limits: Limits {
            timeout_ms: 1000,
            output_bytes: 4096,
            memory_bytes: 64 * 1024 * 1024,
            processes: 16,
            cpu_millis: 1000,
            build_bytes: 16 * 1024 * 1024,
        },
    }
}

#[test]
fn registry_denies_unknown_commands_shell_parameters_and_parser_downgrades() {
    let definition = command();
    let parameters: BTreeMap<String, String> = [
        ("build".into(), "/build/default".into()),
        ("jobs".into(), "2".into()),
    ]
    .into();
    assert_eq!(
        definition.render(&parameters).unwrap(),
        [
            "/usr/bin/cmake",
            "--build",
            "/build/default",
            "--parallel",
            "2"
        ]
    );
    for value in ["2; touch /outputs/pwned", "$(id)", "-1", "5", "1\n2"] {
        let mut p = parameters.clone();
        p.insert("jobs".into(), value.into());
        assert!(definition.render(&p).is_err());
    }
    for value in ["/etc/passwd", "/build/../snapshot/source", "/build/a\\b"] {
        let mut p = parameters.clone();
        p.insert("build".into(), value.into());
        assert!(definition.render(&p).is_err());
    }
    let registry = CommandRegistry {
        commands: vec![definition.clone()],
    };
    assert!(registry.get("agent-shell").is_err());
    let mut behavioral = definition;
    behavioral.kind = CheckKind::Behavioral;
    assert!(behavioral.validate().is_err());
}

#[test]
fn native_reports_must_be_complete_and_consistent_even_on_zero_exit() {
    let rust=b"running 3 tests\ntest success ... ok\ntest rejection ... FAILED\ntest ignored_case ... ignored\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    let inventory = normalizer::parse_rust(rust).unwrap();
    assert_eq!(inventory.executed(), 2);
    assert_eq!(
        normalizer::normalize(Parser::RustTests, Some(0), rust, None, &[0]).status,
        Status::Fail
    );
    for invalid in ["", "test success ... ok", "running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"] {
        assert_eq!(normalizer::normalize(Parser::RustTests,Some(0),invalid.as_bytes(),None,&[0]).status,Status::Error);
    }
    let xml=br#"<?xml version="1.0"?><testsuite name="suite" tests="3" failures="1" skipped="1"><testcase name="ok"/><testcase name="bad"><failure message="wrong"/></testcase><testcase name="skip"><skipped/></testcase></testsuite>"#;
    let parsed = normalizer::parse_junit(xml).unwrap();
    assert_eq!(parsed.executed(), 2);
    assert_eq!(parsed.failed, vec!["bad"]);
    for malformed in ["<testsuite tests=\"0\" failures=\"0\">", "<!DOCTYPE x [<!ENTITY steal SYSTEM 'file:///etc/passwd'>]><testsuite tests=\"0\" failures=\"0\"/>", "<testsuite tests=\"1\" failures=\"0\"><testcase name=\"x\"><failure/></testcase></testsuite>", "<testsuite tests=\"2\" failures=\"0\"><testcase name=\"x\"/><testcase name=\"x\"/></testsuite>"] {
        assert!(normalizer::parse_junit(malformed.as_bytes()).is_err(),"{malformed}");
    }
    assert_eq!(
        normalizer::normalize(Parser::CtestJunit, Some(0), b"", None, &[0]).status,
        Status::Error
    );
}

#[test]
fn structured_inventory_cannot_hide_filtered_or_unobserved_cases() {
    let inventory = Inventory {
        schema_version: 1,
        discovered: vec!["a".into(), "b".into()],
        selected: vec!["a".into(), "b".into()],
        passed: vec!["a".into()],
        failed: vec![],
        skipped: vec![],
    };
    assert!(inventory.validate().is_err());
    let bytes = wire::canonical(&inventory).unwrap();
    assert_eq!(
        normalizer::normalize(Parser::InventoryJson, Some(0), b"", Some(&bytes), &[0]).status,
        Status::Error
    );
}

// Shared native fixtures deliberately use public API calls, not serialized observations.
fn spec(definition: &CommandDefinition) -> CheckSpec {
    CheckSpec {
        check_id: definition.id.clone(),
        configuration_digest: Digest::of(b"native-test"),
        checker_digest: definition.checker_digest.clone(),
        command_id: definition.id.clone(),
        validated_parameters: BTreeMap::new(),
        phase: Phase::Final,
        required: true,
        obligation_ids: vec!["isolation".into()],
        expected_case_ids: vec![],
        minimum_test_count: 0,
        success_exit_codes: vec![0],
        timeout_ms: definition.limits.timeout_ms,
        output_limit_bytes: definition.limits.output_bytes,
        dependencies: vec![],
    }
}

#[cfg(target_os = "linux")]
mod docker {
    use super::*;
    use rovik_harness::{
        identity::EvidenceKey,
        ledger::Ledger,
        runner::{self, Cancellation, DockerBackend, Environment, ExecutionInput, BACKEND_VERSION},
        snapshot::{FileEntry, Snapshot},
    };
    use std::path::Path;

    fn fixture(
        script: &str,
        timeout_ms: u64,
    ) -> (
        tempfile::TempDir,
        Ledger,
        Snapshot,
        Snapshot,
        Snapshot,
        Environment,
        CommandRegistry,
        CheckSpec,
        DockerBackend,
    ) {
        assert_eq!(
            std::env::var("ROVIK_HARNESS_DOCKER").as_deref(),
            Ok("1"),
            "explicit native-test opt-in required"
        );
        let image = Digest::parse(
            &std::env::var("ROVIK_HARNESS_IMAGE").expect("pin the Docker image config digest"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("controller")).unwrap();
        let source = ledger
            .store("source", "application/octet-stream", b"readonly-source", 0)
            .unwrap();
        let driver = ledger
            .store("source", "application/octet-stream", script.as_bytes(), 0)
            .unwrap();
        let candidate = Snapshot {
            schema_version: 1,
            base_revision: "fixture-base".into(),
            files: [(
                "readonly.txt".into(),
                FileEntry {
                    mode: 0o100644,
                    content: source,
                },
            )]
            .into(),
            deletions: vec![],
            dependency_assets: vec![],
        };
        let oracle = Snapshot {
            files: [(
                "driver.sh".into(),
                FileEntry {
                    mode: 0o100644,
                    content: driver,
                },
            )]
            .into(),
            ..candidate.clone()
        };
        let dependencies = Snapshot {
            files: BTreeMap::new(),
            ..candidate.clone()
        };
        let mut definition = command();
        definition.id = "probe".into();
        definition.executable = "/bin/sh".into();
        definition.argv_template = vec!["/oracle/driver.sh".into()];
        definition.parameters.clear();
        definition.limits.timeout_ms = timeout_ms;
        definition.limits.output_bytes = 1024 * 1024;
        definition.environment = [
            ("RUSTUP_HOME".into(), "/usr/local/rustup".into()),
            ("CARGO_HOME".into(), "/usr/local/cargo".into()),
        ]
        .into();
        let spec = spec(&definition);
        let registry = CommandRegistry {
            commands: vec![definition],
        };
        let environment = Environment {
            schema_version: 1,
            image_digest: image,
            backend_version: BACKEND_VERSION.into(),
            toolchain_digest: Digest::of(b"native-fixture-tools"),
            dependency_digest: dependencies.digest().unwrap(),
            registry_digest: Digest::of_json(&registry).unwrap(),
            locale: "C".into(),
            timezone: "UTC".into(),
        };
        (
            dir,
            ledger,
            candidate,
            oracle,
            dependencies,
            environment,
            registry,
            spec,
            DockerBackend::local(Path::new("/usr/local/bin/docker")).unwrap(),
        )
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn container_denies_source_oracle_host_credential_and_network_access() {
        let script = r#"set -eu
test "$(cat /snapshot/readonly.txt)" = readonly-source
if printf changed > /snapshot/readonly.txt 2>/dev/null; then exit 11; fi
if printf changed >> /oracle/driver.sh 2>/dev/null; then exit 12; fi
test ! -e /workspace
test ! -e /var/run/docker.sock
test ! -e /root/.ssh
test ! -e /root/.aws
test ! -e /proc/1/root/workspace
test -z "${HTTP_PROXY+x}"
test -z "${HTTPS_PROXY+x}"
if grep -q eth0 /proc/net/route; then exit 13; fi
test "$(id -u)" = 65534
printf isolated
"#;
        let (_dir, ledger, candidate, oracle, deps, environment, registry, spec, backend) =
            fixture(script, 20_000);
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(
            observed.exit_code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(observed.stderr())
        );
        assert!(!observed.incomplete());
        assert!(String::from_utf8_lossy(observed.stdout()).contains("isolated"));
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn timeout_kills_the_entire_job_including_background_children() {
        let (_dir, ledger, candidate, oracle, deps, environment, registry, spec, backend) =
            fixture("sleep 100 &\nwait\n", 200);
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert!(observed.timed_out());
        assert!(observed.incomplete());
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn cancellation_and_output_limits_never_return_complete_observations() {
        let (_dir, ledger, candidate, oracle, deps, environment, registry, spec, backend) =
            fixture("while :; do printf '01234567890123456789'; done", 5000);
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert!(observed.incomplete());
        assert!(observed.stdout().len() <= spec.output_limit_bytes as usize);
        let cancel = Cancellation::default();
        cancel.cancel();
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &cancel,
            )
            .unwrap();
        assert!(observed.incomplete());
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn writable_build_space_is_bounded_and_clean_for_every_execution() {
        let (_dir,ledger,candidate,oracle,deps,environment,registry,spec,backend)=fixture("test ! -e /build/previous || exit 9\ntouch /build/previous\ndd if=/dev/zero of=/build/full bs=1048576 count=64\n",5000);
        for _ in 0..2 {
            let observed = backend
                .execute(
                    &ledger,
                    &candidate,
                    &oracle,
                    &deps,
                    &environment,
                    &registry,
                    &spec,
                    &Cancellation::default(),
                )
                .unwrap();
            assert_ne!(observed.exit_code(), Some(0));
            assert_ne!(observed.exit_code(), Some(9));
            assert!(String::from_utf8_lossy(observed.stderr()).contains("No space left"));
        }
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn structured_reports_survive_collection_and_resource_limits_are_enforced() {
        let script = r#"printf '%s' '{"schema_version":1,"discovered":["case"],"selected":["case"],"passed":["case"],"failed":[],"skipped":[]}' > /outputs/report.json
"#;
        let (
            _dir,
            ledger,
            candidate,
            oracle,
            deps,
            mut environment,
            mut registry,
            mut spec,
            backend,
        ) = fixture(script, 5000);
        registry.commands[0].parser = Parser::InventoryJson;
        registry.commands[0].checker_digest = normalizer::checker_digest(Parser::InventoryJson);
        spec.checker_digest = registry.commands[0].checker_digest.clone();
        registry.commands[0].kind = CheckKind::Behavioral;
        registry.commands[0].report_file = Some("/outputs/report.json".into());
        environment.registry_digest = Digest::of_json(&registry).unwrap();
        spec.minimum_test_count = 1;
        spec.expected_case_ids = vec!["case".into()];
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(observed.exit_code(), Some(0));
        assert!(!observed.incomplete());
        let inventory: Inventory =
            wire::decode(observed.report().expect("report collected before teardown")).unwrap();
        inventory.validate().unwrap();
        assert_eq!(inventory.passed, vec!["case"]);

        let script = r#"awk 'BEGIN { while (1) { i++; a[i] = sprintf("%01000d", i) } }'
"#;
        let (_dir, ledger, candidate, oracle, deps, environment, registry, spec, backend) =
            fixture(script, 10_000);
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert_ne!(observed.exit_code(), Some(0));
        assert!(
            observed.incomplete() || observed.exit_code() == Some(137),
            "allocation fixture must hit its memory limit"
        );

        let (_dir, ledger, candidate, oracle, deps, environment, registry, spec, backend) = fixture(
            "i=0; while test $i -lt 100; do sleep 60 & i=$((i+1)); done; wait",
            1000,
        );
        let observed = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        assert!(observed.incomplete() || observed.exit_code() != Some(0));
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and an explicitly pinned local image"]
    fn admitted_receipts_bind_real_execution_and_fake_json_has_no_authority() {
        let (_dir, mut ledger, candidate, oracle, deps, environment, registry, spec, backend) =
            fixture("printf verified", 5000);
        let input = ledger
            .store_json(
                "contract",
                &serde_json::json!({"request":"native receipt"}),
                0,
            )
            .unwrap();
        ledger
            .create_run("run", &input.digest, &[("checks".into(), 4)].into(), 0)
            .unwrap();
        let d = Digest::of(b"fixture-binding");
        let key = EvidenceKey {
            candidate_digest: candidate.digest().unwrap(),
            contract_digest: d.clone(),
            design_digest: d.clone(),
            policy_digest: d.clone(),
            oracle_digest: oracle.digest().unwrap(),
            plan_digest: d,
            environment_digest: Digest::of_json(&environment).unwrap(),
        };
        let binding = ledger.store_json("operation", &key, 0).unwrap();
        let before = ledger.run("run").unwrap();
        ledger.bind(&before, &binding.digest, 0).unwrap();
        let now = runner::now_ms().unwrap();
        let operation = ledger
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
                "run",
                "check:1",
                "check",
                &operation.digest,
                "test-controller",
                now,
                60_000,
                &[("checks".into(), 1)].into(),
            )
            .unwrap();
        let observation = backend
            .execute(
                &ledger,
                &candidate,
                &oracle,
                &deps,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
            )
            .unwrap();
        let result = runner::admit_observation(
            &mut ledger,
            &lease,
            &key,
            &spec,
            &registry.commands[0],
            observation,
            runner::now_ms().unwrap(),
        )
        .unwrap();
        assert_eq!(result.status, Status::Pass);
        assert!(
            ledger.admitted_results("run", &binding.digest).is_err(),
            "unsettled operations cannot pass"
        );
        let result_ref = ledger
            .store_json("result", &result, runner::now_ms().unwrap())
            .unwrap();
        ledger
            .complete(
                &lease,
                &result_ref.digest,
                &[("checks".into(), 1)].into(),
                runner::now_ms().unwrap(),
            )
            .unwrap();
        assert_eq!(
            ledger.admitted_results("run", &binding.digest).unwrap(),
            vec![result.clone()]
        );
        let mut forged = result;
        forged.result_id = "worker-claim".into();
        forged.runner_receipt_id = "forged".into();
        ledger
            .store_json("result", &forged, runner::now_ms().unwrap())
            .unwrap();
        assert_eq!(
            ledger
                .admitted_results("run", &binding.digest)
                .unwrap()
                .len(),
            1
        );
    }
}
