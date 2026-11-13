mod support;

#[test]
fn managed_constructors_do_not_hide_real_runtime_resource_ffi_or_concurrency_risks() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = rovik_harness::ledger::Ledger::open(&dir.path().join("state")).unwrap();
    for (path, before, after, expected) in [
        (
            "quota.mjs",
            "",
            "const error = new RangeError('bad input');",
            None,
        ),
        (
            "quota.ts",
            "",
            "const error = new Error('bad input');",
            None,
        ),
        (
            "quota.cpp",
            "",
            "auto value = new Quota();",
            Some("resource-lifetime"),
        ),
        (
            "handles.py",
            "handle = open('data')\nhandle.close()",
            "",
            Some("resource-lifetime"),
        ),
        (
            "handles.mjs",
            "",
            "fs.openSync('data');",
            Some("resource-lifetime"),
        ),
        ("ffi.py", "", "import ctypes", Some("ffi")),
        ("ffi.mjs", "", "import ffi from 'ffi-napi';", Some("ffi")),
        (
            "workers.ts",
            "",
            "const worker = new Worker ('job');",
            Some("concurrency"),
        ),
        (
            "workers.py",
            "",
            "threading.Thread(target=work).start()",
            Some("concurrency"),
        ),
    ] {
        let base = rovik_harness::fixtures::snapshot(&ledger, "base", &[(path, before)]).unwrap();
        let candidate =
            rovik_harness::fixtures::snapshot(&ledger, "candidate", &[(path, after)]).unwrap();
        let result = rovik_harness::risk::assess_for_languages(
            &ledger,
            &base,
            &candidate,
            &[],
            &[rovik_harness::profile::Language::JavaScript],
        )
        .unwrap();
        if let Some(expected) = expected {
            assert!(result.risks.contains(expected), "{path}: {result:?}");
        } else {
            assert!(result.risks.is_empty(), "{path}: {result:?}");
        }
    }
    let base = rovik_harness::fixtures::snapshot(&ledger, "base", &[("renamed.mjs", "")]).unwrap();
    let candidate = rovik_harness::fixtures::snapshot(
        &ledger,
        "candidate",
        &[("renamed.mjs", "auto value = new Quota();")],
    )
    .unwrap();
    let native = rovik_harness::risk::assess_for_languages(
        &ledger,
        &base,
        &candidate,
        &[],
        &[rovik_harness::profile::Language::Cpp],
    )
    .unwrap();
    assert!(
        native.risks.contains("resource-lifetime"),
        "a file extension cannot override the approved native profile"
    );
}
use rovik_harness::{
    fixtures,
    identity::Digest,
    ledger::Ledger,
    risk::{self, RiskPolicy, RiskRoute},
};
#[test]
fn deleted_cleanup_and_new_ffi_cannot_be_downgraded_by_worker_tags() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let base = fixtures::snapshot(
        &ledger,
        "base",
        &[(
            "src/lib.rs",
            "unsafe extern \"C\" { fn release(); }\nfn cleanup() { free (p); }\n",
        )],
    )
    .unwrap();
    let candidate =
        fixtures::snapshot(&ledger, "base", &[("src/lib.rs", "fn cleanup() {}\n")]).unwrap();
    let assessment =
        risk::assess(&ledger, &base, &candidate, &["integer-boundaries".into()]).unwrap();
    assert!(assessment.risks.is_superset(
        &[
            "ffi".into(),
            "unsafe".into(),
            "resource-lifetime".into(),
            "integer-boundaries".into()
        ]
        .into()
    ));
    let mut setup = fixtures::quota_rust(&ledger, "risk", Digest::of(b"image"), "base").unwrap();
    assert!(RiskPolicy::default()
        .validate(&assessment, &setup.plan, &setup.registry)
        .is_err());
    let mut policy = RiskPolicy::default();
    for risk in ["ffi", "unsafe", "resource-lifetime"] {
        policy.routes.insert(
            risk.into(),
            RiskRoute {
                commands_by_configuration: Default::default(),
                concurrency_cases: Default::default(),
                commands: [setup.registry.commands[0].id.clone()].into(),
                reviews: [format!("review-{risk}")].into(),
            },
        );
        setup
            .plan
            .review_requirements
            .push(format!("review-{risk}"));
    }
    policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .unwrap();
    setup.plan.checks[0].phase = rovik_harness::plan::Phase::Fast;
    assert!(policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .is_err());
    let mut concurrent = assessment;
    concurrent.risks.insert("concurrency".into());
    assert!(policy
        .validate(&concurrent, &setup.plan, &setup.registry)
        .unwrap_err()
        .message
        .contains("concurrency"));
}

#[test]
fn concurrency_routes_require_distinct_cases_for_every_property_and_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut setup = fixtures::quota_runtime(
        &ledger,
        "concurrent",
        Digest::of(b"image"),
        "base",
        rovik_harness::profile::Language::Python,
    )
    .unwrap();
    let mut route = RiskRoute {
        commands_by_configuration: Default::default(),
        commands: [setup.registry.commands[0].id.clone()].into(),
        reviews: ["concurrency-review".into()].into(),
        concurrency_cases: Default::default(),
    };
    for property in ["schedules", "linearizability", "cancellation", "shutdown"] {
        route
            .concurrency_cases
            .insert(property.into(), [format!("test_{property}")].into());
    }
    setup
        .plan
        .review_requirements
        .push("concurrency-review".into());
    setup.plan.checks[0].expected_case_ids = route
        .concurrency_cases
        .values()
        .flatten()
        .cloned()
        .collect();
    let assessment = risk::Assessment {
        risks: ["concurrency".into()].into(),
        sources: Default::default(),
    };
    let mut policy = RiskPolicy {
        routes: [("concurrency".into(), route)].into(),
    };
    policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .unwrap();
    setup
        .plan
        .configurations
        .push(Digest::of(b"another-target"));
    assert!(policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .is_err());
    setup.plan.configurations.pop();
    policy
        .routes
        .get_mut("concurrency")
        .unwrap()
        .concurrency_cases
        .remove("shutdown");
    assert!(policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .is_err());
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned runtime image"]
fn protected_concurrency_cases_detect_lost_updates_and_observe_cancellation_and_shutdown() {
    use rovik_harness::{
        evidence::Status,
        pack_protocol::{Operation, Request},
        packs,
        profile::Language,
        registry::CommandRegistry,
        runner::{Cancellation, ContainerBackend},
        runtime::{self, TestInput},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
    for broken in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("state")).unwrap();
        let mut setup = fixtures::quota_runtime(
            &ledger,
            "concurrency",
            image.clone(),
            "base",
            Language::Python,
        )
        .unwrap();
        let source = include_str!("../fixtures/concurrency/counter.py");
        let source = if broken {
            format!(
                "import contextlib\n{}",
                source.replace("threading.Lock()", "contextlib.nullcontext()")
            )
        } else {
            source.into()
        };
        let candidate = fixtures::snapshot(
            &ledger,
            "base",
            &[("counter.py", &source), ("requirements.lock", "")],
        )
        .unwrap();
        let cases = [
            "test_schedules",
            "test_linearizability",
            "test_cancellation",
            "test_shutdown",
        ];
        let input = TestInput {
            snapshot_handle: "snapshot:active".into(),
            profile: setup.profiles[0].clone(),
            configuration_id: setup.profiles[0].configurations[0].id.clone(),
            test_files: vec!["test_counter.py".into()],
            typescript_project: None,
            required_test_ids: cases.iter().map(|s| s.to_string()).collect(),
        };
        let request = Request {
            protocol_version: 1,
            request_id: "concurrency".into(),
            operation: Operation::Test,
            payload: serde_json::to_value(&input).unwrap(),
        };
        let text = format!("{}\n", serde_json::to_string(&request).unwrap());
        let oracle = fixtures::snapshot(
            &ledger,
            "oracle",
            &[
                ("requests/concurrency.json", &text),
                (
                    "test_counter.py",
                    include_str!("../fixtures/concurrency/test_counter.py"),
                ),
            ],
        )
        .unwrap();
        let command = packs::runtime_test_command(["concurrency".into()].into()).unwrap();
        let registry = CommandRegistry {
            commands: vec![command.clone()],
        };
        setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
        let mut spec = setup.plan.checks[0].clone();
        spec.command_id = command.id;
        spec.checker_digest = command.checker_digest;
        spec.expected_case_ids = cases.iter().map(|s| s.to_string()).collect();
        spec.minimum_test_count = 4;
        spec.validated_parameters = [("request_id".into(), "concurrency".into())].into();
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
        let result = runtime::verify(
            &request,
            observation.stdout(),
            &spec.configuration_digest,
            observation.exit_code(),
        )
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(observation.stderr())));
        assert_eq!(
            result.status,
            if broken { Status::Fail } else { Status::Pass }
        );
        assert_eq!(result.inventory.executed(), 4);
        if broken {
            assert!(result
                .inventory
                .failed
                .contains(&"test_linearizability".into()));
        }
    }
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned native image"]
fn actual_sanitizers_and_ffi_consumers_reject_resource_and_abi_mutants() {
    use rovik_harness::{
        normalizer, packs,
        plan::CheckKind,
        registry::{CommandRegistry, Parser},
        runner::{Cancellation, DockerBackend},
    };
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("state")).unwrap();
    let mut setup = fixtures::quota_rust(
        &ledger,
        "interop",
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        "base",
    )
    .unwrap();
    let oracle = fixtures::snapshot(
        &ledger,
        "protected-interop",
        &[
            ("packet.h", include_str!("../fixtures/interop/packet.h")),
            ("packet.c", include_str!("../fixtures/interop/packet.c")),
            (
                "allocator.c",
                include_str!("../fixtures/interop/allocator.c"),
            ),
            ("consumer.c", include_str!("../fixtures/interop/consumer.c")),
            (
                "consumer.rs",
                include_str!("../fixtures/interop/consumer.rs"),
            ),
        ],
    )
    .unwrap();
    let mut command = packs::graph_command(["interop".into()].into()).unwrap();
    command.executable = rovik_harness::tools::EXECUTABLE.into();
    command.argv_template = vec!["interop".into()];
    command.parameters.clear();
    command.parser = Parser::InventoryJson;
    command.checker_digest = normalizer::checker_digest(command.parser);
    command.kind = CheckKind::Behavioral;
    command.report_file = Some("/outputs/cases.json".into());
    command.limits.timeout_ms = 180_000;
    command.limits.output_bytes = 8 * 1024 * 1024;
    let mut spec = setup.plan.checks.remove(0);
    spec.command_id = command.id.clone();
    spec.checker_digest = command.checker_digest.clone();
    spec.timeout_ms = command.limits.timeout_ms;
    spec.output_limit_bytes = command.limits.output_bytes;
    let registry = CommandRegistry {
        commands: vec![command],
    };
    setup.environment.registry_digest = Digest::of_json(&registry).unwrap();
    let observation = DockerBackend::local(&crate::support::docker_path())
        .unwrap()
        .execute(
            &ledger,
            &setup.base,
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
        String::from_utf8_lossy(observation.stderr()),
        String::from_utf8_lossy(observation.report().unwrap_or(b"missing report"))
    );
    assert!(!observation.incomplete());
    let report: normalizer::Inventory =
        rovik_harness::wire::decode(observation.report().unwrap()).unwrap();
    report.validate().unwrap();
    assert_eq!(report.executed(), 20);
    assert_eq!(report.passed.len(), 20);
    assert!(report.failed.is_empty());
}

#[test]
fn legacy_risk_routes_keep_their_canonical_identity_and_global_command_requirement() {
    let original = serde_json::json!({"commands":["native-tests"],"reviews":["design-review"],"concurrency_cases":{}});
    let route: RiskRoute = serde_json::from_value(original.clone()).unwrap();
    assert!(route.commands_by_configuration.is_empty());
    assert_eq!(
        Digest::of_json(&original).unwrap(),
        Digest::of_json(&route).unwrap()
    );
}

#[test]
fn mixed_profile_risk_routes_require_the_correct_native_command_and_review_in_every_configuration()
{
    use std::collections::{BTreeMap, BTreeSet};
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("state")).unwrap();
    let mut setup =
        fixtures::quota_rust(&ledger, "mixed-risk", Digest::of(b"image"), "base").unwrap();
    let runtime = fixtures::quota_runtime(
        &ledger,
        "runtime-risk",
        Digest::of(b"image"),
        "base",
        rovik_harness::profile::Language::TypeScript,
    )
    .unwrap();
    let native_configuration = setup.plan.configurations[0].clone();
    let runtime_configuration = runtime.plan.configurations[0].clone();
    let native_command = setup.registry.commands[0].id.clone();
    let runtime_command = runtime.registry.commands[0].id.clone();
    setup
        .registry
        .commands
        .extend(runtime.registry.commands.clone());
    setup
        .plan
        .configurations
        .push(runtime_configuration.clone());
    setup.plan.checks.extend(runtime.plan.checks.clone());
    setup.plan.review_requirements.push("design-review".into());
    let mut policy = RiskPolicy {
        routes: BTreeMap::from([(
            "public-api".into(),
            RiskRoute {
                commands: BTreeSet::from([native_command.clone(), runtime_command.clone()]),
                commands_by_configuration: BTreeMap::from([
                    (
                        native_configuration.clone(),
                        BTreeSet::from([native_command.clone()]),
                    ),
                    (
                        runtime_configuration.clone(),
                        BTreeSet::from([runtime_command.clone()]),
                    ),
                ]),
                reviews: BTreeSet::from(["design-review".into()]),
                concurrency_cases: BTreeMap::new(),
            },
        )]),
    };
    let assessment = risk::Assessment {
        risks: BTreeSet::from(["public-api".into()]),
        sources: BTreeMap::new(),
    };
    policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .unwrap();
    for mutation in [
        "missing",
        "unknown",
        "empty",
        "union",
        "wrong-check",
        "legacy",
    ] {
        let mut invalid = policy.clone();
        let route = invalid.routes.get_mut("public-api").unwrap();
        match mutation {
            "missing" => {
                route
                    .commands_by_configuration
                    .remove(&runtime_configuration);
            }
            "unknown" => {
                route.commands_by_configuration.insert(
                    Digest::of(b"unknown"),
                    BTreeSet::from([runtime_command.clone()]),
                );
            }
            "empty" => {
                route
                    .commands_by_configuration
                    .insert(runtime_configuration.clone(), BTreeSet::new());
            }
            "union" => {
                route.commands.insert("unregistered".into());
            }
            "wrong-check" => {
                route.commands_by_configuration.insert(
                    native_configuration.clone(),
                    BTreeSet::from([runtime_command.clone()]),
                );
                route.commands_by_configuration.insert(
                    runtime_configuration.clone(),
                    BTreeSet::from([native_command.clone()]),
                );
            }
            "legacy" => {
                route.commands_by_configuration.clear();
            }
            _ => unreachable!(),
        }
        assert!(
            invalid
                .validate(&assessment, &setup.plan, &setup.registry)
                .is_err(),
            "{mutation}"
        );
    }
    setup
        .plan
        .review_requirements
        .retain(|review| review != "design-review");
    assert!(policy
        .validate(&assessment, &setup.plan, &setup.registry)
        .unwrap_err()
        .message
        .contains("review"));
    setup.plan.review_requirements.push("design-review".into());
    let mut additional_risk = assessment.clone();
    additional_risk.risks.insert("unsafe".into());
    assert!(policy
        .validate(&additional_risk, &setup.plan, &setup.registry)
        .unwrap_err()
        .message
        .contains("unrouted risk"));
    policy
        .routes
        .get_mut("public-api")
        .unwrap()
        .commands_by_configuration
        .remove(&native_configuration);
    let no_risks = risk::Assessment {
        risks: BTreeSet::new(),
        sources: BTreeMap::new(),
    };
    assert!(
        policy
            .validate(&no_risks, &setup.plan, &setup.registry)
            .is_err(),
        "an incomplete explicit matrix cannot await a risk to become invalid"
    );
}
