use rovik_harness::{
    contract::{ContractRegistry, TaskContract},
    identity::Digest,
    plan::{CheckKind, CheckPlan, CheckSpec, Phase, PlanRegistry},
    policy::{EffectivePolicy, PolicyLayer},
    profile::{Configuration, Language, Profile, Resolution},
    scope::Scope,
    wire,
};
use std::collections::{BTreeMap, BTreeSet};

fn contract() -> TaskContract {
    wire::decode(include_bytes!("../examples/quota-contract.v1.json")).unwrap()
}
fn registry(contract: &TaskContract) -> ContractRegistry {
    ContractRegistry {
        profiles: contract.profile_ids.iter().cloned().collect(),
        configurations: Default::default(),
        suites: [(
            contract.acceptance_suite_ids[0].clone(),
            contract
                .invariants
                .iter()
                .flat_map(|i| i.case_ids.clone())
                .collect(),
        )]
        .into(),
        constraints: contract.constraints.keys().cloned().collect(),
        risks: contract.risk_tags.iter().cloned().collect(),
    }
}

#[test]
fn the_version_one_contract_is_readable_but_unknown_authority_is_rejected() {
    let contract = contract();
    let registry = registry(&contract);
    contract.validate(&registry).unwrap();
    let schema: serde_json::Value =
        wire::decode(include_bytes!("../schemas/task-contract.v1.schema.json")).unwrap();
    assert_eq!(contract.schema_version, 1);
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        contract.schema_version
    );
    for version in [0, 2, 3, 99] {
        let mut unsupported = contract.clone();
        unsupported.schema_version = version;
        assert!(unsupported.validate(&registry).is_err());
    }
    for field in [
        "schema_version",
        "profile_ids",
        "acceptance_suite_ids",
        "constraints",
        "risk_tags",
        "invariants",
    ] {
        let mut value = serde_json::to_value(&contract).unwrap();
        match field {
            "schema_version" => value[field] = 99.into(),
            "profile_ids" | "acceptance_suite_ids" | "risk_tags" => {
                value[field] = serde_json::json!(["unknown"])
            }
            "constraints" => value[field] = serde_json::json!({"unknown":true}),
            "invariants" => value[field][0]["case_ids"] = serde_json::json!(["unknown"]),
            _ => unreachable!(),
        }
        let modified: TaskContract = wire::decode(&wire::canonical(&value).unwrap()).unwrap();
        assert!(modified.validate(&registry).is_err(), "{field}");
    }
    let mut value = serde_json::to_value(&contract).unwrap();
    value["approved"] = true.into();
    assert!(wire::decode::<TaskContract>(&serde_json::to_vec(&value).unwrap()).is_err());
    value.as_object_mut().unwrap().remove("approved");
    value["scope"]["allow_all"] = true.into();
    assert!(wire::decode::<TaskContract>(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn duplicate_invariants_and_blocking_questions_do_not_become_executable() {
    let mut c = contract();
    let registry = registry(&c);
    c.invariants.push(c.invariants[0].clone());
    assert!(c.validate(&registry).is_err());
    c.invariants.pop();
    c.unknowns.push(rovik_harness::contract::Unknown {
        id: "Q".into(),
        question: "Who serializes?".into(),
        blocking: true,
        owner: "domain owner".into(),
    });
    c.validate(&registry).unwrap();
    assert!(c.require_resolved().is_err());
}

#[test]
fn narrower_policy_cannot_drop_rules_or_expand_tools_paths_or_budgets() {
    let layer = PolicyLayer {
        allow_commands: ["build".into(), "test".into()].into(),
        deny_commands: BTreeSet::new(),
        obligations: ["cases".into()].into(),
        limits: [("model_calls".into(), 4)].into(),
        facts: BTreeMap::new(),
        scope: Scope {
            allow_write: vec!["src/**".into()],
            protected: vec!["src/oracle/**".into()],
        },
        deny_read: vec!["src/private/**".into()],
    };
    let mut child = layer.clone();
    child.obligations.clear();
    child.limits.insert("model_calls".into(), 100);
    child.scope.allow_write = vec!["**".into()];
    child.scope.protected.clear();
    child.deny_read.clear();
    child.deny_commands.insert("build".into());
    let resolved = EffectivePolicy::resolve(
        &[layer.clone(), child.clone()],
        &layer.allow_commands,
        &layer.obligations,
    )
    .unwrap();
    assert_eq!(resolved.commands, ["test".into()].into());
    assert_eq!(resolved.obligations, layer.obligations);
    assert_eq!(resolved.limits["model_calls"], 4);
    assert!(resolved.check_write("src/oracle/test.rs").is_err());
    assert!(resolved.check_write("other.rs").is_err());
    assert!(resolved.check_read("src/private/token.txt").is_err());
    assert!(resolved.check_read("src/private-other/token.txt").is_ok());
    let mut parent = layer.clone();
    parent.facts.insert("allocation".into(), "none".into());
    child.facts.insert("allocation".into(), "required".into());
    assert!(
        EffectivePolicy::resolve(&[parent, child], &layer.allow_commands, &layer.obligations)
            .is_err()
    );
}

#[test]
fn read_exclusions_preserve_legacy_policy_identity_and_reject_invalid_patterns() {
    let legacy = serde_json::json!({
        "commands": [], "obligations": [], "limits": {}, "facts": {},
        "scopes": [{"allow_write": ["src/**"], "protected": []}]
    });
    let mut policy: EffectivePolicy = wire::decode(&wire::canonical(&legacy).unwrap()).unwrap();
    assert!(policy.deny_read.is_empty());
    assert_eq!(
        Digest::of_json(&policy).unwrap(),
        Digest::of_json(&legacy).unwrap()
    );
    policy.deny_read = vec!["src/private/**".into()];
    assert_ne!(
        Digest::of_json(&policy).unwrap(),
        Digest::of_json(&legacy).unwrap()
    );
    policy.validate_reads().unwrap();
    for patterns in [
        vec!["../secrets"],
        vec!["/secrets"],
        vec!["src\\secrets"],
        vec!["src/*"],
        vec!["src/private/**", "src/private/**"],
    ] {
        policy.deny_read = patterns.into_iter().map(str::to_owned).collect();
        assert!(policy.validate_reads().is_err());
        assert!(policy.check_read("src/public.rs").is_err());
    }
    policy.deny_read = vec!["**".into()];
    assert!(policy.check_read("src/public.rs").is_err());
}

fn plan() -> (CheckPlan, PlanRegistry) {
    let d = Digest::of(b"fixture");
    let c = CheckSpec {
        check_id: "cases".into(),
        configuration_digest: d.clone(),
        checker_digest: d.clone(),
        command_id: "test".into(),
        validated_parameters: BTreeMap::new(),
        phase: Phase::Final,
        required: true,
        obligation_ids: vec!["SPEC-001".into()],
        expected_case_ids: vec!["reserve-zero".into()],
        minimum_test_count: 1,
        success_exit_codes: vec![0],
        timeout_ms: 1000,
        output_limit_bytes: 4096,
        dependencies: vec![],
    };
    let registry = PlanRegistry {
        commands: [("test".into(), (CheckKind::Behavioral, d.clone()))].into(),
        configurations: [(d.clone(), ["SPEC-001".into()].into())].into(),
        ..Default::default()
    };
    (
        CheckPlan {
            schema_version: 1,
            run_id: "run".into(),
            contract_digest: d.clone(),
            design_digest: d.clone(),
            policy_digest: d.clone(),
            oracle_digest: d.clone(),
            configurations: vec![d],
            checks: vec![c],
            review_requirements: vec![],
            approval_requirements: vec![],
            non_applicable_obligations: vec![],
            approved_exceptions: vec![],
        },
        registry,
    )
}

#[test]
fn final_plans_must_cover_every_configuration_and_obligation() {
    let (plan, registry) = plan();
    plan.validate(&registry).unwrap();
    for change in 0..8 {
        let mut p = plan.clone();
        match change {
            0 => p.checks[0].required = false,
            1 => p.checks[0].phase = Phase::Fast,
            2 => p.checks[0].obligation_ids.clear(),
            3 => {
                p.checks[0].expected_case_ids.clear();
                p.checks[0].minimum_test_count = 0;
            }
            4 => p.checks[0].dependencies = vec!["cases".into()],
            5 => p.checks.push(p.checks[0].clone()),
            6 => p.configurations.push(Digest::of(b"missing-feature")),
            7 => p.checks[0].command_id = "agent-shell".into(),
            _ => unreachable!(),
        }
        assert!(p.validate(&registry).is_err(), "change {change}");
    }
    let mut r = registry;
    r.configurations
        .insert(Digest::of(b"missing-feature"), ["SPEC-001".into()].into());
    assert!(plan.validate(&r).is_err());
}

#[test]
fn every_native_profile_requires_pinned_assets_and_explicit_target_facts() {
    for language in [Language::Rust, Language::Cpp, Language::C] {
        let mut profile = Profile {
            build_root: String::new(),
            schema_version: 1,
            id: "fixture".into(),
            language,
            status: Resolution::Template,
            toolchain_lock: None,
            dependency_bundle: None,
            facts: BTreeMap::new(),
            configurations: vec![],
            public_consumer_commands: vec![],
        };
        assert!(profile.require_resolved().is_err());
        profile.status = Resolution::Resolved;
        profile.toolchain_lock = Some(Digest::of(b"pinned-tools"));
        profile.dependency_bundle = Some(Digest::of(b"pinned-dependencies"));
        profile.facts = language
            .required_fields()
            .iter()
            .map(|s| ((*s).into(), "explicit-fixture-value".into()))
            .collect();
        profile.configurations.push(Configuration {
            id: "default".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            features: vec![],
            default_features: true,
            build_targets: vec!["quota".into()],
            flags: vec![],
            runtime: "native-linux".into(),
        });
        profile.require_resolved().unwrap();
        profile.facts.remove(language.required_fields()[0]);
        assert!(profile.require_resolved().is_err());
    }
}

#[test]
fn legacy_invariant_serialization_retains_its_identity_and_all_configuration_cases() {
    let original: serde_json::Value =
        serde_json::from_slice(include_bytes!("../examples/quota-contract.v1.json")).unwrap();
    let contract = contract();
    assert_eq!(
        Digest::of_json(&original).unwrap(),
        Digest::of_json(&contract).unwrap()
    );
    for invariant in &contract.invariants {
        assert_eq!(
            invariant
                .cases_for_configuration(&Digest::of(b"any-target"))
                .unwrap(),
            invariant.case_ids
        );
    }
}

#[test]
fn configuration_case_mapping_requires_a_complete_nonempty_matrix_and_exact_case_union() {
    let mut contract = contract();
    contract.invariants.truncate(1);
    contract.invariants[0].case_ids = vec!["backend-case".into(), "frontend-case".into()];
    let backend = Digest::of(b"rust-target");
    let frontend = Digest::of(b"typescript-target");
    let mut registry = registry(&contract);
    registry.configurations = [backend.clone(), frontend.clone()].into();
    contract.invariants[0].cases_by_configuration = BTreeMap::from([
        (backend.clone(), vec!["backend-case".into()]),
        (frontend.clone(), vec!["frontend-case".into()]),
    ]);
    contract.validate(&registry).unwrap();
    assert_eq!(
        contract.invariants[0]
            .cases_for_configuration(&backend)
            .unwrap(),
        ["backend-case"]
    );
    assert!(contract.invariants[0]
        .cases_for_configuration(&Digest::of(b"unknown"))
        .is_err());
    for mutation in ["missing", "unknown", "empty", "duplicate", "union"] {
        let mut invalid = contract.clone();
        let mapping = &mut invalid.invariants[0].cases_by_configuration;
        match mutation {
            "missing" => {
                mapping.remove(&frontend);
            }
            "unknown" => {
                mapping.insert(Digest::of(b"unknown"), vec!["frontend-case".into()]);
            }
            "empty" => {
                mapping.insert(frontend.clone(), vec![]);
            }
            "duplicate" => {
                mapping.insert(
                    frontend.clone(),
                    vec!["frontend-case".into(), "frontend-case".into()],
                );
            }
            "union" => {
                mapping.insert(frontend.clone(), vec!["backend-case".into()]);
            }
            _ => unreachable!(),
        }
        assert!(invalid.validate(&registry).is_err(), "{mutation}");
    }
}
