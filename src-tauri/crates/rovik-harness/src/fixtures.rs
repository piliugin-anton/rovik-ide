//! Explicitly illustrative quota fixture. These facts are not production defaults.

use crate::{
    approvals::{AuthorityPolicy, Principal},
    contract::{DesignRecord, ExistingCode, TaskContract},
    controller::{TaskSetup, WorkerLimits},
    identity::Digest,
    ledger::Ledger,
    packs,
    plan::{CheckPlan, CheckSpec, Phase},
    policy::EffectivePolicy,
    profile::{Configuration, Language, Profile, Resolution},
    registry::CommandRegistry,
    runner::{now_ms, Environment},
    snapshot::{AdmissionRules, FileEntry, Snapshot},
    wire, Result,
};

/// Reference implementation bytes supplied by the implementation specification.
pub const QUOTA_RUST: &str = include_str!("../fixtures/reference/quota/rust/lib.rs");
/// Stable illustrative acceptance inventory.
pub const QUOTA_CASES: [&str; 7] = [
    "reserve-success",
    "reserve-exact",
    "reserve-insufficient",
    "reserve-zero",
    "reserve-empty",
    "reserve-maximum",
    "reserve-insufficient-maximum",
];

/// Publish an explicit in-memory file tree as immutable input assets.
pub fn snapshot(ledger: &Ledger, revision: &str, files: &[(&str, &str)]) -> Result<Snapshot> {
    let now = now_ms()?;
    let entries = files
        .iter()
        .map(|(path, text)| {
            Ok((
                (*path).into(),
                FileEntry {
                    mode: 0o100644,
                    content: ledger.store(
                        "source",
                        "application/octet-stream",
                        text.as_bytes(),
                        now,
                    )?,
                },
            ))
        })
        .collect::<Result<_>>()?;
    Ok(Snapshot {
        schema_version: 1,
        base_revision: revision.into(),
        files: entries,
        deletions: vec![],
        dependency_assets: vec![],
    })
}

/// Prepare the reference Rust task with caller-supplied immutable image and base identity.
/// The returned setup deliberately has no approval; the operator must review it.
pub fn quota_rust(
    ledger: &Ledger,
    run_id: &str,
    image_digest: Digest,
    base_revision: &str,
) -> Result<TaskSetup> {
    let now = now_ms()?;
    let mut contract: TaskContract =
        wire::decode(include_bytes!("../examples/quota-contract.v2.json"))?;
    contract.profile_ids = vec!["quota-rust-host".into()];
    let bad = QUOTA_RUST.replace("requested > self.remaining", "requested >= self.remaining");
    let base = snapshot(
        ledger,
        base_revision,
        &[("implementations/quota/lib.rs", &bad)],
    )?;
    let oracle = snapshot(
        ledger,
        "protected-oracle-v1",
        &[
            (
                "consumer.rs",
                include_str!("../fixtures/reference/quota/rust/consumer.rs"),
            ),
            (
                "run.sh",
                include_str!("../fixtures/reference/quota/rust/run.sh"),
            ),
        ],
    )?;
    let dependencies = snapshot(ledger, "no-external-dependencies", &[])?;
    let lock=ledger.store_json("environment",&serde_json::json!({"image":image_digest,"rustc":"1.85.1","target":"x86_64-unknown-linux-gnu"}),now)?;
    let configuration = Configuration {
        id: "quota-rust-debug".into(),
        target: "x86_64-unknown-linux-gnu".into(),
        features: vec![],
        default_features: false,
        build_targets: vec!["protected-consumer".into()],
        flags: vec![
            "--edition=2021".into(),
            "-Cpanic=unwind".into(),
            "-Coverflow-checks=yes".into(),
            "-Dwarnings".into(),
            "-Funsafe-code".into(),
        ],
        runtime: "linux-native".into(),
    };
    let profile = Profile {
        schema_version: 1,
        id: "quota-rust-host".into(),
        language: Language::Rust,
        status: Resolution::Resolved,
        toolchain_lock: Some(lock.digest.clone()),
        dependency_bundle: Some(dependencies.digest()?),
        facts: [
            ("edition", "2021"),
            ("minimum_supported_rust_version", "1.85.1"),
            ("execution_target", "x86_64-unknown-linux-gnu"),
            ("std_or_no_std", "std"),
            ("allocation_policy", "no allocation in quota"),
            ("panic_policy", "unwind in the protected test consumer"),
            ("unsafe_scope", "forbidden"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
        configurations: vec![configuration.clone()],
        public_consumer_commands: vec!["quota-rust-consumer".into()],
    };
    let command = packs::rust_reference_command(&profile)?;
    let registry = CommandRegistry {
        commands: vec![command.clone()],
    };
    let policy = EffectivePolicy {
        commands: [command.id.clone()].into(),
        obligations: Default::default(),
        limits: Default::default(),
        facts: Default::default(),
        scopes: vec![contract.scope.clone()],
    };
    let design=DesignRecord {schema_version:1,id:"quota-value-object".into(),contract_digest:Digest::of_json(&contract)?,existing_code_considered:vec![ExistingCode {file:"implementations/quota/lib.rs".into(),symbol:"Quota::reserve".into(),source_revision:base.digest()?,suitability:"Keep the existing value object and positive Amount; repair the comparison boundary.".into()}],components:[("quota".into(),"Own quota state and validate reservations without IO or allocation.".into())].into(),invariant_owners:contract.invariants.iter().map(|i|(i.id.clone(),"quota".into())).collect(),public_api_changes:vec!["Preserve Amount, Quota and ReserveError signatures.".into()],resource_contracts:vec!["Caller-owned value; exclusive mutable borrow; no heap ownership.".into()],error_contracts:vec!["Reject zero or insufficient amount without state mutation.".into()],concurrency_contract:"Caller serializes operations on the same value.".into(),variation_decisions:vec!["No new abstraction or dependency is required.".into()],verification_obligations:vec![],unknowns:vec![]};
    let plan = CheckPlan {
        schema_version: 1,
        run_id: run_id.into(),
        contract_digest: Digest::of_json(&contract)?,
        design_digest: Digest::of_json(&design)?,
        policy_digest: Digest::of_json(&policy)?,
        oracle_digest: oracle.digest()?,
        configurations: vec![configuration.digest()?],
        checks: vec![CheckSpec {
            check_id: "rust-quota-contract".into(),
            configuration_digest: configuration.digest()?,
            checker_digest: command.checker_digest.clone(),
            command_id: command.id.clone(),
            validated_parameters: Default::default(),
            phase: Phase::Final,
            required: true,
            obligation_ids: contract.invariants.iter().map(|i| i.id.clone()).collect(),
            expected_case_ids: QUOTA_CASES.iter().map(|s| (*s).into()).collect(),
            minimum_test_count: 7,
            success_exit_codes: vec![0],
            timeout_ms: command.limits.timeout_ms,
            output_limit_bytes: command.limits.output_bytes,
            dependencies: vec![],
        }],
        review_requirements: vec![],
        approval_requirements: vec![],
        non_applicable_obligations: vec![],
        approved_exceptions: vec![],
    };
    let environment = Environment {
        schema_version: 1,
        image_digest,
        backend_version: crate::runner::BACKEND_VERSION.into(),
        toolchain_digest: lock.digest,
        dependency_digest: dependencies.digest()?,
        registry_digest: Digest::of_json(&registry)?,
        locale: "C".into(),
        timezone: "UTC".into(),
    };
    let authority = AuthorityPolicy {
        actors: [(
            Principal::current()?.actor_id().into(),
            [
                "domain-contract".into(),
                "design".into(),
                "oracle".into(),
                "revoke".into(),
                "operate".into(),
                "accounting".into(),
                "failure-resolution".into(),
            ]
            .into(),
        )]
        .into(),
    };
    Ok(TaskSetup {
        schema_version: 1,
        run_id: run_id.into(),
        repository_id: "quota-fixture".into(),
        constraint_names: contract.constraints.keys().cloned().collect(),
        risk_names: contract.risk_tags.iter().cloned().collect(),
        suites: [(
            "quota.reserve.cases.v1".into(),
            QUOTA_CASES.iter().map(|s| (*s).into()).collect(),
        )]
        .into(),
        contract,
        design,
        profiles: vec![profile],
        negative_control: base.clone(),
        base,
        oracle,
        dependencies,
        registry,
        environment,
        policy,
        risk_policy: Default::default(),
        admission: AdmissionRules::default(),
        plan,
        authority,
        budgets: [
            ("model_calls".into(), 4),
            ("tokens".into(), 64_000),
            ("cost_microunits".into(), 1_000_000),
            ("checks".into(), 8),
        ]
        .into(),
        worker: WorkerLimits {
            model_id: "fixture-scripted".into(),
            context_bytes: 64 * 1024,
            output_tokens: 8192,
            timeout_ms: 120_000,
            token_reservation: 16_000,
            cost_reservation: 250_000,
            format_repairs: 1,
        },
        negative_control_cases: vec!["reserve-exact".into()],
        deadline_ms: now + 3_600_000,
    })
}

/// Reference C/C++ task using separately compiled protected public API consumers.
pub fn quota_c_family(
    ledger: &Ledger,
    run_id: &str,
    image: Digest,
    revision: &str,
    language: Language,
) -> Result<TaskSetup> {
    crate::require(
        matches!(language, Language::C | Language::Cpp),
        "expected C or C++ fixture",
    )?;
    let mut setup = quota_rust(ledger, run_id, image, revision)?;
    let cpp = language == Language::Cpp;
    let path = if cpp {
        "implementations/quota/quota.hpp"
    } else {
        "implementations/quota/quota.c"
    };
    let source = if cpp {
        include_str!("../fixtures/reference/quota/cpp/quota.hpp")
    } else {
        include_str!("../fixtures/reference/quota/c/quota.c")
    };
    let bad = if cpp {
        source.replace(
            "amount.value() > remaining_",
            "amount.value() >= remaining_",
        )
    } else {
        source.replace("amount > self->remaining", "amount >= self->remaining")
    };
    let mut files = vec![(path, bad.as_str())];
    if !cpp {
        files.push((
            "implementations/quota/quota.h",
            include_str!("../fixtures/reference/quota/c/quota.h"),
        ));
    }
    setup.base = snapshot(ledger, revision, &files)?;
    setup.negative_control = setup.base.clone();
    let mut profile = setup.profiles[0].clone();
    profile.id = if cpp {
        "quota-cpp-host"
    } else {
        "quota-c-host"
    }
    .into();
    profile.language = language;
    profile.facts = if cpp {
        [
            ("standard_library_lock", "gcc-12-libstdc++-image-lock"),
            ("language_standard", "23"),
            ("target_abi", "x86_64-linux-gnu"),
            ("exceptions_policy", "disabled"),
            ("rtti_policy", "disabled"),
            ("allocation_policy", "no allocation in quota"),
            (
                "abi_compatibility_policy",
                "source-compatible public header",
            ),
            ("threading_model", "caller serialized"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    } else {
        [
            ("language_standard", "17"),
            ("target_abi", "x86_64-linux-gnu"),
            ("hosted_or_freestanding", "hosted"),
            ("allocation_policy", "no allocation in quota"),
            (
                "threading_and_interrupt_policy",
                "caller serialized; no interrupts",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    };
    let config = &mut profile.configurations[0];
    config.id = if cpp {
        "quota-cpp-debug"
    } else {
        "quota-c-debug"
    }
    .into();
    config.target = "x86_64-linux-gnu".into();
    config.flags = vec!["-Wall".into(), "-Wextra".into(), "-Werror".into()];
    if cpp {
        config
            .flags
            .extend(["-fno-exceptions".into(), "-fno-rtti".into()]);
    }
    let lock = ledger.store_json("environment", &serde_json::json!({"image":setup.environment.image_digest,"gcc":"12","c_standard":"17","cpp_standard":"23","target":"x86_64-linux-gnu"}), now_ms()?)?;
    setup.environment.toolchain_digest = lock.digest.clone();
    profile.toolchain_lock = Some(lock.digest);
    let command = packs::c_family_reference_command(&profile)?;
    profile.public_consumer_commands = vec![command.id.clone()];
    let mut cases: Vec<String> = QUOTA_CASES.iter().map(|s| (*s).into()).collect();
    if !cpp {
        cases.push("null-preserves-output".into());
    }
    let serialized_profile =
        serde_json::to_string(&profile).map_err(|e| crate::Error(e.to_string()))?;
    let serialized_cases =
        serde_json::to_string(&cases).map_err(|e| crate::Error(e.to_string()))?;
    setup.oracle = snapshot(
        ledger,
        "protected-native-oracle-v1",
        &[
            ("profile.json", &serialized_profile),
            ("cases.json", &serialized_cases),
            ("run-consumer.py", include_str!("../assets/run-consumer.py")),
            (
                if cpp { "consumer.cpp" } else { "consumer.c" },
                if cpp {
                    include_str!("../fixtures/reference/quota/cpp/consumer.cpp")
                } else {
                    include_str!("../fixtures/reference/quota/c/consumer.c")
                },
            ),
        ],
    )?;
    setup.registry.commands = vec![command.clone()];
    setup.policy.commands = [command.id.clone()].into();
    setup.contract.profile_ids = vec![profile.id.clone()];
    setup.design.contract_digest = Digest::of_json(&setup.contract)?;
    setup.design.existing_code_considered[0].file = path.into();
    setup.design.existing_code_considered[0].symbol = if cpp {
        "Quota::reserve"
    } else {
        "quota_reserve"
    }
    .into();
    setup.design.existing_code_considered[0].source_revision = setup.base.digest()?;
    setup.design.public_api_changes = vec!["Preserve the existing native public API.".into()];
    setup.design.resource_contracts = vec!["Caller-owned quota value; no heap ownership.".into()];
    let check = &mut setup.plan.checks[0];
    check.check_id = command.id.clone();
    check.command_id = command.id;
    check.checker_digest = command.checker_digest;
    check.timeout_ms = command.limits.timeout_ms;
    check.output_limit_bytes = command.limits.output_bytes;
    check.configuration_digest = profile.configurations[0].digest()?;
    check.expected_case_ids = cases;
    check.minimum_test_count = check.expected_case_ids.len() as u64;
    setup.plan.configurations = vec![check.configuration_digest.clone()];
    setup.plan.contract_digest = Digest::of_json(&setup.contract)?;
    setup.plan.design_digest = Digest::of_json(&setup.design)?;
    setup.plan.policy_digest = Digest::of_json(&setup.policy)?;
    setup.plan.oracle_digest = setup.oracle.digest()?;
    setup.environment.registry_digest = Digest::of_json(&setup.registry)?;
    setup.profiles = vec![profile];
    Ok(setup)
}
