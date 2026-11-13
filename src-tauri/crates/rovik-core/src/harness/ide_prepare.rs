//! Typed, reviewable workspace onboarding. Preparing inputs never grants approval.
//!
//! The wizard selects real committed tests and a real failing revision. Immutable
//! inputs and closed adapters become a derived plan; actual baseline and control
//! execution remain mandatory before a candidate can receive a passing verdict.

use super::GitIntegration;
use rovik_harness::{
    approvals::{AuthorityPolicy, Principal},
    baseline::{BaselinePlan, ExpectedFailure},
    contract::{DesignRecord, ExistingCode, Invariant, TaskContract},
    controller::{TaskSetup, WorkerLimits},
    design::{Mechanism, MechanismDecision},
    identity::Digest,
    ledger::Ledger,
    native_checks::{NativeCheck, NativeCheckInput},
    onboarding::{self, ProfileSelection},
    pack_protocol::{NativeInput, Operation, Request},
    packs,
    planning::{Catalog, CheckTemplate, PlanningInput},
    policy::EffectivePolicy,
    profile::{Configuration, Language, Profile, Resolution},
    registry::CommandRegistry,
    runner::{self, Environment},
    runtime::TestInput,
    scope::Scope,
    snapshot::{AdmissionRules, FileEntry, Snapshot},
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

/// Human-entered inputs for the first setup in a repository, not a proposed plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceSetupInput {
    /// Stable task label.
    pub name: String,
    /// Requested outcome from the reviewed implementation plan.
    pub intent: String,
    /// Explicit domain context; manifests cannot establish domain meaning.
    pub bounded_context: String,
    /// Literal file paths or directory patterns ending in `/**`.
    pub write_paths: Vec<String>,
    /// Additional paths which implementation must preserve.
    pub protected_paths: Vec<String>,
    /// Every discovered build root needs an explicit resolved selection.
    pub profiles: Vec<WorkspaceProfileInput>,
    /// Required behavior and cases for each configuration.
    pub requirements: Vec<WorkspaceRequirement>,
    /// Reviewed ownership and reuse decisions.
    pub design: WorkspaceDesignInput,
    /// Prepared host assets selected by the operator.
    pub environment: WorkspaceEnvironmentInput,
    /// An existing Git revision whose relevant behavior is known to fail.
    pub negative_control_revision: String,
    /// Cases from one behavioral check which must actually fail on the control.
    pub negative_control_cases: Vec<String>,
    /// Explicit historical failures; all other baseline checks must pass.
    #[serde(default)]
    pub baseline_failures: Vec<WorkspaceBaselineFailure>,
    /// Selected model and bounded provider requests.
    pub worker: WorkerLimits,
    /// Explicit model_calls, tokens, cost_microunits and checks accounts.
    pub budgets: BTreeMap<String, u64>,
    /// Maximum run lifetime; registration never creates an unlimited deadline.
    pub duration_ms: u64,
}

/// One explicitly configured build root and its independently protected tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceProfileInput {
    /// Stable profile identity, used in requirement mapping keys.
    pub id: String,
    /// Repository-relative discovered manifest path.
    pub manifest: String,
    /// Registered adapter family.
    pub language: Language,
    /// Explicit language facts; required names are supplied by the language profile.
    pub facts: BTreeMap<String, String>,
    /// Exact supported targets and features.
    pub configurations: Vec<Configuration>,
    /// Existing committed test files. Native tests stay frozen in the source tree;
    /// runtime .mjs/.py tests are copied to the protected oracle and must exercise
    /// `/build/project` (TypeScript output is `/build/project/dist`).
    pub protected_test_paths: Vec<String>,
    /// Existing protected TypeScript compiler configuration, when applicable.
    #[serde(default)]
    pub typescript_project: Option<String>,
}

/// A requirement's mapping uses readable `profileId/configurationId` keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRequirement {
    /// Stable requirement ID.
    pub id: String,
    /// Behavior to establish, not an inferred domain assumption.
    pub statement: String,
    /// One declared component responsible for this behavior.
    pub owner: String,
    /// Nonempty exact native case IDs for every selected configuration.
    pub cases_by_configuration: BTreeMap<String, Vec<String>>,
}

/// Source evidence inspected before selecting a design.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceCitation {
    /// Existing repository-relative file.
    pub file: String,
    /// Existing symbol whose lexical presence is independently checked.
    pub symbol: String,
    /// Why this code can or cannot satisfy the requirement.
    pub rationale: String,
}

/// A direct function/module design; more elaborate mechanisms use reviewed role drafts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceDesignInput {
    /// Component responsibility descriptions.
    pub components: BTreeMap<String, String>,
    /// Explicit source reuse analysis.
    pub existing_code: Vec<WorkspaceCitation>,
    /// Public API and compatibility decisions.
    pub public_api_changes: Vec<String>,
    /// Ownership and resource lifecycle decisions.
    pub resource_contracts: Vec<String>,
    /// Failure behavior decisions.
    pub error_contracts: Vec<String>,
    /// Synchronization and cancellation assumptions.
    pub concurrency_contract: String,
    /// Reason a direct function/module is sufficient.
    pub mechanism_pressure: String,
    /// Simpler alternatives considered and their tradeoffs.
    pub mechanism_alternatives: Vec<String>,
    /// Ownership and maintenance cost of this decision.
    pub mechanism_cost: String,
}

/// Prepared execution environment; verification never installs dependencies or pulls images.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceEnvironmentInput {
    /// Locally prepared image's immutable sha256 config digest.
    pub image_digest: Digest,
    /// Explicit execution engine, either docker or podman.
    pub engine: String,
    /// Absolute selected host file describing the pinned toolchain.
    pub toolchain_manifest_path: String,
    /// Absolute selected prepared offline dependency directory.
    pub dependency_directory: String,
}

/// A real failing baseline inventory, bound to the derived behavioral check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceBaselineFailure {
    /// Selected profile identity.
    pub profile_id: String,
    /// Selected configuration identity.
    pub configuration_id: String,
    /// Exact failures expected on the unchanged baseline.
    pub case_ids: BTreeSet<String>,
    /// Why these failures are part of the requested change.
    pub rationale: String,
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message.into()))
    }
}

/// Construct and validate the first setup from explicit reviewed wizard inputs.
/// `baseline` comes from the trusted IDE integration, including cloud parent refs.
/// This function grants no approvals, launches no model, and runs no project code.
pub fn build(
    project_root: &Path,
    ledger: &Ledger,
    repository_id: &str,
    baseline: &Snapshot,
    input: &WorkspaceSetupInput,
) -> Result<TaskSetup> {
    baseline.validate(ledger, true)?;
    input.worker.validate()?;
    ensure(
        input.duration_ms > 0 && input.duration_ms <= 86_400_000,
        "choose a run duration between one millisecond and one day",
    )?;
    ensure(
        !input.profiles.is_empty() && !input.requirements.is_empty(),
        "select build profiles and at least one behavioral requirement",
    )?;
    let now = runner::now_ms()?;
    let backend_version = match input.environment.engine.as_str() {
        "docker" => runner::BACKEND_VERSION,
        "podman" => runner::PODMAN_BACKEND_VERSION,
        _ => return Err(Error::new("select Docker or Podman explicitly".into())),
    };
    let toolchain_bytes = selected_file(
        Path::new(&input.environment.toolchain_manifest_path),
        8 * 1024 * 1024,
    )?;
    ensure(
        !toolchain_bytes.is_empty(),
        "selected toolchain manifest is empty",
    )?;
    let toolchain = ledger
        .store(
            "environment",
            "application/octet-stream",
            &toolchain_bytes,
            now,
        )?
        .digest;
    let dependencies = dependency_snapshot(
        ledger,
        Path::new(&input.environment.dependency_directory),
        now,
    )?;
    let dependency_digest = ledger.store_json("snapshot", &dependencies, now)?.digest;
    let inventory = onboarding::inspect(ledger, baseline)?;
    let mut selected = Vec::new();
    let mut configuration_keys = BTreeMap::new();
    for choice in &input.profiles {
        ensure(
            !choice.protected_test_paths.is_empty(),
            format!("{} needs protected existing test files", choice.id),
        )?;
        ensure(
            !choice.id.contains('/'),
            "profile ID cannot contain a slash",
        )?;
        let unit = inventory
            .units
            .iter()
            .find(|unit| unit.manifest == choice.manifest)
            .ok_or_else(|| {
                Error::new(format!(
                    "manifest is absent from the trusted baseline: {}",
                    choice.manifest
                ))
            })?;
        let profile = Profile {
            schema_version: 1,
            id: choice.id.clone(),
            build_root: unit.root.clone(),
            language: choice.language,
            status: Resolution::Resolved,
            toolchain_lock: Some(toolchain.clone()),
            dependency_bundle: Some(dependency_digest.clone()),
            facts: choice.facts.clone(),
            configurations: choice.configurations.clone(),
            public_consumer_commands: vec![],
        };
        for config in &profile.configurations {
            ensure(
                !config.id.contains('/'),
                "configuration ID cannot contain a slash",
            )?;
            ensure(
                configuration_keys
                    .insert(format!("{}/{}", profile.id, config.id), config.digest()?)
                    .is_none(),
                "duplicate profile/configuration selection",
            )?;
        }
        selected.push(ProfileSelection {
            manifest: choice.manifest.clone(),
            profile,
        });
    }
    let profiles = onboarding::resolve(ledger, &inventory, &selected)?;
    let mut protected: BTreeSet<String> = input.protected_paths.iter().cloned().collect();
    // Build scripts and manifests can change which tests execute. The initial
    // wizard keeps them fixed; a separately reviewed setup can admit build edits.
    protected.extend(
        baseline
            .files
            .keys()
            .filter(|p| rovik_harness::snapshot::is_build_input(p))
            .cloned(),
    );
    let mut oracle = Snapshot {
        schema_version: 1,
        base_revision: baseline.base_revision.clone(),
        files: BTreeMap::new(),
        deletions: vec![],
        dependency_assets: vec![],
    };
    for choice in &input.profiles {
        for path in choice
            .protected_test_paths
            .iter()
            .chain(choice.typescript_project.iter())
        {
            rovik_harness::scope::validate_path(path)?;
            ensure(
                !path.starts_with("requests/"),
                "protected tests cannot occupy reserved request paths",
            )?;
            let entry = baseline.files.get(path).ok_or_else(|| {
                Error::new(format!(
                    "protected test/configuration must exist in the committed baseline: {path}"
                ))
            })?;
            protected.insert(path.clone());
            oracle.files.insert(path.clone(), entry.clone());
        }
    }
    for (profile, choice) in profiles.iter().zip(&input.profiles) {
        if profile.language == Language::TypeScript {
            let path = choice
                .typescript_project
                .as_deref()
                .ok_or_else(|| Error::new("TypeScript needs a selected compiler project".into()))?;
            let bytes = rebase_typescript_project(ledger, baseline, profile, path)?;
            let content = ledger.store("source", "application/json", &bytes, now)?;
            oracle.files.insert(
                path.into(),
                FileEntry {
                    mode: 0o100644,
                    content,
                },
            );
        }
    }
    let scope = Scope {
        allow_write: input.write_paths.clone(),
        protected: protected.into_iter().collect(),
    };
    scope.validate()?;
    let mut invariants = Vec::new();
    for requirement in &input.requirements {
        ensure(
            requirement
                .cases_by_configuration
                .keys()
                .collect::<BTreeSet<_>>()
                == configuration_keys.keys().collect(),
            format!(
                "requirement {} must list cases for every profile/configuration",
                requirement.id
            ),
        )?;
        let mapping: BTreeMap<_, _> = requirement
            .cases_by_configuration
            .iter()
            .map(|(key, cases)| (configuration_keys[key].clone(), cases.clone()))
            .collect();
        let cases = mapping
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        invariants.push(Invariant {
            id: requirement.id.clone(),
            statement: requirement.statement.clone(),
            owner: requirement.owner.clone(),
            case_ids: cases,
            cases_by_configuration: mapping,
        });
    }
    let all_cases: BTreeSet<String> = invariants
        .iter()
        .flat_map(|i| i.case_ids.iter().cloned())
        .collect();
    let suite_id = "workspace-protected-cases".to_string();
    let contract = TaskContract {
        schema_version: 1,
        id: input.name.clone(),
        bounded_context: input.bounded_context.clone(),
        profile_ids: profiles.iter().map(|p| p.id.clone()).collect(),
        intent: input.intent.clone(),
        scope: scope.clone(),
        invariants,
        acceptance_suite_ids: vec![suite_id.clone()],
        constraints: BTreeMap::new(),
        risk_tags: vec![],
        unknowns: vec![],
        numeric_contract: None,
    };
    let design = design(input, baseline, &contract)?;
    let mut commands = BTreeMap::new();
    let mut templates = Vec::new();
    for (profile, choice) in profiles.iter().zip(&input.profiles) {
        for config in &profile.configurations {
            let digest = config.digest()?;
            let cases: BTreeSet<String> = contract
                .invariants
                .iter()
                .map(|i| i.cases_for_configuration(&digest))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            let request_id = format!(
                "workspace-{}",
                &Digest::of(format!("{}/{}", profile.id, config.id).as_bytes()).hex()[..24]
            );
            let (request, mut command) =
                check_request(profile, choice, config, &request_id, &cases)?;
            command.id = format!(
                "workspace-tests-{}",
                &Digest::of(profile.id.as_bytes()).hex()[..16]
            );
            // Each profile has a separate closed choice set so configuration
            // requests cannot select a different profile through parameters.
            if let Some(existing) = commands.get_mut(&command.id) {
                let existing: &mut rovik_harness::registry::CommandDefinition = existing;
                if let Some(rovik_harness::registry::Parameter::Choice { values }) =
                    existing.parameters.get_mut("request_id")
                {
                    values.insert(request_id.clone());
                }
            } else {
                commands.insert(command.id.clone(), command.clone());
            }
            let mut request_bytes = wire::canonical(&request)?;
            request_bytes.push(b'\n');
            let content = ledger.store("source", "application/json", &request_bytes, now)?;
            oracle.files.insert(
                format!("requests/{request_id}.json"),
                FileEntry {
                    mode: 0o100644,
                    content,
                },
            );
            templates.push(CheckTemplate {
                check_id: check_id(&profile.id),
                profile_id: profile.id.clone(),
                configuration_id: config.id.clone(),
                command_id: command.id,
                parameters: BTreeMap::from([("request_id".into(), request_id)]),
                obligations: contract.invariants.iter().map(|i| i.id.clone()).collect(),
                minimum_test_count: cases.len() as u64,
                case_ids: cases,
                dependencies: vec![],
                fast: false,
            });
        }
    }
    let registry = CommandRegistry {
        commands: commands.into_values().collect(),
    };
    let commands_by_configuration = templates
        .iter()
        .map(|template| {
            let key = format!("{}/{}", template.profile_id, template.configuration_id);
            (
                configuration_keys[&key].clone(),
                BTreeSet::from([template.command_id.clone()]),
            )
        })
        .collect();
    // This route is part of the reviewed policy and never authorizes a review.
    // Other risks still require dedicated evidence through a separate setup.
    let risk_policy = rovik_harness::risk::RiskPolicy {
        routes: BTreeMap::from([(
            "public-api".into(),
            rovik_harness::risk::RiskRoute {
                commands: registry
                    .commands
                    .iter()
                    .map(|command| command.id.clone())
                    .collect(),
                commands_by_configuration,
                reviews: BTreeSet::from(["design-review".into()]),
                concurrency_cases: BTreeMap::new(),
            },
        )]),
    };
    let policy = EffectivePolicy {
        commands: registry.commands.iter().map(|c| c.id.clone()).collect(),
        obligations: BTreeSet::new(),
        limits: input.budgets.clone(),
        facts: BTreeMap::from([(
            "risk_policy_digest".into(),
            Digest::of_json(&risk_policy)?.to_string(),
        )]),
        scopes: vec![scope],
    };
    let run_id = format!("prepared-{}", uuid::Uuid::new_v4());
    let plan = PlanningInput {
        run_id: run_id.clone(),
        contract: contract.clone(),
        design: design.clone(),
        policy: policy.clone(),
        oracle_digest: oracle.digest()?,
        profiles: profiles.clone(),
        registry: registry.clone(),
        catalog: Catalog {
            schema_version: 1,
            checks: templates,
            semantic_rules: vec![],
            candidate_reviews: vec!["design-review".into()],
        },
    }
    .derive()?;
    ensure(
        plan.required_final().any(|check| {
            input
                .negative_control_cases
                .iter()
                .all(|case| check.expected_case_ids.contains(case))
        }),
        "negative control cases must belong to one configured behavioral check",
    )?;
    let negative_control =
        revision_snapshot(project_root, ledger, &input.negative_control_revision)?;
    for choice in &input.profiles {
        if matches!(
            choice.language,
            Language::Rust | Language::C | Language::Cpp
        ) {
            for path in &choice.protected_test_paths {
                ensure(
                    baseline.files.get(path) == negative_control.files.get(path),
                    format!("negative control must preserve protected native test bytes: {path}"),
                )?;
            }
            for path in baseline
                .files
                .keys()
                .filter(|p| rovik_harness::snapshot::is_build_input(p))
            {
                ensure(
                    baseline.files.get(path) == negative_control.files.get(path),
                    format!(
                        "negative control must preserve native build/test registration: {path}"
                    ),
                )?;
            }
        }
    }
    let baseline_plan = BaselinePlan {
        expected_failures: input
            .baseline_failures
            .iter()
            .map(|failure| {
                let key = format!("{}/{}", failure.profile_id, failure.configuration_id);
                let configuration_digest = configuration_keys
                    .get(&key)
                    .ok_or_else(|| {
                        Error::new(format!(
                            "baseline failure names an unknown configuration: {key}"
                        ))
                    })?
                    .clone();
                Ok(ExpectedFailure {
                    check_id: check_id(&failure.profile_id),
                    configuration_digest,
                    case_ids: failure.case_ids.clone(),
                    observed_signature: None,
                    rationale: failure.rationale.clone(),
                })
            })
            .collect::<Result<_>>()?,
    };
    let principal = Principal::current()?;
    let authority = AuthorityPolicy {
        actors: BTreeMap::from([(
            principal.actor_id().to_string(),
            [
                "operate",
                "baseline-plan",
                "domain-contract",
                "design",
                "oracle",
                "design-review",
                "delivery",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        )]),
    };
    let environment = Environment {
        schema_version: 1,
        image_digest: input.environment.image_digest.clone(),
        backend_version: backend_version.into(),
        toolchain_digest: toolchain,
        dependency_digest,
        registry_digest: Digest::of_json(&registry)?,
        locale: "C".into(),
        timezone: "UTC".into(),
    };
    let setup = TaskSetup {
        schema_version: 1,
        run_id,
        repository_id: repository_id.into(),
        contract,
        design,
        profiles,
        base: baseline.clone(),
        initial_candidate: None,
        baseline: baseline_plan,
        oracle,
        dependencies,
        registry,
        environment,
        policy,
        exceptions: vec![],
        risk_policy,
        admission: AdmissionRules::default(),
        plan,
        suites: BTreeMap::from([(suite_id, all_cases)]),
        constraint_names: BTreeSet::new(),
        risk_names: BTreeSet::new(),
        authority,
        budgets: input.budgets.clone(),
        worker: input.worker.clone(),
        reviewer: None,
        negative_control,
        negative_control_cases: input.negative_control_cases.clone(),
        deadline_ms: now
            .checked_add(input.duration_ms)
            .ok_or_else(|| Error::new("run deadline overflow".into()))?,
    };
    setup.validate(ledger)?;
    Ok(setup)
}

fn check_id(profile: &str) -> String {
    format!("tests-{}", &Digest::of(profile.as_bytes()).hex()[..16])
}

fn design(
    input: &WorkspaceSetupInput,
    baseline: &Snapshot,
    contract: &TaskContract,
) -> Result<DesignRecord> {
    let owners: BTreeSet<_> = contract
        .invariants
        .iter()
        .map(|i| i.owner.clone())
        .collect();
    let reuse: Vec<_> = input
        .design
        .existing_code
        .iter()
        .map(|c| c.file.clone())
        .collect();
    Ok(DesignRecord {
        schema_version: 1,
        id: format!("{}-design", input.name),
        contract_digest: Digest::of_json(contract)?,
        existing_code_considered: input
            .design
            .existing_code
            .iter()
            .map(|c| {
                Ok(ExistingCode {
                    file: c.file.clone(),
                    symbol: c.symbol.clone(),
                    source_revision: baseline.digest()?,
                    suitability: c.rationale.clone(),
                })
            })
            .collect::<Result<_>>()?,
        components: input.design.components.clone(),
        invariant_owners: contract
            .invariants
            .iter()
            .map(|i| (i.id.clone(), i.owner.clone()))
            .collect(),
        public_api_changes: input.design.public_api_changes.clone(),
        resource_contracts: input.design.resource_contracts.clone(),
        error_contracts: input.design.error_contracts.clone(),
        concurrency_contract: input.design.concurrency_contract.clone(),
        variation_decisions: vec![input.design.mechanism_pressure.clone()],
        mechanism_decisions: owners
            .into_iter()
            .map(|owner| MechanismDecision {
                owner,
                pressure: input.design.mechanism_pressure.clone(),
                mechanism: Mechanism::FunctionOrModule,
                alternatives: input.design.mechanism_alternatives.clone(),
                cost: input.design.mechanism_cost.clone(),
                reuse: reuse.clone(),
                evidence: vec![],
            })
            .collect(),
        verification_obligations: vec![],
        unknowns: vec![],
    })
}

fn check_request(
    profile: &Profile,
    choice: &WorkspaceProfileInput,
    config: &Configuration,
    request_id: &str,
    cases: &BTreeSet<String>,
) -> Result<(Request, rovik_harness::registry::CommandDefinition)> {
    let requests = BTreeSet::from([request_id.to_string()]);
    let (operation, payload, command) = match profile.language {
        Language::Rust | Language::C | Language::Cpp => {
            ensure(
                config.runtime == "linux-native",
                "native wizard needs the registered Linux runtime",
            )?;
            ensure(
                choice.typescript_project.is_none(),
                "native profile cannot select a TypeScript project",
            )?;
            if profile.language == Language::Rust {
                ensure(
                    profile.facts.get("std_or_no_std").map(String::as_str) == Some("std"),
                    "no_std needs a separately registered target runner; the workspace wizard supports std",
                )?;
                ensure(
                    profile.facts.get("execution_target") == Some(&config.target),
                    "Rust execution_target must match the selected configuration target",
                )?;
            } else {
                let standard = if profile.language == Language::C {
                    "17"
                } else {
                    "23"
                };
                ensure(
                    profile.facts.get("language_standard").map(String::as_str) == Some(standard)
                        && config.target == "x86_64-linux-gnu",
                    "the prepared C/C++ workspace adapter requires GCC 12, C17 or C++23 and x86_64-linux-gnu",
                )?;
            }
            let check = if profile.language == Language::Rust {
                NativeCheck::CargoTests
            } else {
                NativeCheck::Ctest
            };
            let input = NativeCheckInput {
                native: NativeInput {
                    snapshot_handle: "snapshot:active".into(),
                    profile: profile.clone(),
                    configuration_id: config.id.clone(),
                    forbidden_edges: vec![],
                    architecture: None,
                    required_test_ids: cases.clone(),
                    typescript_project: None,
                },
                check,
            };
            input.validate()?;
            (
                Operation::NativeCheck,
                serde_json::to_value(input).map_err(|e| Error::new(e.to_string()))?,
                packs::workspace_command(check, requests)?,
            )
        }
        Language::JavaScript | Language::TypeScript | Language::Python => {
            let input = TestInput {
                snapshot_handle: "snapshot:active".into(),
                profile: profile.clone(),
                configuration_id: config.id.clone(),
                test_files: choice.protected_test_paths.clone(),
                typescript_project: choice.typescript_project.clone(),
                required_test_ids: cases.clone(),
            };
            input.validate()?;
            (
                Operation::Test,
                serde_json::to_value(input).map_err(|e| Error::new(e.to_string()))?,
                packs::runtime_test_command(requests)?,
            )
        }
    };
    Ok((
        Request {
            protocol_version: 1,
            request_id: request_id.into(),
            operation,
            payload,
        },
        command,
    ))
}

// TypeScript resolves source selection relative to its protected compiler project.
// Rebase only the supported single-project form; a partial extends/reference
// resolver would silently change compiler semantics across an approval boundary.
fn rebase_typescript_project(
    ledger: &Ledger,
    baseline: &Snapshot,
    profile: &Profile,
    path: &str,
) -> Result<Vec<u8>> {
    let entry = baseline
        .files
        .get(path)
        .ok_or_else(|| Error::new("selected TypeScript project is absent from baseline".into()))?;
    let mut value: serde_json::Value =
        wire::decode(&ledger.read(&entry.content.digest)?).map_err(|error| {
            Error::new(format!(
                "TypeScript preparation needs a strict JSON compiler project: {error}"
            ))
        })?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::new("TypeScript compiler project must be an object".into()))?;
    ensure(!object.contains_key("extends") && !object.contains_key("references"),
        "TypeScript extends/project references need a separately prepared single-project compiler config")?;
    let directory = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    let directory = directory
        .strip_prefix(&profile.build_root)
        .map_err(|_| {
            Error::new("TypeScript compiler project must be inside its selected build root".into())
        })?
        .to_str()
        .ok_or_else(|| Error::new("TypeScript compiler project path must be UTF-8".into()))?;
    if !object.contains_key("files") && !object.contains_key("include") {
        object.insert("include".into(), serde_json::json!(["**/*"]));
    }
    if !object.contains_key("exclude") {
        object.insert(
            "exclude".into(),
            serde_json::json!([
                "node_modules",
                "bower_components",
                "jspm_packages",
                "/build/project/dist"
            ]),
        );
    }
    for key in ["files", "include", "exclude"] {
        if let Some(paths) = object.get_mut(key) {
            rebase_paths(paths, directory)?;
        }
    }
    let options = object
        .entry("compilerOptions")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::new("TypeScript compilerOptions must be an object".into()))?;
    for unsupported in ["outFile", "declarationDir", "tsBuildInfoFile"] {
        ensure(
            !options.contains_key(unsupported),
            format!("TypeScript {unsupported} needs a separate prepared compiler adapter"),
        )?;
    }
    ensure(
        !options.get("allowImportingTsExtensions").is_some_and(|value| value != &serde_json::Value::Bool(false)),
        "TypeScript import-extension rewriting needs a separately prepared emission-compatible compiler config",
    )?;
    let base = if let Some(base) = options.get_mut("baseUrl") {
        let rebased = candidate_path(
            base.as_str()
                .ok_or_else(|| Error::new("TypeScript baseUrl must be a path string".into()))?,
            directory,
        )?;
        *base = rebased.clone().into();
        rebased
            .strip_prefix("/build/project/")
            .unwrap_or("")
            .to_string()
    } else {
        directory.to_string()
    };
    for key in ["rootDirs", "typeRoots"] {
        if let Some(paths) = options.get_mut(key) {
            rebase_paths(paths, directory)?;
        }
    }
    if let Some(paths) = options.get_mut("paths") {
        for paths in paths
            .as_object_mut()
            .ok_or_else(|| Error::new("TypeScript paths must be an object".into()))?
            .values_mut()
        {
            rebase_paths(paths, &base)?;
        }
    }
    // These are the existing runtime adapter's mandatory command-line values.
    // Persisting them here makes the protected compiler selection reviewable.
    options.insert("rootDir".into(), "/build/project".into());
    options.insert("outDir".into(), "/build/project/dist".into());
    for (key, value) in [
        ("strict", true),
        ("noEmitOnError", true),
        ("noCheck", false),
        ("noEmit", false),
        ("emitDeclarationOnly", false),
        ("incremental", false),
        ("composite", false),
    ] {
        options.insert(key.into(), value.into());
    }
    wire::canonical(&value)
}

fn rebase_paths(value: &mut serde_json::Value, directory: &str) -> Result<()> {
    for path in value
        .as_array_mut()
        .ok_or_else(|| Error::new("TypeScript source/path selection must be an array".into()))?
    {
        *path = candidate_path(
            path.as_str().ok_or_else(|| {
                Error::new("TypeScript source/path selection must contain strings".into())
            })?,
            directory,
        )?
        .into();
    }
    Ok(())
}

fn candidate_path(value: &str, directory: &str) -> Result<String> {
    ensure(
        !value.is_empty()
            && !value.contains(['\\', ':'])
            && !value.contains("${")
            && !value.chars().any(char::is_control),
        "TypeScript path must be a portable literal path or source glob",
    )?;
    let (base, relative) = if value == "/build/project" {
        ("", "")
    } else if let Some(relative) = value.strip_prefix("/build/project/") {
        ("", relative)
    } else {
        ensure(
            !value.starts_with('/'),
            "TypeScript source paths must stay inside /build/project",
        )?;
        (directory, value)
    };
    let mut parts: Vec<_> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure(
                    parts.pop().is_some(),
                    "TypeScript path escapes the selected candidate build root",
                )?;
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        Ok("/build/project".into())
    } else {
        Ok(format!("/build/project/{}", parts.join("/")))
    }
}

fn revision_snapshot(root: &Path, ledger: &Ledger, revision: &str) -> Result<Snapshot> {
    ensure(
        !revision.trim().is_empty(),
        "select a real known-failing Git revision",
    )?;
    let repository = git2::Repository::open(root).map_err(|e| Error::new(e.to_string()))?;
    let commit = repository
        .revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map_err(|e| Error::new(format!("cannot resolve negative control revision: {e}")))?;
    let tree = commit.tree().map_err(|e| Error::new(e.to_string()))?;
    GitIntegration::tree_snapshot(&repository, &tree, ledger, &commit.id().to_string())
}

fn selected_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure(
        path.is_absolute(),
        "select an absolute host path for prepared assets",
    )?;
    let metadata = fs::symlink_metadata(path)?;
    ensure(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit,
        "prepared asset must be a bounded regular file, not a symlink",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure(
            metadata.nlink() == 1,
            "prepared assets cannot be hard-linked outside the selected bundle",
        )?;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= limit,
        "prepared asset exceeds the file size limit",
    )?;
    Ok(bytes)
}

fn dependency_snapshot(ledger: &Ledger, root: &Path, now: u64) -> Result<Snapshot> {
    ensure(
        root.is_absolute(),
        "select an absolute prepared dependency directory",
    )?;
    let metadata = fs::symlink_metadata(root)?;
    ensure(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "dependency directory cannot be a symlink",
    )?;
    let root = fs::canonicalize(root)?;
    let mut files = BTreeMap::new();
    let mut pending = vec![root.clone()];
    let mut bytes: u64 = 0;
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            ensure(
                entries <= 32_768,
                "prepared dependency directory exceeds the entry limit",
            )?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure(
                !metadata.file_type().is_symlink(),
                "dependency symlinks require a separate prepared bundle adapter",
            )?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| Error::new("dependency path escaped selected directory".into()))?
                .to_str()
                .ok_or_else(|| Error::new("dependency path must be UTF-8".into()))?
                .replace('\\', "/");
            rovik_harness::scope::validate_path(&relative)?;
            let content = selected_file(&path, 64 * 1024 * 1024)?;
            bytes = bytes
                .checked_add(content.len() as u64)
                .ok_or_else(|| Error::new("dependency size overflow".into()))?;
            ensure(
                bytes <= 512 * 1024 * 1024,
                "prepared dependency bundle exceeds 512 MiB",
            )?;
            let content = ledger.store("source", "application/octet-stream", &content, now)?;
            files.insert(
                relative,
                FileEntry {
                    mode: 0o100644,
                    content,
                },
            );
        }
    }
    let snapshot = Snapshot {
        schema_version: 1,
        base_revision: "prepared-dependencies".into(),
        files,
        deletions: vec![],
        dependency_assets: vec![],
    };
    snapshot.validate(ledger, true)?;
    Ok(snapshot)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Ledger, Snapshot, WorkspaceSetupInput) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let repository = git2::Repository::init(&project).unwrap();
        let sources = [
            ("backend/Cargo.toml", "[package]\nname='example'\nversion='0.1.0'\nedition='2021'\n"),
            ("backend/src/lib.rs", "pub fn classify(value: i32) -> bool { value > 0 }\n"),
            ("backend/tests/behavior.rs", "#[test] fn boundary() { assert!(example::classify(0)); }\n"),
            ("package.json", "{\"type\":\"module\",\"devDependencies\":{\"typescript\":\"5.7.3\"}}"),
            ("package-lock.json", "{\"lockfileVersion\":3,\"packages\":{}}"),
            ("src/value.ts", "export function classify(value: number): boolean { return value > 0; }\n"),
            ("tsconfig.json", "{\"compilerOptions\":{\"module\":\"NodeNext\",\"target\":\"ES2022\"},\"include\":[\"src/**/*.ts\"]}"),
            ("tests/behavior.mjs", "import {test} from 'node:test'; import assert from 'node:assert/strict'; import {classify} from '/build/project/dist/src/value.js'; test('frontend-boundary', () => assert.equal(classify(0), true));"),
        ];
        let mut index = repository.index().unwrap();
        for (path, content) in sources {
            fs::create_dir_all(project.join(path).parent().unwrap()).unwrap();
            fs::write(project.join(path), content).unwrap();
            index.add_path(Path::new(path)).unwrap();
        }
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let author = git2::Signature::now("Wizard test", "wizard@example.invalid").unwrap();
        let revision = repository
            .commit(
                Some("HEAD"),
                &author,
                &author,
                "failing baseline",
                &tree,
                &[],
            )
            .unwrap();
        let ledger = Ledger::open(&root.join("state")).unwrap();
        let baseline = GitIntegration::new("project".into(), &project)
            .unwrap()
            .snapshot(&ledger)
            .unwrap();
        fs::write(
            root.join("toolchain.txt"),
            "rustc 1.85.1; node 22.14.0; npm 10.9.2; typescript 5.7.3",
        )
        .unwrap();
        fs::create_dir(root.join("dependencies")).unwrap();
        let native_case = "example@0.1.0/test/behavior::boundary";
        let input: WorkspaceSetupInput = serde_json::from_value(serde_json::json!({
            "name":"boundary-behavior", "intent":"Treat the boundary as included in both public APIs", "boundedContext":"classification",
            "writePaths":["backend/src/**", "src/**"], "protectedPaths":[],
            "profiles":[
                {"id":"backend", "manifest":"backend/Cargo.toml", "language":"rust",
                 "facts":{"edition":"2021", "minimum_supported_rust_version":"1.85.1", "execution_target":"x86_64-unknown-linux-gnu", "std_or_no_std":"std", "allocation_policy":"standard allocation", "panic_policy":"unwind", "unsafe_scope":"forbidden"},
                 "configurations":[{"id":"tests", "target":"x86_64-unknown-linux-gnu", "features":[], "default_features":true, "build_targets":["workspace-tests"], "flags":[], "runtime":"linux-native"}],
                 "protectedTestPaths":["backend/tests/behavior.rs"]},
                {"id":"frontend", "manifest":"package.json", "language":"typescript",
                 "facts":{"node_version":"22.14.0", "npm_version":"10.9.2", "typescript_version":"5.7.3", "module_system":"esm", "test_runner":"node:test", "dependency_policy":"npm-ci-offline-no-scripts"},
                 "configurations":[{"id":"tests", "target":"linux-x86_64", "features":[], "default_features":false, "build_targets":["protected-tests"], "flags":["--strict", "--noEmitOnError"], "runtime":"node"}],
                 "protectedTestPaths":["tests/behavior.mjs"], "typescriptProject":"tsconfig.json"}
            ],
            "requirements":[{"id":"BOUNDARY-001", "statement":"Include zero", "owner":"classification", "casesByConfiguration":{"backend/tests":[native_case], "frontend/tests":["frontend-boundary"]}}],
            "design":{"components":{"classification":"Own inclusion rules across public bindings"}, "existingCode":[{"file":"backend/src/lib.rs", "symbol":"classify", "rationale":"Existing public predicate owns the boundary comparison"}], "publicApiChanges":["Preserve signatures"], "resourceContracts":["No retained resources"], "errorContracts":["No new errors"], "concurrencyContract":"Pure predicates are independently callable", "mechanismPressure":"Keep a direct predicate because no independent variation is needed", "mechanismAlternatives":["A strategy introduces indirection without a second policy"], "mechanismCost":"No additional state or dispatch"},
            "environment":{"imageDigest":Digest::of(b"prepared-image"), "engine":"podman", "toolchainManifestPath":root.join("toolchain.txt"), "dependencyDirectory":root.join("dependencies")},
            "negativeControlRevision":revision.to_string(), "negativeControlCases":[native_case],
            "baselineFailures":[{"profileId":"backend", "configurationId":"tests", "caseIds":[native_case], "rationale":"Boundary defect being repaired"}, {"profileId":"frontend", "configurationId":"tests", "caseIds":["frontend-boundary"], "rationale":"Same public boundary defect"}],
            "worker":{"model_id":"reviewed-provider-model", "context_bytes":131072, "output_tokens":8192, "timeout_ms":180000, "token_reservation":16384, "cost_reservation":100000, "format_repairs":1},
            "budgets":{"model_calls":16, "tokens":262144, "cost_microunits":1000000, "checks":128}, "durationMs":3600000
        })).unwrap();
        (directory, ledger, baseline, input)
    }

    #[test]
    fn mixed_workspace_setup_derives_real_protected_requests_without_approval_or_execution() {
        let (directory, ledger, baseline, input) = fixture();
        let setup = build(
            &directory.path().join("project"),
            &ledger,
            "project",
            &baseline,
            &input,
        )
        .unwrap();
        setup.validate(&ledger).unwrap();
        assert_eq!(setup.profiles.len(), 2);
        assert_eq!(setup.plan.required_final().count(), 2);
        assert_eq!(setup.contract.invariants[0].cases_by_configuration.len(), 2);
        assert_eq!(
            setup.environment.backend_version,
            runner::PODMAN_BACKEND_VERSION
        );
        assert!(
            ledger.run(&setup.run_id).is_err(),
            "preparation cannot mint run authority"
        );
        for check in setup.plan.required_final() {
            let request_id = &check.validated_parameters["request_id"];
            let entry = &setup.oracle.files[&format!("requests/{request_id}.json")];
            let request: Request =
                wire::decode(&ledger.read(&entry.content.digest).unwrap()).unwrap();
            if request.operation == Operation::NativeCheck {
                let native = rovik_harness::native_checks::input(&request).unwrap();
                assert_eq!(native.check, NativeCheck::CargoTests);
                assert_eq!(
                    check.expected_case_ids,
                    ["example@0.1.0/test/behavior::boundary"]
                );
            } else {
                let runtime = rovik_harness::runtime::input(&request).unwrap();
                assert_eq!(runtime.profile.language, Language::TypeScript);
                assert_eq!(check.expected_case_ids, ["frontend-boundary"]);
                assert_eq!(runtime.typescript_project.as_deref(), Some("tsconfig.json"));
            }
        }
        for protected in [
            "backend/tests/behavior.rs",
            "backend/Cargo.toml",
            "tests/behavior.mjs",
            "tsconfig.json",
        ] {
            assert!(setup.policy.check_write(protected).is_err());
        }
        assert!(setup.policy.check_write("backend/src/lib.rs").is_ok());
        assert_eq!(setup.negative_control, baseline);
        let compiler: serde_json::Value = wire::decode(
            &ledger
                .read(&setup.oracle.files["tsconfig.json"].content.digest)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            compiler["include"],
            serde_json::json!(["/build/project/src/**/*.ts"])
        );
        assert_eq!(compiler["compilerOptions"]["rootDir"], "/build/project");
        assert_eq!(compiler["compilerOptions"]["outDir"], "/build/project/dist");
        assert_ne!(
            setup.oracle.files["tsconfig.json"].content.digest,
            baseline.files["tsconfig.json"].content.digest
        );
        let public_api = rovik_harness::risk::Assessment {
            risks: BTreeSet::from(["public-api".into()]),
            sources: BTreeMap::new(),
        };
        setup
            .risk_policy
            .validate(&public_api, &setup.plan, &setup.registry)
            .unwrap();
        assert_eq!(
            setup.policy.facts["risk_policy_digest"],
            Digest::of_json(&setup.risk_policy).unwrap().to_string()
        );
        assert_eq!(
            super::super::ide_setup::summarize(&setup)
                .unwrap()
                .risk_policy
                .routes
                .len(),
            1
        );
        let unsafe_change = rovik_harness::risk::Assessment {
            risks: BTreeSet::from(["unsafe".into()]),
            sources: BTreeMap::new(),
        };
        assert!(setup
            .risk_policy
            .validate(&unsafe_change, &setup.plan, &setup.registry)
            .is_err());
    }

    #[test]
    fn wizard_rejects_missing_semantics_cases_profiles_and_unprepared_assets() {
        let (directory, ledger, baseline, input) = fixture();
        let project = directory.path().join("project");
        for failure in [
            "case-mapping",
            "profile",
            "symbol",
            "test-path",
            "version",
            "control",
            "toolchain",
            "cross-check-control",
        ] {
            let mut invalid = input.clone();
            match failure {
                "case-mapping" => {
                    invalid.requirements[0]
                        .cases_by_configuration
                        .remove("frontend/tests");
                }
                "profile" => {
                    invalid.profiles.pop();
                }
                "symbol" => {
                    invalid.design.existing_code[0].symbol = "invented_symbol".into();
                }
                "test-path" => {
                    invalid.profiles[0].protected_test_paths = vec!["new/uncommitted.rs".into()];
                }
                "version" => {
                    invalid.profiles[1]
                        .facts
                        .insert("node_version".into(), "latest".into());
                }
                "control" => {
                    invalid.negative_control_revision = "missing-revision".into();
                }
                "toolchain" => {
                    invalid.environment.toolchain_manifest_path = "relative/toolchain.txt".into();
                }
                "cross-check-control" => {
                    invalid
                        .negative_control_cases
                        .push("frontend-boundary".into());
                }
                _ => unreachable!(),
            }
            assert!(
                build(&project, &ledger, "project", &baseline, &invalid).is_err(),
                "{failure}"
            );
        }
    }

    #[test]
    fn native_control_cannot_pass_off_changed_test_bytes_as_implementation_sensitivity() {
        let (directory, ledger, baseline, mut input) = fixture();
        let root = directory.path().join("project");
        let repository = git2::Repository::open(&root).unwrap();
        fs::write(
            root.join("backend/tests/behavior.rs"),
            "#[test] fn boundary() { panic!(\"fake control\"); }",
        )
        .unwrap();
        let mut index = repository.index().unwrap();
        index
            .add_path(Path::new("backend/tests/behavior.rs"))
            .unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repository.head().unwrap().peel_to_commit().unwrap();
        let author = git2::Signature::now("Wizard test", "wizard@example.invalid").unwrap();
        let revision = repository
            .commit(
                Some("HEAD"),
                &author,
                &author,
                "changed oracle",
                &tree,
                &[&parent],
            )
            .unwrap();
        input.negative_control_revision = revision.to_string();
        let error = build(&root, &ledger, "project", &baseline, &input).unwrap_err();
        assert!(error
            .message
            .contains("preserve protected native test bytes"));
    }

    #[test]
    fn compiler_rebase_retains_aliases_and_rejects_unresolved_projects_or_escaping_paths() {
        let (_directory, ledger, mut baseline, input) = fixture();
        let profile = Profile {
            schema_version: 1,
            id: "frontend".into(),
            build_root: String::new(),
            language: Language::TypeScript,
            status: Resolution::Resolved,
            toolchain_lock: None,
            dependency_bundle: None,
            facts: input.profiles[1].facts.clone(),
            configurations: input.profiles[1].configurations.clone(),
            public_consumer_commands: vec![],
        };
        let valid = serde_json::json!({"compilerOptions":{"noEmit":true,"strict":false,"baseUrl":".","paths":{"@/*":["src/*"]},"typeRoots":["node_modules/@types"]},"files":["src/value.ts"],"exclude":["tests/**"]});
        let content = ledger
            .store(
                "source",
                "application/json",
                &wire::canonical(&valid).unwrap(),
                runner::now_ms().unwrap(),
            )
            .unwrap();
        baseline.files.get_mut("tsconfig.json").unwrap().content = content;
        let rebased: serde_json::Value = wire::decode(
            &rebase_typescript_project(&ledger, &baseline, &profile, "tsconfig.json").unwrap(),
        )
        .unwrap();
        assert_eq!(
            rebased["files"],
            serde_json::json!(["/build/project/src/value.ts"])
        );
        assert_eq!(
            rebased["exclude"],
            serde_json::json!(["/build/project/tests/**"])
        );
        assert_eq!(
            rebased["compilerOptions"]["paths"]["@/*"],
            serde_json::json!(["/build/project/src/*"])
        );
        assert_eq!(
            rebased["compilerOptions"]["typeRoots"],
            serde_json::json!(["/build/project/node_modules/@types"])
        );
        assert_eq!(rebased["compilerOptions"]["noEmit"], false);
        assert_eq!(rebased["compilerOptions"]["strict"], true);
        for invalid in [
            serde_json::json!({"extends":"./tsconfig.base.json"}),
            serde_json::json!({"references":[{"path":"./backend"}]}),
            serde_json::json!({"compilerOptions":{"declarationDir":"../exports"}}),
            serde_json::json!({"compilerOptions":{"allowImportingTsExtensions":true}}),
            serde_json::json!({"include":["../outside/**/*.ts"]}),
            serde_json::json!({"include":["/oracle/frozen-source.ts"]}),
            serde_json::json!({"compilerOptions":{"paths":{"@/*":["/etc/*"]}}}),
        ] {
            baseline.files.get_mut("tsconfig.json").unwrap().content = ledger
                .store(
                    "source",
                    "application/json",
                    &wire::canonical(&invalid).unwrap(),
                    runner::now_ms().unwrap(),
                )
                .unwrap();
            assert!(
                rebase_typescript_project(&ledger, &baseline, &profile, "tsconfig.json").is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn selected_prepared_assets_reject_symlink_and_hardlink_aliases() {
        let (directory, ledger, baseline, input) = fixture();
        std::os::unix::fs::symlink(
            directory.path().join("toolchain.txt"),
            directory.path().join("dependencies/link"),
        )
        .unwrap();
        let error = build(
            &directory.path().join("project"),
            &ledger,
            "project",
            &baseline,
            &input,
        )
        .unwrap_err();
        assert!(error.message.contains("dependency symlinks"));
        fs::remove_file(directory.path().join("dependencies/link")).unwrap();
        fs::hard_link(
            directory.path().join("toolchain.txt"),
            directory.path().join("dependencies/linked-file"),
        )
        .unwrap();
        let error = build(
            &directory.path().join("project"),
            &ledger,
            "project",
            &baseline,
            &input,
        )
        .unwrap_err();
        assert!(error.message.contains("hard-linked"));
    }
}
