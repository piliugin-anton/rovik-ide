mod support;
use rovik_harness::{
    fixtures,
    identity::Digest,
    ledger::Ledger,
    pack_protocol::{
        self, Edge, ForbiddenEdge, Graph, NativeInput, Node, Operation, Precision, Request,
    },
    profile::Language,
};
fn input(ledger: &Ledger) -> NativeInput {
    let setup = fixtures::quota_rust(ledger, "pack-fixture", Digest::of(b"image"), "base").unwrap();
    NativeInput {
        snapshot_handle: "snapshot:active".into(),
        configuration_id: setup.profiles[0].configurations[0].id.clone(),
        profile: setup.profiles[0].clone(),
        forbidden_edges: vec![ForbiddenEdge {
            source: "domain".into(),
            target: "io".into(),
        }],
        required_test_ids: Default::default(),
    }
}
fn graph(input: &NativeInput) -> Graph {
    Graph {
        precision: Precision::CargoResolved,
        nodes: ["domain", "helper", "io"]
            .iter()
            .map(|id| Node {
                id: (*id).into(),
                path: None,
                kind: "package".into(),
            })
            .collect(),
        edges: vec![],
        unresolved: vec![],
        configuration_digest: input.profile.configurations[0].digest().unwrap(),
        compiler_commands: vec![],
        test_inventory: vec![],
    }
}
#[test]
fn configured_boundaries_reject_transitive_edges_unknown_precision_and_disappearing_sources() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let input = input(&ledger);
    let mut graph = graph(&input);
    graph.validate(&input).unwrap();
    graph.edges = vec![
        Edge {
            source: "domain".into(),
            target: "helper".into(),
            kind: "normal".into(),
        },
        Edge {
            source: "helper".into(),
            target: "io".into(),
            kind: "normal".into(),
        },
    ];
    assert!(graph.validate(&input).unwrap_err().0.contains("forbidden"));
    graph.edges.clear();
    graph.precision = Precision::Lexical;
    assert!(graph.validate(&input).is_err());
    graph.precision = Precision::CargoResolved;
    graph.nodes.remove(0);
    assert!(graph.validate(&input).is_err());
}
#[test]
fn protocol_checks_correlation_closed_schemas_versions_and_required_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut input = input(&ledger);
    let graph = graph(&input);
    let request = Request {
        protocol_version: 1,
        request_id: "index-1".into(),
        operation: Operation::Index,
        payload: serde_json::to_value(&input).unwrap(),
    };
    let reply = serde_json::json!({"protocol_version":1,"request_id":"index-1","ok":true,"result":{"language":"rust","graph":graph}});
    let bytes = format!("{reply}\n");
    pack_protocol::verify_index(&request, bytes.as_bytes(), &graph.configuration_digest).unwrap();
    for bad in [
        bytes.replace("index-1", "other"),
        bytes.replace("\"protocol_version\":1", "\"protocol_version\":2"),
        bytes.trim().to_string(),
        format!("{bytes}{bytes}"),
        bytes.replace("\"ok\":true", "\"ok\":true,\"unknown\":true"),
    ] {
        assert!(
            pack_protocol::verify_index(&request, bad.as_bytes(), &graph.configuration_digest)
                .is_err()
        );
    }
    input.required_test_ids.insert("required-doctest".into());
    assert!(graph.validate(&input).unwrap_err().0.contains("discovery"));
}
#[test]
fn supported_reference_profiles_are_exact_and_consumers_cannot_be_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    for language in [Language::Rust, Language::C, Language::Cpp] {
        let mut setup = if language == Language::Rust {
            fixtures::quota_rust(&ledger, "quota", Digest::of(b"image"), "base").unwrap()
        } else {
            fixtures::quota_c_family(&ledger, "quota", Digest::of(b"image"), "base", language)
                .unwrap()
        };
        setup.validate(&ledger).unwrap();
        let mut profile = setup.profiles[0].clone();
        profile.configurations[0].flags.clear();
        assert!(if language == Language::Rust {
            rovik_harness::packs::rust_reference_command(&profile)
        } else {
            rovik_harness::packs::c_family_reference_command(&profile)
        }
        .is_err());
        setup.profiles[0]
            .public_consumer_commands
            .push("omitted-consumer".into());
        assert!(setup.validate(&ledger).is_err());
    }
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn configured_cargo_features_and_cmake_compile_graphs_observe_real_dependency_edges() {
    use rovik_harness::{
        packs,
        plan::{CheckSpec, Phase},
        registry::CommandRegistry,
        runner::{Cancellation, DockerBackend},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    for language in [Language::Rust, Language::C, Language::Cpp] {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&dir.path().join("state")).unwrap();
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let mut setup = if language == Language::Rust {
            fixtures::quota_rust(&ledger, "graph", image, "base").unwrap()
        } else {
            fixtures::quota_c_family(&ledger, "graph", image, "base", language).unwrap()
        };
        let mut input = NativeInput {
            snapshot_handle: "snapshot:active".into(),
            configuration_id: setup.profiles[0].configurations[0].id.clone(),
            profile: setup.profiles[0].clone(),
            forbidden_edges: vec![],
            required_test_ids: Default::default(),
        };
        let mut files = vec![];
        let cmake=format!("cmake_minimum_required(VERSION 3.25)\nproject(graph LANGUAGES {})\noption(FORBIDDEN \"edge\" OFF)\nadd_executable(consumer src/main.{})\nif(FORBIDDEN)\n target_compile_definitions(consumer PRIVATE FORBIDDEN)\nendif()\nenable_testing()\nadd_test(NAME consumer-smoke COMMAND consumer)\n",if language==Language::C {"C"}else{"CXX"},if language==Language::C {"c"}else{"cpp"});
        if language == Language::Rust {
            input.profile.configurations[0].flags.clear();
            input.profile.configurations[0].build_targets = vec!["domain".into()];
            files.extend([("Cargo.toml","[workspace]\nmembers=[\"domain\",\"io\"]\nresolver=\"2\"\n"),("Cargo.lock","version = 3\n[[package]]\nname = \"domain\"\nversion = \"0.1.0\"\ndependencies = [\"io\"]\n[[package]]\nname = \"io\"\nversion = \"0.1.0\"\n"),("domain/Cargo.toml","[package]\nname=\"domain\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nio={path=\"../io\", optional=true}\n[features]\nexternal=[\"dep:io\"]\n"),("domain/src/lib.rs","pub fn value()->u32 { 1 }\n"),("io/Cargo.toml","[package]\nname=\"io\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"),("io/src/lib.rs","pub fn value()->u32 { 2 }\n")]);
            input.forbidden_edges.push(ForbiddenEdge {
                source: "domain@0.1.0".into(),
                target: "io@0.1.0".into(),
            });
        } else {
            input.profile.configurations[0].build_targets = vec!["consumer".into()];
            files.extend([("CMakeLists.txt",cmake.as_str()),(if language==Language::C {"src/main.c"}else{"src/main.cpp"},"#ifdef FORBIDDEN\n#include \"forbidden.h\"\n#endif\nint main(void) { return 0; }\n"),("src/forbidden.h","#define FORBIDDEN_EDGE 1\n")]);
            input.required_test_ids.insert("consumer-smoke".into());
            input.forbidden_edges.push(ForbiddenEdge {
                source: if language == Language::C {
                    "src/main.c"
                } else {
                    "src/main.cpp"
                }
                .into(),
                target: "src/forbidden.h".into(),
            });
        }
        let candidate = fixtures::snapshot(&ledger, "base", &files).unwrap();
        for enabled in [false, true] {
            input.profile.configurations[0].features = if enabled {
                vec![if language == Language::Rust {
                    "domain/external"
                } else {
                    "FORBIDDEN=ON"
                }
                .into()]
            } else {
                vec![]
            };
            let request = Request {
                protocol_version: 1,
                request_id: "configured".into(),
                operation: Operation::Index,
                payload: serde_json::to_value(&input).unwrap(),
            };
            let request_text = format!("{}\n", serde_json::to_string(&request).unwrap());
            let oracle = fixtures::snapshot(
                &ledger,
                "oracle",
                &[("requests/configured.json", &request_text)],
            )
            .unwrap();
            let command = packs::graph_command(["configured".into()].into()).unwrap();
            let registry = CommandRegistry {
                commands: vec![command.clone()],
            };
            setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
            let config = input.profile.configurations[0].digest().unwrap();
            let spec = CheckSpec {
                check_id: "graph".into(),
                configuration_digest: config.clone(),
                checker_digest: command.checker_digest,
                command_id: command.id,
                validated_parameters: [("request_id".into(), "configured".into())].into(),
                phase: Phase::Final,
                required: true,
                obligation_ids: vec![],
                expected_case_ids: vec![],
                minimum_test_count: 0,
                success_exit_codes: vec![0],
                timeout_ms: 180_000,
                output_limit_bytes: 4 * 1024 * 1024,
                dependencies: vec![],
            };
            let observation = DockerBackend::local(&crate::support::docker_path())
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
            assert_eq!(
                observation.exit_code(),
                Some(0),
                "{}\n{}",
                String::from_utf8_lossy(observation.stdout()),
                String::from_utf8_lossy(observation.stderr())
            );
            assert!(!observation.incomplete());
            let checked = pack_protocol::verify_index(&request, observation.stdout(), &config);
            if enabled {
                assert!(checked.unwrap_err().0.contains("forbidden"));
            } else {
                checked.unwrap();
            }
            // Exercise the same protected-request admission path used by the controller.
            use rovik_harness::{
                evidence::Status,
                identity::EvidenceKey,
                runner::{self, ExecutionInput},
            };
            let now = runner::now_ms().unwrap();
            let run_id = if enabled {
                "configured-on"
            } else {
                "configured-off"
            };
            let input_artifact = ledger.store_json("operation", &request, now).unwrap();
            let run = ledger
                .create_run(
                    run_id,
                    &input_artifact.digest,
                    &[("checks".into(), 2)].into(),
                    now,
                )
                .unwrap();
            let key = EvidenceKey {
                candidate_digest: ledger
                    .store_json("snapshot", &candidate, now)
                    .unwrap()
                    .digest,
                contract_digest: ledger
                    .store_json("contract", &setup.contract, now)
                    .unwrap()
                    .digest,
                design_digest: ledger
                    .store_json("design", &setup.design, now)
                    .unwrap()
                    .digest,
                policy_digest: ledger
                    .store_json("policy", &setup.policy, now)
                    .unwrap()
                    .digest,
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
                    run_id,
                    "graph-check",
                    "check",
                    &input.digest,
                    "fixture",
                    now,
                    60_000,
                    &[("checks".into(), 1)].into(),
                )
                .unwrap();
            let result = runner::admit_observation(
                &mut ledger,
                &lease,
                &key,
                &spec,
                &registry.commands[0],
                observation,
                now,
            )
            .unwrap();
            assert_eq!(
                result.status,
                if enabled { Status::Error } else { Status::Pass }
            );
            let artifact = ledger.store_json("result", &result, now).unwrap();
            ledger
                .complete(
                    &lease,
                    &artifact.digest,
                    &[("checks".into(), 1)].into(),
                    now,
                )
                .unwrap();
            assert_eq!(
                ledger
                    .admitted_results(run_id, &binding.digest)
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}

#[test]
fn effective_native_commands_must_retain_the_required_sanitizer_mode() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut setup = fixtures::quota_c_family(
        &ledger,
        "instrument",
        Digest::of(b"image"),
        "base",
        Language::C,
    )
    .unwrap();
    setup.profiles[0].configurations[0].flags = vec![
        "-fsanitize=address,undefined".into(),
        "-fno-sanitize-recover=all".into(),
    ];
    let input = NativeInput {
        snapshot_handle: "snapshot:active".into(),
        configuration_id: setup.profiles[0].configurations[0].id.clone(),
        profile: setup.profiles[0].clone(),
        forbidden_edges: vec![],
        required_test_ids: Default::default(),
    };
    let mut graph = Graph {
        precision: Precision::ConfiguredBuild,
        nodes: vec![Node {
            id: "main.c".into(),
            path: Some("main.c".into()),
            kind: "translation_unit".into(),
        }],
        edges: vec![],
        unresolved: vec![],
        configuration_digest: input.profile.configurations[0].digest().unwrap(),
        compiler_commands: vec![rovik_harness::pack_protocol::CompilerCommand {
            file: "main.c".into(),
            argv: vec![
                "/usr/bin/gcc".into(),
                "-std=c17".into(),
                "-fsanitize=address,undefined".into(),
                "-fno-sanitize-recover=all".into(),
            ],
        }],
        test_inventory: vec![],
    };
    graph.validate(&input).unwrap();
    graph.compiler_commands[0]
        .argv
        .push("-fno-sanitize=all".into());
    assert!(graph.validate(&input).is_err());
    graph.compiler_commands[0].argv.pop();
    graph.compiler_commands[0].argv.remove(2);
    assert!(graph.validate(&input).is_err());
}
