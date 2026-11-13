#![cfg(target_os = "linux")]
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    contract::ContractRegistry,
    controller::{Controller, TaskSetup},
    fixtures,
    identity::Digest,
    ledger::Ledger,
    planning::{Catalog, CheckTemplate, PlanningInput},
    provider::{ModelRequest, ModelResponse, Provider, Role, Usage},
    roles::{self, DraftArtifact, DraftResponse, Preparation, RoleInput},
    runner::{self, Cancellation, ContainerBackend},
    state::State,
    wire, FailureCategory, Result,
};
use std::{collections::BTreeMap, path::Path};

fn setup(ledger: &Ledger, id: &str) -> TaskSetup {
    fixtures::quota_rust(ledger, id, Digest::of(b"fixture-image"), "base").unwrap()
}
fn registry(setup: &TaskSetup) -> ContractRegistry {
    ContractRegistry {
        profiles: setup.profiles.iter().map(|p| p.id.clone()).collect(),
        configurations: setup
            .profiles
            .iter()
            .flat_map(|p| &p.configurations)
            .map(|c| c.digest().unwrap())
            .collect(),
        suites: setup.suites.clone(),
        constraints: setup.constraint_names.clone(),
        risks: setup.risk_names.clone(),
    }
}
fn controller(path: &Path) -> Controller {
    Controller {
        ledger: Ledger::open(path).unwrap(),
        backend: ContainerBackend::local(Path::new("/bin/true")).unwrap(),
    }
}
fn approve(
    c: &Controller,
    setup: &TaskSetup,
    requirement: rovik_harness::plan::ApprovalRequirement,
) {
    c.ledger
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

#[test]
fn planning_derives_every_configuration_and_rejects_missing_capabilities_or_cases() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut setup = setup(&ledger, "plan");
    let mut second = setup.profiles[0].configurations[0].clone();
    second.id = "second-target-policy".into();
    second.flags.push("-Copt-level=2".into());
    setup.profiles[0].configurations.push(second);
    let checks: Vec<_> = setup.profiles[0]
        .configurations
        .iter()
        .map(|config| CheckTemplate {
            check_id: "contract".into(),
            profile_id: setup.profiles[0].id.clone(),
            configuration_id: config.id.clone(),
            command_id: setup.registry.commands[0].id.clone(),
            parameters: BTreeMap::new(),
            obligations: setup
                .contract
                .invariants
                .iter()
                .map(|i| i.id.clone())
                .collect(),
            case_ids: setup.plan.checks[0]
                .expected_case_ids
                .iter()
                .cloned()
                .collect(),
            minimum_test_count: 7,
            dependencies: vec![],
            fast: true,
        })
        .collect();
    let mut input = PlanningInput {
        run_id: setup.run_id,
        contract: setup.contract,
        design: setup.design,
        policy: setup.policy,
        oracle_digest: setup.oracle.digest().unwrap(),
        profiles: setup.profiles,
        registry: setup.registry,
        catalog: Catalog {
            schema_version: 1,
            checks,
            semantic_rules: vec![],
            candidate_reviews: vec!["design-review".into()],
        },
    };
    let plan = input.derive().unwrap();
    assert_eq!(plan.configurations.len(), 2);
    assert_eq!(plan.required_final().count(), 2);
    assert_eq!(plan.checks.len(), 4);
    let saved = input.catalog.checks.pop().unwrap();
    assert_eq!(
        input.derive().unwrap_err().category,
        FailureCategory::Capability
    );
    input.catalog.checks.push(saved);
    input.catalog.checks[0].case_ids.remove("reserve-exact");
    assert!(input
        .derive()
        .unwrap_err()
        .message
        .contains("protected case"));
}

#[test]
fn discovery_retains_tauri_build_roots_and_reuse_citations_cannot_name_invented_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let project = fixtures::snapshot(
        &ledger,
        "base",
        &[
            (
                "package.json",
                r#"{"devDependencies":{"typescript":"7.0.2"}}"#,
            ),
            ("src-tauri/Cargo.toml", "[package]\nname='app'"),
            ("src-tauri/tauri.conf.json", "{}"),
        ],
    )
    .unwrap();
    let inventory = rovik_harness::onboarding::inspect(&ledger, &project).unwrap();
    assert_eq!(inventory.units.len(), 2);
    assert_eq!(inventory.units[1].root, "src-tauri");
    assert!(inventory.unresolved.iter().any(|s| s.contains("IPC")));
    let mut setup = setup(&ledger, "reuse");
    rovik_harness::design::validate(&ledger, &setup.base, &setup.design).unwrap();
    setup.design.existing_code_considered[0].symbol = "invented_symbol".into();
    assert!(rovik_harness::design::validate(&ledger, &setup.base, &setup.design).is_err());
    setup.design.existing_code_considered[0].symbol = "Quota::reserve".into();
    setup.design.mechanism_decisions[0].mechanism = rovik_harness::design::Mechanism::Strategy;
    assert!(
        rovik_harness::design::validate(&ledger, &setup.base, &setup.design)
            .unwrap_err()
            .message
            .contains("evidence")
    );
}

struct Analyst {
    contract: rovik_harness::contract::TaskContract,
    invalid: bool,
}
impl Provider for Analyst {
    fn generate(&mut self, request: &ModelRequest, _: &Cancellation) -> Result<ModelResponse> {
        assert_eq!(request.role, Role::DomainAnalyst);
        let output = if self.invalid {
            serde_json::json!({"schema_version":1,"request_id":request.request_id,"approval":"self-approved"}).to_string()
        } else {
            serde_json::to_string(&DraftResponse {
                schema_version: 1,
                request_id: request.request_id.clone(),
                artifact: DraftArtifact::Contract {
                    contract: self.contract.clone(),
                },
            })
            .unwrap()
        };
        Ok(ModelResponse {
            request_id: request.request_id.clone(),
            provider_response_id: Some("scripted".into()),
            output,
            finish_reason: "stop".into(),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 20,
                cached_tokens: None,
                cost_microunits: Some(3),
            }),
        })
    }
}

#[test]
fn drafting_starts_without_an_approved_setup_and_promotion_preserves_actual_model_usage() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = setup(&c.ledger, "prepare");
    let prep = Preparation {
        schema_version: 1,
        run_id: setup.run_id.clone(),
        repository_id: setup.repository_id.clone(),
        request: "Repair equality reservations".into(),
        base: setup.base.clone(),
        registry: registry(&setup),
        authority: setup.authority.clone(),
        worker: setup.worker.clone(),
        budgets: setup.budgets.clone(),
        deadline_ms: setup.deadline_ms,
    };
    prep.create(&mut c.ledger).unwrap();
    let artifacts = BTreeMap::new();
    let input = RoleInput {
        run_id: &prep.run_id,
        role: Role::DomainAnalyst,
        snapshot: &prep.base,
        registry: &prep.registry,
        artifacts: &artifacts,
        limits: &prep.worker,
        deadline_ms: prep.deadline_ms,
    };
    let draft = roles::generate(
        &mut c.ledger,
        &input,
        &mut Analyst {
            contract: setup.contract.clone(),
            invalid: false,
        },
        &Cancellation::default(),
    )
    .unwrap();
    assert!(matches!(draft.artifact, DraftArtifact::Contract { .. }));
    assert_eq!(
        c.ledger.budgets(&setup.run_id).unwrap()["model_calls"].spent,
        1
    );
    assert!(c.ledger.approval_history(&setup.run_id).unwrap().is_empty());
    let promoted = c
        .promote_preparation(&Principal::current().unwrap(), &setup)
        .unwrap();
    assert_eq!(promoted.state, State::Baseline);
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["tokens"].spent, 30);
    assert!(!c.pending_approvals(&setup.run_id).unwrap().is_empty());
}

#[test]
fn a_role_cannot_issue_an_approval_and_both_format_attempts_are_charged() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let setup = setup(&ledger, "role-denial");
    let prep = Preparation {
        schema_version: 1,
        run_id: setup.run_id.clone(),
        repository_id: setup.repository_id.clone(),
        request: "Fix the boundary".into(),
        base: setup.base.clone(),
        registry: registry(&setup),
        authority: setup.authority.clone(),
        worker: setup.worker.clone(),
        budgets: setup.budgets.clone(),
        deadline_ms: setup.deadline_ms,
    };
    prep.create(&mut ledger).unwrap();
    let artifacts = BTreeMap::new();
    let input = RoleInput {
        run_id: &prep.run_id,
        role: Role::DomainAnalyst,
        snapshot: &prep.base,
        registry: &prep.registry,
        artifacts: &artifacts,
        limits: &prep.worker,
        deadline_ms: prep.deadline_ms,
    };
    assert!(roles::generate(
        &mut ledger,
        &input,
        &mut Analyst {
            contract: setup.contract,
            invalid: true
        },
        &Cancellation::default()
    )
    .is_err());
    assert_eq!(
        ledger.budgets(&prep.run_id).unwrap()["model_calls"].spent,
        2
    );
    assert_eq!(
        ledger.budgets(&prep.run_id).unwrap()["cost_microunits"].spent,
        6
    );
    assert!(ledger.approval_history(&prep.run_id).unwrap().is_empty());
}

#[test]
fn replan_activation_requires_exact_new_approvals_and_spends_its_budget_without_erasing_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let mut original = setup(&c.ledger, "replan");
    original.budgets.insert("replans".into(), 1);
    original.authority.actors.values_mut().for_each(|types| {
        types.insert("replan".into());
    });
    c.create(&original, runner::now_ms().unwrap()).unwrap();
    for next in [State::Baseline, State::ContractReady, State::DesignReady] {
        let current = c.ledger.run(&original.run_id).unwrap();
        let event = c
            .ledger
            .store_json(
                "event",
                &serde_json::json!({"fixture":next}),
                runner::now_ms().unwrap(),
            )
            .unwrap();
        c.ledger
            .transition(
                &current,
                next,
                &event.digest,
                None,
                runner::now_ms().unwrap(),
            )
            .unwrap();
    }
    let old_input = c.ledger.run(&original.run_id).unwrap().input_digest;
    let mut replacement = original.clone();
    replacement
        .design
        .variation_decisions
        .push("Keep the direct value object after semantic review.".into());
    replacement.plan.design_digest = Digest::of_json(&replacement.design).unwrap();
    let requirement = c
        .stage_replan(&Principal::current().unwrap(), &replacement)
        .unwrap();
    assert!(c
        .activate_replan(
            &Principal::current().unwrap(),
            &original.run_id,
            &requirement.subject_digest
        )
        .is_err());
    assert_eq!(
        c.ledger.budgets(&original.run_id).unwrap()["replans"].spent,
        0
    );
    for approval in replacement.approval_requirements(None).unwrap() {
        approve(&c, &replacement, approval);
    }
    approve(&c, &replacement, requirement.clone());
    let active = c
        .activate_replan(
            &Principal::current().unwrap(),
            &original.run_id,
            &requirement.subject_digest,
        )
        .unwrap();
    assert_eq!(active.state, State::Baseline);
    assert!(active.binding_digest.is_none());
    assert_eq!(
        c.ledger.budgets(&original.run_id).unwrap()["replans"].spent,
        1
    );
    let historical: TaskSetup = wire::decode(&c.ledger.read(&old_input).unwrap()).unwrap();
    assert_eq!(
        historical.design.variation_decisions,
        original.design.variation_decisions
    );
}

#[test]
fn a_preparation_with_unknown_provider_usage_can_be_recovered_and_reconciled_before_promotion() {
    struct UnknownUsage(Analyst);
    impl Provider for UnknownUsage {
        fn generate(
            &mut self,
            request: &ModelRequest,
            cancel: &Cancellation,
        ) -> Result<ModelResponse> {
            let mut response = self.0.generate(request, cancel)?;
            response.usage = None;
            Ok(response)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    c.backend = ContainerBackend::management();
    let setup = setup(&c.ledger, "prepare-recovery");
    let prep = Preparation {
        schema_version: 1,
        run_id: setup.run_id.clone(),
        repository_id: setup.repository_id.clone(),
        request: "Fix the boundary".into(),
        base: setup.base.clone(),
        registry: registry(&setup),
        authority: setup.authority.clone(),
        worker: setup.worker.clone(),
        budgets: setup.budgets.clone(),
        deadline_ms: setup.deadline_ms,
    };
    prep.create(&mut c.ledger).unwrap();
    let artifacts = BTreeMap::new();
    let input = RoleInput {
        run_id: &prep.run_id,
        role: Role::DomainAnalyst,
        snapshot: &prep.base,
        registry: &prep.registry,
        artifacts: &artifacts,
        limits: &prep.worker,
        deadline_ms: prep.deadline_ms,
    };
    assert!(roles::generate(
        &mut c.ledger,
        &input,
        &mut UnknownUsage(Analyst {
            contract: setup.contract,
            invalid: false
        }),
        &Cancellation::default()
    )
    .is_err());
    let operator = Principal::current().unwrap();
    c.recover(&operator, &prep.run_id).unwrap();
    assert!(c.resume(&operator, &prep.run_id).is_err());
    let operation = c.ledger.operations(&prep.run_id).unwrap().remove(0);
    c.reconcile(
        &operator,
        &prep.run_id,
        &rovik_harness::operations::Reconciliation {
            operation_id: operation.operation_id,
            usage: [
                ("model_calls".into(), 1),
                ("tokens".into(), 30),
                ("cost_microunits".into(), 3),
            ]
            .into(),
            reason: "The provider billing receipt supplies the missing usage.".into(),
        },
    )
    .unwrap();
    c.resume(&operator, &prep.run_id).unwrap();
    assert_eq!(c.ledger.run(&prep.run_id).unwrap().state, State::Intake);
    assert_eq!(
        c.ledger.budgets(&prep.run_id).unwrap()["model_calls"].spent,
        1
    );
    assert_eq!(
        c.ledger.budgets(&prep.run_id).unwrap()["tokens"].reserved,
        0
    );
}

#[test]
fn context_prefers_cited_source_after_the_first_twelve_paths_and_keeps_exact_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let names: Vec<_> = (0..30).map(|n| format!("source{n:02}.rs")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|path| (path.as_str(), "fn behavior() {}\n"))
        .collect();
    let snapshot = fixtures::snapshot(&ledger, "base", &files).unwrap();
    let ranked = rovik_harness::context::ranked_sources(
        &ledger,
        &snapshot,
        "behavior",
        &["source29.rs".into()],
        None,
    )
    .unwrap();
    assert_eq!(ranked.len(), 30);
    assert_eq!(ranked[0].item_id, "source:source29.rs");
    assert_eq!(ranked[0].source_revision, snapshot.digest().unwrap());
    assert_eq!(
        ranked[0].artifact_digest,
        snapshot.files["source29.rs"].content.digest
    );
}

#[test]
fn planner_uses_each_configuration_case_mapping_without_weakening_legacy_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let mut setup = setup(&ledger, "configuration-cases");
    let mut second = setup.profiles[0].configurations[0].clone();
    second.id = "other-platform".into();
    setup.profiles[0].configurations.push(second);
    let first = setup.profiles[0].configurations[0].digest().unwrap();
    let second = setup.profiles[0].configurations[1].digest().unwrap();
    for invariant in &mut setup.contract.invariants {
        let original = invariant.case_ids.clone();
        let other: Vec<_> = original
            .iter()
            .map(|case| format!("other-{case}"))
            .collect();
        invariant.case_ids.extend(other.clone());
        invariant.cases_by_configuration =
            BTreeMap::from([(first.clone(), original), (second.clone(), other)]);
    }
    setup.design.contract_digest = Digest::of_json(&setup.contract).unwrap();
    let checks = setup.profiles[0]
        .configurations
        .iter()
        .map(|config| CheckTemplate {
            check_id: "mapped-cases".into(),
            profile_id: setup.profiles[0].id.clone(),
            configuration_id: config.id.clone(),
            command_id: setup.registry.commands[0].id.clone(),
            parameters: BTreeMap::new(),
            obligations: setup
                .contract
                .invariants
                .iter()
                .map(|i| i.id.clone())
                .collect(),
            case_ids: setup
                .contract
                .invariants
                .iter()
                .flat_map(|i| {
                    i.cases_for_configuration(&config.digest().unwrap())
                        .unwrap()
                        .iter()
                        .cloned()
                })
                .collect(),
            minimum_test_count: 1,
            dependencies: vec![],
            fast: false,
        })
        .collect();
    let mut input = PlanningInput {
        run_id: setup.run_id,
        contract: setup.contract,
        design: setup.design,
        policy: setup.policy,
        oracle_digest: setup.oracle.digest().unwrap(),
        profiles: setup.profiles,
        registry: setup.registry,
        catalog: Catalog {
            schema_version: 1,
            checks,
            semantic_rules: vec![],
            candidate_reviews: vec![],
        },
    };
    let plan = input.derive().unwrap();
    assert_eq!(plan.required_final().count(), 2);
    assert!(plan
        .required_final()
        .find(|c| c.configuration_digest == first)
        .unwrap()
        .expected_case_ids
        .iter()
        .all(|case| !case.starts_with("other-")));
    assert!(plan
        .required_final()
        .find(|c| c.configuration_digest == second)
        .unwrap()
        .expected_case_ids
        .iter()
        .all(|case| case.starts_with("other-")));
    let saved = input.contract.clone();
    input.catalog.checks[1]
        .case_ids
        .remove("other-reserve-exact");
    assert!(input
        .derive()
        .unwrap_err()
        .message
        .contains("protected case"));
    input.catalog.checks[1]
        .case_ids
        .insert("other-reserve-exact".into());
    for invariant in &mut input.contract.invariants {
        invariant.cases_by_configuration.clear();
    }
    input.design.contract_digest = Digest::of_json(&input.contract).unwrap();
    assert!(input
        .derive()
        .unwrap_err()
        .message
        .contains("protected case"));
    input.contract = saved;
    input.contract.invariants[0]
        .cases_by_configuration
        .remove(&second);
    input.design.contract_digest = Digest::of_json(&input.contract).unwrap();
    assert!(input
        .derive()
        .unwrap_err()
        .message
        .contains("registered configurations"));
}

#[test]
fn setup_validation_cannot_collapse_two_profile_matrices_into_one_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&directory.path().join("state")).unwrap();
    let mut setup = setup(&ledger, "ambiguous-profile");
    let mut other = setup.profiles[0].clone();
    other.id = "different-build-root".into();
    other.build_root = "other".into();
    setup.contract.profile_ids.push(other.id.clone());
    setup.profiles.push(other);
    setup.design.contract_digest = Digest::of_json(&setup.contract).unwrap();
    assert!(setup
        .validate(&ledger)
        .unwrap_err()
        .message
        .contains("ambiguous configuration"));
}
