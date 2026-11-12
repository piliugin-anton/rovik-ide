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
        .0
        .contains("concurrency"));
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
            (
                "run-interop.py",
                include_str!("../fixtures/interop/run-interop.py"),
            ),
        ],
    )
    .unwrap();
    let mut command = packs::graph_command(["interop".into()].into()).unwrap();
    command.executable = "/usr/bin/python3".into();
    command.argv_template = vec!["/oracle/run-interop.py".into()];
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
    let observation = DockerBackend::local(std::path::Path::new("/usr/local/bin/docker"))
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
