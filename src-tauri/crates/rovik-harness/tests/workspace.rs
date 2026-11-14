mod support;
use rovik_harness::{
    fixtures,
    identity::Digest,
    ledger::Ledger,
    native_checks::{self, NativeCheck, NativeCheckInput, NativeCheckOutput},
    normalizer::Inventory,
    pack_protocol::{NativeInput, Operation, Request},
    profile::Language,
};

fn request(ledger: &Ledger, check: NativeCheck) -> Request {
    let setup = fixtures::quota_rust(ledger, "workspace", Digest::of(b"image"), "base").unwrap();
    Request {
        protocol_version: 1,
        request_id: "workspace".into(),
        operation: Operation::NativeCheck,
        payload: serde_json::to_value(NativeCheckInput {
            oracle_test_files: vec![],
            check,
            native: NativeInput {
                snapshot_handle: "snapshot:active".into(),
                configuration_id: setup.profiles[0].configurations[0].id.clone(),
                profile: setup.profiles[0].clone(),
                forbidden_edges: vec![],
                architecture: None,
                required_test_ids: Default::default(),
                typescript_project: None,
            },
        })
        .unwrap(),
    }
}
#[test]
fn native_workspace_reports_cannot_change_operation_or_hide_discovered_and_skipped_cases() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let request = request(&ledger, NativeCheck::CargoTests);
    let input = native_checks::input(&request).unwrap();
    let config = input.native.profile.configurations[0].digest().unwrap();
    let mut output = NativeCheckOutput {
        check: NativeCheck::CargoTests,
        configuration_digest: config.clone(),
        compiler_version: "rustc 1.85.1".into(),
        exit_code: 0,
        inventory: Inventory {
            schema_version: 1,
            discovered: vec!["one::case".into()],
            selected: vec!["one::case".into()],
            passed: vec!["one::case".into()],
            failed: vec![],
            skipped: vec![],
        },
    };
    let encode = |output: &NativeCheckOutput| {
        format!("{}\n",serde_json::json!({"protocol_version":1,"request_id":"workspace","ok":true,"result":output})).into_bytes()
    };
    assert_eq!(
        native_checks::verify(&request, &encode(&output), &config, Some(0))
            .unwrap()
            .status,
        rovik_harness::evidence::Status::Pass
    );
    output.inventory.discovered.push("hidden".into());
    assert!(native_checks::verify(&request, &encode(&output), &config, Some(0)).is_err());
    output.inventory.discovered.pop();
    output.inventory.skipped = std::mem::take(&mut output.inventory.passed);
    assert_eq!(
        native_checks::verify(&request, &encode(&output), &config, Some(0))
            .unwrap()
            .status,
        rovik_harness::evidence::Status::Fail
    );
    output.check = NativeCheck::CargoCheck;
    assert!(native_checks::verify(&request, &encode(&output), &config, Some(0)).is_err());
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native workspace image"]
fn real_workspace_checks_cover_every_rust_package_and_configured_c_and_cpp_tests() {
    use rovik_harness::{
        packs,
        plan::{CheckSpec, Phase},
        registry::CommandRegistry,
        runner::{Cancellation, ContainerBackend},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
    for language in [Language::Rust, Language::C, Language::Cpp] {
        let checks = if language == Language::Rust {
            vec![
                NativeCheck::CargoFmt,
                NativeCheck::CargoCheck,
                NativeCheck::CargoClippy,
                NativeCheck::CargoTests,
                NativeCheck::CargoDocTests,
            ]
        } else {
            vec![NativeCheck::ClangTidy, NativeCheck::Ctest]
        };
        for check in checks {
            for mutant in [false, true] {
                if mutant && check != NativeCheck::CargoTests {
                    continue;
                }
                eprintln!("workspace {language:?} {check:?}, mutant={mutant}");
                let dir = tempfile::tempdir().unwrap();
                let ledger = Ledger::open(&dir.path().join("state")).unwrap();
                let mut setup = if language == Language::Rust {
                    fixtures::quota_rust(&ledger, "workspace", image.clone(), "base").unwrap()
                } else {
                    fixtures::quota_c_family(&ledger, "workspace", image.clone(), "base", language)
                        .unwrap()
                };
                let mut profile = setup.profiles[0].clone();
                let mut files = Vec::new();
                let first=format!("/// Returns two.\n/// ```\n/// assert_eq!(one::value(), 2);\n/// ```\npub fn value() -> u32 {{\n    {}\n}}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn returns_two() {{\n        assert_eq!(super::value(), 2);\n    }}\n}}\n",if mutant {3}else{2});
                let cmake=format!("cmake_minimum_required(VERSION 3.25)\nproject(workspace LANGUAGES {})\nadd_executable(smoke main.{})\nenable_testing()\nadd_test(NAME smoke COMMAND smoke)\n",if language==Language::C {"C"}else{"CXX"},if language==Language::C {"c"}else{"cpp"});
                if language == Language::Rust {
                    profile.configurations[0].flags.clear();
                    profile.configurations[0].build_targets = vec!["one".into(), "two".into()];
                    files.extend([
                        ("Cargo.toml","[workspace]\nmembers=[\"one\",\"two\"]\nresolver=\"2\"\n"),
                        ("Cargo.lock","version = 3\n[[package]]\nname = \"one\"\nversion = \"0.1.0\"\n[[package]]\nname = \"two\"\nversion = \"0.1.0\"\ndependencies = [\"one\"]\n"),
                        ("one/Cargo.toml","[package]\nname=\"one\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),
                        ("one/src/lib.rs",first.as_str()),
                        ("two/Cargo.toml","[package]\nname=\"two\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\none={path=\"../one\"}\n"),
                        ("two/src/lib.rs","/// Uses the dependency.\n/// ```\n/// assert_eq!(two::value(), 2);\n/// ```\npub fn value() -> u32 {\n    one::value()\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn returns_two() {\n        assert_eq!(super::value(), 2);\n    }\n}\n")]);
                } else {
                    profile.configurations[0].build_targets = vec!["smoke".into()];
                    files.extend([
                        ("CMakeLists.txt", cmake.as_str()),
                        (
                            if language == Language::C {
                                "main.c"
                            } else {
                                "main.cpp"
                            },
                            "int main(void) { return 0; }\n",
                        ),
                    ]);
                }
                let candidate = fixtures::snapshot(&ledger, "base", &files).unwrap();
                let native = NativeInput {
                    snapshot_handle: "snapshot:active".into(),
                    configuration_id: profile.configurations[0].id.clone(),
                    profile: profile.clone(),
                    forbidden_edges: vec![],
                    architecture: None,
                    required_test_ids: Default::default(),
                    typescript_project: None,
                };
                let request = Request {
                    protocol_version: 1,
                    request_id: "workspace".into(),
                    operation: Operation::NativeCheck,
                    payload: serde_json::to_value(NativeCheckInput {
                        native,
                        check,
                        oracle_test_files: vec![],
                    })
                    .unwrap(),
                };
                let request_text = format!("{}\n", serde_json::to_string(&request).unwrap());
                let oracle = fixtures::snapshot(
                    &ledger,
                    "oracle",
                    &[("requests/workspace.json", &request_text)],
                )
                .unwrap();
                let command = packs::workspace_command(check, ["workspace".into()].into()).unwrap();
                let registry = CommandRegistry {
                    commands: vec![command.clone()],
                };
                setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
                let config = profile.configurations[0].digest().unwrap();
                let spec = CheckSpec {
                    check_id: "workspace".into(),
                    configuration_digest: config.clone(),
                    checker_digest: command.checker_digest,
                    command_id: command.id,
                    validated_parameters: [("request_id".into(), "workspace".into())].into(),
                    phase: Phase::Final,
                    required: true,
                    obligation_ids: vec![],
                    expected_case_ids: vec![],
                    minimum_test_count: u64::from(check.behavioral()),
                    success_exit_codes: vec![0],
                    timeout_ms: command.limits.timeout_ms,
                    output_limit_bytes: command.limits.output_bytes,
                    dependencies: vec![],
                };
                let observation = ContainerBackend::local(&support::docker_path())
                    .unwrap()
                    .execute(
                        &ledger,
                        &candidate,
                        &oracle,
                        &setup.dependencies,
                        &setup.environment,
                        &registry,
                        &spec,
                        &Cancellation::default(),
                    )
                    .unwrap();
                let result = native_checks::verify(
                    &request,
                    observation.stdout(),
                    &config,
                    observation.exit_code(),
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}\n{}\n{}",
                        String::from_utf8_lossy(observation.stdout()),
                        String::from_utf8_lossy(observation.stderr())
                    )
                });
                assert_eq!(
                    result.status,
                    if mutant {
                        rovik_harness::evidence::Status::Fail
                    } else {
                        rovik_harness::evidence::Status::Pass
                    },
                    "{:?}",
                    result
                );
                if check.behavioral() {
                    assert_eq!(
                        result.inventory.executed(),
                        if language == Language::Rust { 2 } else { 1 }
                    );
                }
            }
        }
    }
}
