//! Durable application workflow. All state advancement is based on trusted work.

use crate::{
    approvals::AuthorityPolicy,
    broker::Broker,
    context::{self, ContextItem, Trust},
    contract::{ContractRegistry, DesignRecord, TaskContract},
    evidence::{self, GateDecision, Status, TrustedPreconditions, Verdict},
    identity::{Digest, EvidenceKey},
    ledger::{Ledger, RunRecord},
    plan::{ApprovalRequirement, CheckKind, CheckPlan, CheckSpec, Phase, PlanRegistry},
    policy::EffectivePolicy,
    profile::Profile,
    provider::{ModelRequest, Provider, WorkerTurn, WORKER_CONTROL},
    registry::CommandRegistry,
    require,
    runner::{self, Cancellation, ContainerBackend, Environment, ExecutionInput},
    snapshot::{AdmissionRules, Snapshot},
    state::State,
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Explicit model and context limits, resolved by the operator before a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLimits {
    /// Registered model identity.
    pub model_id: String,
    /// Maximum complete serialized context, including tool replies.
    pub context_bytes: usize,
    /// Maximum provider output tokens.
    pub output_tokens: u64,
    /// Maximum duration of one model attempt.
    pub timeout_ms: u64,
    /// Reservation for total input plus output tokens.
    pub token_reservation: u64,
    /// Maximum expected cost for one model attempt, in configured microunits.
    pub cost_reservation: u64,
    /// At most one schema-format repair per request sequence.
    pub format_repairs: u32,
    /// Bounded retries of completed infrastructure failures for one exact check binding.
    #[serde(default = "default_infrastructure_retries")]
    pub infrastructure_retries_per_check: u32,
}
impl WorkerLimits {
    /// Reject unresolved identities, unbounded context/output and excessive automatic retries.
    pub fn validate(&self) -> Result<()> {
        require(
            !self.model_id.trim().is_empty() && self.model_id != "operator-must-select-model",
            "select an explicit model identity",
        )?;
        require(
            self.context_bytes > 0
                && self.context_bytes <= wire::MAX_JSON_BYTES
                && self.output_tokens > 0
                && self.timeout_ms > 0
                && self.timeout_ms <= 86_400_000
                && self.format_repairs <= 1
                && self.infrastructure_retries_per_check <= 3,
            "unresolved worker limits",
        )?;
        require(
            self.token_reservation >= self.output_tokens,
            "token reservation omits requested output",
        )?;
        Ok(())
    }
}
fn default_infrastructure_retries() -> u32 {
    1
}

/// Complete trusted intake. Models receive selected context, never this capability set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSetup {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable run ID.
    pub run_id: String,
    /// Registered repository identity.
    pub repository_id: String,
    /// Approved behavioral contract.
    pub contract: TaskContract,
    /// Target-native responsibility and resource design.
    pub design: DesignRecord,
    /// Complete resolved language matrix.
    pub profiles: Vec<Profile>,
    /// Immutable authorized baseline.
    pub base: Snapshot,
    /// Approved pre-existing failures; all other baseline checks must pass.
    #[serde(default)]
    pub baseline: crate::baseline::BaselinePlan,
    /// Protected expected behavior and driver assets.
    pub oracle: Snapshot,
    /// Immutable prepared dependency bundle.
    pub dependencies: Snapshot,
    /// Trusted command registry.
    pub registry: CommandRegistry,
    /// Pinned execution environment.
    pub environment: Environment,
    /// Restrictively resolved policy.
    pub policy: EffectivePolicy,
    /// Configuration-scoped dispositions, bound into policy and approved independently.
    #[serde(default)]
    pub exceptions: Vec<crate::exceptions::PolicyException>,
    /// Trusted conditional evidence routing; its digest is included in nonempty effective policy facts.
    #[serde(default)]
    pub risk_policy: crate::risk::RiskPolicy,
    /// Authorized non-source changes, if any.
    pub admission: AdmissionRules,
    /// Complete initial verification plan.
    pub plan: CheckPlan,
    /// Protected suite-to-case inventory.
    pub suites: BTreeMap<String, BTreeSet<String>>,
    /// Registered semantic constraint names.
    pub constraint_names: BTreeSet<String>,
    /// Registered risk vocabulary.
    pub risk_names: BTreeSet<String>,
    /// Authenticated approval authority registration.
    pub authority: AuthorityPolicy,
    /// Explicit upper accounts: model_calls, tokens, cost_microunits and checks.
    pub budgets: BTreeMap<String, u64>,
    /// Resolved provider/context limits.
    pub worker: WorkerLimits,
    /// Optional independently prompted model review, charged to the same run budget.
    #[serde(default)]
    pub reviewer: Option<WorkerLimits>,
    /// Trusted relevant mutant or failing baseline used to validate the oracle.
    pub negative_control: Snapshot,
    /// Cases that must fail against that control.
    pub negative_control_cases: Vec<String>,
    /// Absolute overall execution deadline in UTC milliseconds.
    pub deadline_ms: u64,
}

impl TaskSetup {
    /// Build the trusted plan registry from every declared profile and obligation.
    pub fn plan_registry(&self) -> Result<PlanRegistry> {
        let obligations: BTreeSet<_> = self
            .policy
            .obligations
            .iter()
            .chain(&self.design.verification_obligations)
            .cloned()
            .chain(self.contract.invariants.iter().map(|i| i.id.clone()))
            .collect();
        let configurations = self
            .profiles
            .iter()
            .flat_map(|p| &p.configurations)
            .map(|c| Ok((c.digest()?, obligations.clone())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let commands = self
            .registry
            .commands
            .iter()
            .map(|c| (c.id.clone(), (c.kind, c.checker_digest.clone())))
            .collect();
        let mut authorized_dispositions = BTreeSet::new();
        let mut disposition_configurations = BTreeMap::new();
        crate::unique(
            self.exceptions
                .iter()
                .map(|exception| exception.exception_id.as_str()),
        )?;
        for exception in &self.exceptions {
            let digest = Digest::of_json(exception)?;
            let dispositions = match exception.kind {
                crate::exceptions::DispositionKind::Exception => &self.plan.approved_exceptions,
                crate::exceptions::DispositionKind::NotApplicable => {
                    &self.plan.non_applicable_obligations
                }
            };
            require(
                dispositions.iter().any(|disposition| {
                    disposition.evidence_digest == digest
                        && disposition.obligation_id == exception.obligation_id
                        && exception
                            .approval()
                            .is_ok_and(|approval| approval == disposition.approval)
                }),
                "registered exception must match its exact plan disposition and approval",
            )?;
            authorized_dispositions.insert(digest.clone());
            disposition_configurations.insert(digest, exception.configurations.clone());
        }
        Ok(PlanRegistry {
            commands,
            configurations,
            authorized_dispositions,
            disposition_configurations,
            non_waivable: ["TRUST-001".into(), "SPEC-001".into()].into(),
        })
    }
    /// Validate shape, references and all mandatory coverage before model execution.
    pub fn validate(&self, ledger: &Ledger) -> Result<()> {
        require(self.schema_version == 1, "unsupported task setup version")?;
        crate::id(&self.run_id)?;
        crate::id(&self.repository_id)?;
        let registry = ContractRegistry {
            profiles: self.profiles.iter().map(|p| p.id.clone()).collect(),
            suites: self.suites.clone(),
            constraints: self.constraint_names.clone(),
            risks: self.risk_names.clone(),
        };
        self.contract.validate(&registry)?;
        self.contract.require_resolved()?;
        self.design.validate(&self.contract)?;
        crate::design::validate(ledger, &self.base, &self.design)?;
        require(
            self.contract
                .profile_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                == registry.profiles,
            "setup has missing or extraneous profiles",
        )?;
        crate::unique(self.profiles.iter().map(|p| p.id.as_str()))?;
        for profile in &self.profiles {
            profile.require_resolved()?;
            require(
                profile.toolchain_lock.as_ref() == Some(&self.environment.toolchain_digest)
                    && profile.dependency_bundle.as_ref()
                        == Some(&self.environment.dependency_digest),
                "profile environment does not match execution pins",
            )?;
        }
        if !self.risk_policy.routes.is_empty()
            || self.policy.facts.contains_key("risk_policy_digest")
        {
            require(
                self.policy.facts.get("risk_policy_digest")
                    == Some(&Digest::of_json(&self.risk_policy)?.to_string()),
                "risk routing must be bound into the approved effective policy",
            )?;
        }
        self.registry.validate()?;
        self.environment.validate(&self.registry)?;
        require(
            self.policy.scopes.contains(&self.contract.scope),
            "effective policy omits the approved contract scope",
        )?;
        require(
            self.policy
                .commands
                .iter()
                .all(|c| self.registry.get(c).is_ok()),
            "policy names an unregistered command",
        )?;
        for snapshot in [
            &self.base,
            &self.oracle,
            &self.dependencies,
            &self.negative_control,
        ] {
            snapshot.validate(ledger, true)?;
        }
        require(
            self.dependencies.digest()? == self.environment.dependency_digest,
            "dependency manifest differs from environment",
        )?;
        ledger.read(&self.environment.toolchain_digest)?;
        require(
            self.plan.run_id == self.run_id
                && self.plan.contract_digest == Digest::of_json(&self.contract)?
                && self.plan.design_digest == Digest::of_json(&self.design)?
                && self.plan.policy_digest == Digest::of_json(&self.policy)?
                && self.plan.oracle_digest == self.oracle.digest()?,
            "plan is bound to different task inputs",
        )?;
        if !self.exceptions.is_empty() || self.policy.facts.contains_key("exceptions_digest") {
            require(
                self.policy.facts.get("exceptions_digest")
                    == Some(&Digest::of_json(&self.exceptions)?.to_string()),
                "exceptions must be bound into effective policy",
            )?;
        }
        for exception in &self.exceptions {
            exception.validate(
                &self.contract.scope,
                &self.plan.configurations.iter().cloned().collect(),
                ledger,
                runner::now_ms()?,
            )?;
        }
        self.plan.validate(&self.plan_registry()?)?;
        self.baseline.validate(self)?;
        for profile in &self.profiles {
            for config in &profile.configurations {
                let configuration_digest = config.digest()?;
                for command in &profile.public_consumer_commands {
                    require(
                        self.registry.get(command)?.kind == CheckKind::Behavioral,
                        "public consumer must report native behavioral cases",
                    )?;
                    require(
                        self.plan.required_final().any(|c| {
                            c.command_id == *command
                                && c.configuration_digest == configuration_digest
                        }),
                        "final matrix omits a required public consumer",
                    )?;
                }
            }
        }
        for check in &self.plan.checks {
            let definition = self.registry.get(&check.command_id)?;
            require(
                check
                    .success_exit_codes
                    .iter()
                    .all(|code| !definition.implementation_failure_exit_codes.contains(code)),
                "successful exit codes overlap protected compilation failures",
            )?;
            definition.render(&check.validated_parameters)?;
            if matches!(
                definition.parser,
                crate::registry::Parser::PackProtocol
                    | crate::registry::Parser::PackTests
                    | crate::registry::Parser::PackBuild
                    | crate::registry::Parser::PackNativeTests
            ) {
                let request_id = check
                    .validated_parameters
                    .get("request_id")
                    .ok_or_else(|| Error::new("pack request identity missing".into()))?;
                crate::id(request_id)?;
                let entry = self
                    .oracle
                    .files
                    .get(&format!("requests/{request_id}.json"))
                    .ok_or_else(|| {
                        Error::new("pack request must be a protected oracle artifact".into())
                    })?;
                let request: crate::pack_protocol::Request =
                    wire::decode(&ledger.read(&entry.content.digest)?)?;
                let (profile, configuration_id) = if definition.parser
                    == crate::registry::Parser::PackTests
                {
                    let input = crate::runtime::input(&request)?;
                    require(
                        input
                            .test_files
                            .iter()
                            .chain(input.typescript_project.iter())
                            .all(|path| self.oracle.files.contains_key(path)),
                        "runtime tests and compiler project must be protected oracle assets",
                    )?;
                    require(
                        check
                            .expected_case_ids
                            .iter()
                            .all(|id| input.required_test_ids.contains(id)),
                        "runtime request omits approved cases",
                    )?;
                    (input.profile, input.configuration_id)
                } else if matches!(
                    definition.parser,
                    crate::registry::Parser::PackBuild | crate::registry::Parser::PackNativeTests
                ) {
                    let input = crate::native_checks::input(&request)?;
                    require(
                        input.check.behavioral()
                            == (definition.parser == crate::registry::Parser::PackNativeTests),
                        "native workspace operation has the wrong registered result kind",
                    )?;
                    require(
                        check
                            .expected_case_ids
                            .iter()
                            .all(|id| input.native.required_test_ids.contains(id)),
                        "native workspace request omits approved cases",
                    )?;
                    (input.native.profile, input.native.configuration_id)
                } else {
                    let input = request.native_input()?;
                    require(
                        request.operation == crate::pack_protocol::Operation::Index,
                        "advisory operation cannot authorize a check",
                    )?;
                    (input.profile, input.configuration_id)
                };
                require(
                    request.request_id == *request_id
                        && self.profiles.contains(&profile)
                        && profile.configurations.iter().any(|c| {
                            c.id == configuration_id
                                && c.digest().ok().as_ref() == Some(&check.configuration_digest)
                        }),
                    "pack request does not match the approved profile matrix",
                )?;
            }
            require(
                self.policy.commands.contains(&check.command_id),
                "plan command denied by effective policy",
            )?;
        }
        let cases: BTreeSet<_> = self
            .contract
            .invariants
            .iter()
            .flat_map(|i| i.case_ids.iter().cloned())
            .collect();
        for configuration in &self.plan.configurations {
            let covered: BTreeSet<_> = self
                .plan
                .required_final()
                .filter(|c| &c.configuration_digest == configuration)
                .flat_map(|c| c.expected_case_ids.iter().cloned())
                .collect();
            require(
                cases.is_subset(&covered),
                "final matrix omits contract acceptance cases",
            )?;
        }
        require(
            !self.negative_control_cases.is_empty()
                && self
                    .negative_control_cases
                    .iter()
                    .all(|c| cases.contains(c)),
            "oracle needs a relevant registered negative control",
        )?;
        self.worker.validate()?;
        if let Some(reviewer) = &self.reviewer {
            reviewer.validate()?;
        }
        for unit in ["model_calls", "tokens", "cost_microunits", "checks"] {
            require(
                self.budgets.contains_key(unit),
                format!("unresolved budget: {unit}"),
            )?;
        }
        for (unit, limit) in &self.policy.limits {
            require(
                self.budgets.get(unit).is_some_and(|value| value <= limit),
                format!("unresolved or exceeded policy budget: {unit}"),
            )?;
        }
        Ok(())
    }
    /// All current subject requirements, including reviews bound to a selected candidate.
    pub fn approval_requirements(
        &self,
        candidate: Option<&Snapshot>,
    ) -> Result<Vec<ApprovalRequirement>> {
        let mut requirements = self.mandatory_approvals()?;
        requirements.extend(self.plan.approval_requirements.iter().cloned());
        if let Some(candidate) = candidate {
            for kind in &self.plan.review_requirements {
                requirements.push(ApprovalRequirement {
                    subject_digest: candidate.digest()?,
                    approval_type: kind.clone(),
                    obligation_ids: vec![],
                });
            }
        }
        Ok(requirements)
    }
    /// Exact approval requirements that cannot be removed by a model plan.
    pub fn mandatory_approvals(&self) -> Result<Vec<ApprovalRequirement>> {
        Ok(vec![
            ApprovalRequirement {
                subject_digest: Digest::of_json(&self.baseline.subject(self)?)?,
                approval_type: "baseline-plan".into(),
                obligation_ids: vec![],
            },
            ApprovalRequirement {
                subject_digest: Digest::of_json(&self.contract)?,
                approval_type: "domain-contract".into(),
                obligation_ids: vec![],
            },
            ApprovalRequirement {
                subject_digest: Digest::of_json(&self.design)?,
                approval_type: "design".into(),
                obligation_ids: vec![],
            },
            ApprovalRequirement {
                subject_digest: self.oracle.digest()?,
                approval_type: "oracle".into(),
                obligation_ids: vec![],
            },
        ])
    }
}

/// Repository integration context is observed independently of model output.
pub trait Integration {
    /// Current authorized destination/base revision for this repository.
    fn current_revision(&self, repository_id: &str) -> Result<String>;
}

/// Durable output and residual obligations, suitable for a CLI or IDE record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    /// Run identity.
    pub run_id: String,
    /// Exact durable workflow state.
    pub state: State,
    /// Current source candidate, when implementation completed.
    pub candidate_digest: Option<Digest>,
    /// Final gate result, when evaluated.
    pub gate: Option<GateDecision>,
    /// Outstanding prerequisite or failure reasons.
    pub reasons: Vec<String>,
}

/// A controller owns all transitions and verifier admission for one ledger.
pub struct Controller {
    /// Durable storage, available to trusted management commands.
    pub ledger: Ledger,
    /// Isolated execution adapter.
    pub backend: ContainerBackend,
}

impl Controller {
    /// Register exact intake artifacts and create the durable run.
    pub fn create(&mut self, setup: &TaskSetup, now: u64) -> Result<RunRecord> {
        let input = self.register_setup(setup, now)?;
        self.ledger
            .create_run(&setup.run_id, &input, &setup.budgets, now)
    }
    pub(crate) fn register_setup(&self, setup: &TaskSetup, now: u64) -> Result<Digest> {
        setup.validate(&self.ledger)?;
        self.backend.require_environment(&setup.environment)?;
        require(setup.deadline_ms > now, "task deadline already exhausted")?;
        for (kind, value) in [
            ("baseline-plan", Ok(setup.baseline.subject(setup)?)),
            ("contract", serde_json::to_value(&setup.contract)),
            ("design", serde_json::to_value(&setup.design)),
            ("policy", serde_json::to_value(&setup.policy)),
            ("plan", serde_json::to_value(&setup.plan)),
            ("environment", serde_json::to_value(&setup.environment)),
            ("snapshot", serde_json::to_value(&setup.base)),
            ("snapshot", serde_json::to_value(&setup.oracle)),
            ("snapshot", serde_json::to_value(&setup.dependencies)),
            ("snapshot", serde_json::to_value(&setup.negative_control)),
            ("policy", serde_json::to_value(&setup.contract.scope)),
        ] {
            self.ledger
                .store_json(kind, &value.map_err(|e| Error::new(e.to_string()))?, now)?;
        }
        for exception in &setup.exceptions {
            self.ledger.store_json("exception", exception, now)?;
        }
        let input = self.ledger.store_json("operation", setup, now)?;
        Ok(input.digest)
    }
    /// Reload immutable intake after restart instead of trusting a conversation cache.
    pub fn setup(&self, run_id: &str) -> Result<TaskSetup> {
        let run = self.ledger.run(run_id)?;
        wire::decode(&self.ledger.read(&run.input_digest)?)
    }
    fn advance(
        &mut self,
        run_id: &str,
        next: State,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let now = runner::now_ms()?;
        let current = self.ledger.run(run_id)?;
        let event = self.ledger.store_json(
            "event",
            &serde_json::json!({"kind":kind,"payload":payload}),
            now,
        )?;
        self.ledger
            .transition(&current, next, &event.digest, None, now)?;
        Ok(())
    }
    fn block(&mut self, run_id: &str, restart: State, reason: &str) -> Result<RunReport> {
        let now = runner::now_ms()?;
        let current = self.ledger.run(run_id)?;
        if !current.state.terminal() && current.state != State::Blocked {
            let event = self.ledger.store_json(
                "event",
                &serde_json::json!({"kind":"blocked","reason":reason,"restart":restart}),
                now,
            )?;
            self.ledger
                .transition(&current, State::Blocked, &event.digest, Some(restart), now)?;
        }
        Ok(RunReport {
            run_id: run_id.into(),
            state: self.ledger.run(run_id)?.state,
            candidate_digest: self
                .current_candidate(run_id)?
                .map(|s| s.digest())
                .transpose()?,
            gate: None,
            reasons: vec![reason.into()],
        })
    }
    fn key(&self, setup: &TaskSetup, candidate: &Snapshot) -> Result<EvidenceKey> {
        Ok(EvidenceKey {
            candidate_digest: candidate.digest()?,
            contract_digest: Digest::of_json(&setup.contract)?,
            design_digest: Digest::of_json(&setup.design)?,
            policy_digest: Digest::of_json(&setup.policy)?,
            oracle_digest: setup.oracle.digest()?,
            plan_digest: Digest::of_json(&setup.plan)?,
            environment_digest: Digest::of_json(&setup.environment)?,
        })
    }
    pub(crate) fn select_binding(
        &mut self,
        setup: &TaskSetup,
        candidate: &Snapshot,
    ) -> Result<EvidenceKey> {
        let now = runner::now_ms()?;
        self.ledger.store_json("snapshot", candidate, now)?;
        let key = self.key(setup, candidate)?;
        let binding = self.ledger.store_json("operation", &key, now)?;
        let current = self.ledger.run(&setup.run_id)?;
        self.ledger.bind(&current, &binding.digest, now)?;
        Ok(key)
    }
    /// Reconstruct the selected source candidate from durable binding artifacts.
    pub fn current_candidate(&self, run_id: &str) -> Result<Option<Snapshot>> {
        let run = self.ledger.run(run_id)?;
        let Some(binding) = run.binding_digest else {
            return Ok(None);
        };
        let key: EvidenceKey = wire::decode(&self.ledger.read(&binding)?)?;
        Ok(Some(wire::decode(
            &self.ledger.read(&key.candidate_digest)?,
        )?))
    }
    pub(crate) fn approvals_current(
        &self,
        setup: &TaskSetup,
        candidate: Option<&Snapshot>,
        now: u64,
    ) -> Result<bool> {
        let scope = Digest::of_json(&setup.contract.scope)?;
        let policy = Digest::of_json(&setup.policy)?;
        let requirements = setup.approval_requirements(candidate)?;
        if setup
            .exceptions
            .iter()
            .any(|exception| exception.expires_ms <= now)
        {
            return Ok(false);
        }
        for requirement in requirements {
            if !self.ledger.approval_current(
                &setup.run_id,
                &requirement,
                &scope,
                &policy,
                &setup.authority,
                now,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(crate) fn check(
        &mut self,
        setup: &TaskSetup,
        candidate: &Snapshot,
        key: &EvidenceKey,
        spec: &CheckSpec,
        kind: &str,
        cancel: &Cancellation,
    ) -> Result<evidence::CheckResult> {
        let now = runner::now_ms()?;
        let input = self.ledger.store_json(
            "operation",
            &ExecutionInput {
                evidence_key: key.clone(),
                check: spec.clone(),
            },
            now,
        )?;
        let base_key = Digest::of_json(&(kind, &input.digest))?.to_string();
        let operations = self.ledger.operations(&setup.run_id)?;
        let attempts: Vec<_> = operations
            .iter()
            .filter(|o| {
                o.operation_key == base_key || o.operation_key.starts_with(&format!("{base_key}:"))
            })
            .collect();
        if let Some(last) = attempts.last() {
            if let Some(result) =
                self.ledger
                    .completed_check(&setup.run_id, &last.operation_key, &input.digest)?
            {
                if !result.infrastructure_retryable()
                    || attempts.len() > setup.worker.infrastructure_retries_per_check as usize
                    || cancel.is_cancelled()
                {
                    return Ok(result);
                }
            } else {
                require(
                    last.state == "failed",
                    "unfinished check requires recovery and actual-usage reconciliation",
                )?;
                require(
                    attempts.len() <= setup.worker.infrastructure_retries_per_check as usize,
                    "infrastructure retry budget exhausted",
                )?;
            }
        }
        let operation_key = if attempts.is_empty() {
            base_key
        } else {
            format!("{base_key}:{}", attempts.len())
        };
        let owner = format!("controller-{}", uuid::Uuid::new_v4());
        let lease = self.ledger.claim(
            &setup.run_id,
            &operation_key,
            kind,
            &input.digest,
            &owner,
            now,
            spec.timeout_ms.saturating_add(720_000),
            &[("checks".into(), 1)].into(),
        )?;
        let observation = self.backend.execute_leased(
            &self.ledger,
            candidate,
            &setup.oracle,
            &setup.dependencies,
            &setup.environment,
            &setup.registry,
            spec,
            cancel,
            &lease,
        )?;
        let result = runner::admit_observation(
            &mut self.ledger,
            &lease,
            key,
            spec,
            setup.registry.get(&spec.command_id)?,
            observation,
            runner::now_ms()?,
        )?;
        let artifact = self
            .ledger
            .store_json("result", &result, runner::now_ms()?)?;
        require(
            self.ledger.complete(
                &lease,
                &artifact.digest,
                &[("checks".into(), 1)].into(),
                runner::now_ms()?,
            )?,
            "verification budget exhausted",
        )?;
        if result.status == Status::Pass {
            for attempt in attempts {
                if let Some(previous) = self.ledger.completed_check(
                    &setup.run_id,
                    &attempt.operation_key,
                    &input.digest,
                )? {
                    if previous.infrastructure_retryable() {
                        self.ledger.record_infrastructure_recovery(
                            &previous,
                            &result,
                            runner::now_ms()?,
                        )?;
                    }
                }
            }
        } else if result.infrastructure_retryable()
            && attempts.len() < setup.worker.infrastructure_retries_per_check as usize
            && !cancel.is_cancelled()
        {
            return self.check(setup, candidate, key, spec, kind, cancel);
        }
        Ok(result)
    }
    fn context(&self, setup: &TaskSetup, candidate: &Snapshot) -> Result<context::ContextBundle> {
        let now = runner::now_ms()?;
        let revision = candidate.digest()?;
        let mut items = Vec::new();
        let control = self
            .ledger
            .store("context", "text/plain", WORKER_CONTROL.as_bytes(), now)?;
        items.push(ContextItem {
            item_id: "control".into(),
            artifact_digest: control.digest,
            trust: Trust::AuthoritativePolicy,
            source_revision: revision.clone(),
            mandatory: true,
            text: WORKER_CONTROL.into(),
        });
        for (label, digest, trust) in [
            (
                "contract",
                Digest::of_json(&setup.contract)?,
                Trust::ApprovedSpec,
            ),
            (
                "design",
                Digest::of_json(&setup.design)?,
                Trust::ApprovedSpec,
            ),
            (
                "policy",
                Digest::of_json(&setup.policy)?,
                Trust::AuthoritativePolicy,
            ),
        ] {
            let bytes = self.ledger.read(&digest)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::new("mandatory context is not UTF-8".into()))?;
            items.push(ContextItem {
                item_id: label.into(),
                artifact_digest: digest,
                trust,
                source_revision: revision.clone(),
                mandatory: true,
                text,
            });
        }
        let profiles = self.ledger.store_json("profile", &setup.profiles, now)?;
        items.push(ContextItem {
            item_id: "profiles".into(),
            text: String::from_utf8(self.ledger.read(&profiles.digest)?)
                .map_err(|_| Error::new("profile context is not UTF-8".into()))?,
            artifact_digest: profiles.digest,
            trust: Trust::AuthoritativePolicy,
            source_revision: revision.clone(),
            mandatory: true,
        });
        let risk = self.ledger.store_json("policy", &setup.risk_policy, now)?;
        items.push(ContextItem {
            item_id: "risk-routing".into(),
            text: String::from_utf8(self.ledger.read(&risk.digest)?)
                .map_err(|_| Error::new("risk policy is not UTF-8".into()))?,
            artifact_digest: risk.digest,
            trust: Trust::AuthoritativePolicy,
            source_revision: revision.clone(),
            mandatory: true,
        });
        let index =
            self.ledger
                .store_json("context", &candidate.files.keys().collect::<Vec<_>>(), now)?;
        items.push(ContextItem {
            item_id: "source-index".into(),
            text: String::from_utf8(self.ledger.read(&index.digest)?)
                .map_err(|_| Error::new("source index is not UTF-8".into()))?,
            artifact_digest: index.digest,
            trust: Trust::ObservedCode,
            source_revision: revision.clone(),
            mandatory: false,
        });
        let mut relevant: BTreeSet<_> = setup
            .design
            .existing_code_considered
            .iter()
            .map(|code| code.file.clone())
            .collect();
        let changes: BTreeSet<_> = setup
            .base
            .files
            .keys()
            .chain(candidate.files.keys())
            .filter(|path| setup.base.files.get(*path) != candidate.files.get(*path))
            .cloned()
            .collect();
        // A prior snapshot's graph is retrieval guidance only. It cannot skip final checks.
        let mut graphed = BTreeSet::new();
        for snapshot in [&setup.base, candidate] {
            if !graphed.insert(snapshot.digest()?) {
                continue;
            }
            for observed in crate::architecture::admitted_graphs(
                &self.ledger,
                &setup.run_id,
                &self.key(setup, snapshot)?,
            )? {
                let affected = crate::architecture::impact(&observed.graph, &changes);
                for node in &observed.graph.nodes {
                    if affected.affected_nodes.contains(&node.id) {
                        if let Some(path) = &node.path {
                            relevant.insert(path.clone());
                        }
                    }
                }
                let artifact = self.ledger.store_json(
                    "context",
                    &serde_json::json!({"graph":observed,"impact":affected}),
                    now,
                )?;
                items.push(ContextItem {
                    item_id: format!("impact:{}", artifact.digest.hex()),
                    artifact_digest: artifact.digest.clone(),
                    text: String::from_utf8(self.ledger.read(&artifact.digest)?)
                        .map_err(|e| Error::new(e.to_string()))?,
                    trust: Trust::ObservedCode,
                    source_revision: snapshot.digest()?,
                    mandatory: false,
                });
            }
        }
        items.extend(context::ranked_sources(
            &self.ledger,
            candidate,
            &setup.contract.intent,
            &relevant.into_iter().collect::<Vec<_>>(),
            Some(&setup.contract.scope),
        )?);
        context::build(
            &self.ledger,
            items,
            setup.worker.context_bytes.saturating_sub(4096),
        )
    }
    fn role_registry(setup: &TaskSetup) -> ContractRegistry {
        ContractRegistry {
            profiles: setup.profiles.iter().map(|p| p.id.clone()).collect(),
            suites: setup.suites.clone(),
            constraints: setup.constraint_names.clone(),
            risks: setup.risk_names.clone(),
        }
    }
    fn role_artifacts(&self, setup: &TaskSetup) -> Result<BTreeMap<String, Digest>> {
        let mut artifacts = BTreeMap::from([
            ("contract".into(), Digest::of_json(&setup.contract)?),
            ("design".into(), Digest::of_json(&setup.design)?),
            ("policy".into(), Digest::of_json(&setup.policy)?),
            ("plan".into(), Digest::of_json(&setup.plan)?),
        ]);
        if let Some(binding) = self.ledger.run(&setup.run_id)?.binding_digest {
            let evidence = self.ledger.store_json(
                "report",
                &self.ledger.completed_results(&setup.run_id, &binding)?,
                runner::now_ms()?,
            )?;
            artifacts.insert("native-evidence".into(), evidence.digest);
        }
        Ok(artifacts)
    }
    fn independent_review_current(&self, setup: &TaskSetup, candidate: &Snapshot) -> Result<bool> {
        let Some(limits) = &setup.reviewer else {
            return Ok(true);
        };
        let registry = Self::role_registry(setup);
        let artifacts = self.role_artifacts(setup)?;
        let input = crate::roles::RoleInput {
            run_id: &setup.run_id,
            role: crate::provider::Role::Reviewer,
            snapshot: candidate,
            registry: &registry,
            artifacts: &artifacts,
            limits,
            deadline_ms: setup.deadline_ms,
        };
        Ok(
            crate::roles::completed(&self.ledger, &input)?.is_some_and(|draft| {
                matches!(
                    draft.artifact,
                    crate::roles::DraftArtifact::Review {
                        recommendation: crate::roles::Recommendation::Ready,
                        ..
                    }
                )
            }),
        )
    }
    fn run_role(
        &mut self,
        setup: &TaskSetup,
        snapshot: &Snapshot,
        role: crate::provider::Role,
        limits: &WorkerLimits,
        provider: &mut dyn Provider,
        cancel: &Cancellation,
    ) -> Result<crate::roles::DraftResponse> {
        let registry = Self::role_registry(setup);
        let artifacts = self.role_artifacts(setup)?;
        let input = crate::roles::RoleInput {
            run_id: &setup.run_id,
            role,
            snapshot,
            registry: &registry,
            artifacts: &artifacts,
            limits,
            deadline_ms: setup.deadline_ms,
        };
        if let Some(previous) = crate::roles::completed(&self.ledger, &input)? {
            return Ok(previous);
        }
        crate::roles::generate(&mut self.ledger, &input, provider, cancel)
    }
    fn implement(
        &mut self,
        setup: &TaskSetup,
        provider: &mut dyn Provider,
        cancel: &Cancellation,
        candidate: Snapshot,
        mut feedback: Vec<serde_json::Value>,
    ) -> Result<Snapshot> {
        let context = self.context(setup, &candidate)?;
        let mut diagnostics: BTreeSet<_> =
            if let Some(binding) = self.ledger.run(&setup.run_id)?.binding_digest {
                self.ledger
                    .completed_results(&setup.run_id, &binding)?
                    .into_iter()
                    .flat_map(|r| r.evidence)
                    .collect()
            } else {
                BTreeSet::new()
            };
        let mut format_repairs = 0;
        loop {
            require(!cancel.is_cancelled(), "worker cancelled")?;
            require(
                runner::now_ms()? < setup.deadline_ms,
                "task deadline exhausted",
            )?;
            let request = ModelRequest {
                role: crate::provider::Role::Implementer,
                request_id: uuid::Uuid::new_v4().to_string(),
                provider_model_id: setup.worker.model_id.clone(),
                context: context.clone(),
                snapshot_digest: candidate.digest()?,
                maximum_output_tokens: setup.worker.output_tokens,
                timeout_ms: setup.worker.timeout_ms,
                feedback: feedback.clone(),
            };
            require(
                wire::canonical(&request)?.len() <= setup.worker.context_bytes,
                "complete worker request exceeds context budget",
            )?;
            let response = crate::provider::accounted_call(
                &mut self.ledger,
                &setup.run_id,
                &request,
                crate::provider::Reservation {
                    tokens: setup.worker.token_reservation,
                    cost_microunits: setup.worker.cost_reservation,
                },
                provider,
                cancel,
            )?;
            let turn = wire::decode::<WorkerTurn>(response.output.as_bytes()).and_then(|turn| {
                turn.validate(&request)?;
                Ok(turn)
            });
            let turn = match turn {
                Ok(turn) => turn,
                Err(e) if format_repairs < setup.worker.format_repairs => {
                    format_repairs += 1;
                    feedback = vec![
                        serde_json::json!({"format_error":e.to_string(),"instruction":"Return exactly the required worker JSON schema."}),
                    ];
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !turn.questions.is_empty() {
                return Err(Error::classified(
                    crate::FailureCategory::Contract,
                    format!("semantic decision required: {}", turn.questions.join("; ")),
                ));
            }
            let broker = Broker::new(&self.ledger, &candidate, diagnostics.clone());
            if let Some(proposal) = turn.proposal {
                return broker.propose(
                    &setup.base,
                    &setup.policy,
                    &setup.admission,
                    &proposal,
                    &setup
                        .contract
                        .invariants
                        .iter()
                        .map(|i| i.id.clone())
                        .collect(),
                    runner::now_ms()?,
                );
            }
            feedback.clear();
            for tool in turn.tool_requests {
                if let crate::broker::ToolRequest::RunFastCheck { check_id } = &tool {
                    let matches: Vec<_> = setup
                        .plan
                        .checks
                        .iter()
                        .filter(|s| s.phase == Phase::Fast && s.check_id == *check_id)
                        .collect();
                    require(matches.len() == 1, "unknown or ambiguous advisory check ID")?;
                    let key = self.select_binding(setup, &candidate)?;
                    let result =
                        self.check(setup, &candidate, &key, matches[0], "advisory", cancel)?;
                    diagnostics.extend(result.evidence.iter().cloned());
                    feedback
                        .push(serde_json::to_value(result).map_err(|e| Error::new(e.to_string()))?);
                    continue;
                }
                let broker = Broker::new(&self.ledger, &candidate, diagnostics.clone());
                if matches!(
                    tool,
                    crate::broker::ToolRequest::RequestScopeChange { .. }
                        | crate::broker::ToolRequest::RequestContractChange { .. }
                ) {
                    return Err(Error::new(format!(
                        "operator decision required: {}",
                        broker.call(&tool)?
                    )));
                }
                feedback.push(broker.call(&tool)?);
            }
        }
    }
    /// Recompute acceptance from durable receipts and current external authority.
    pub fn verify(&self, run_id: &str, integration: &dyn Integration) -> Result<GateDecision> {
        let setup = self.setup(run_id)?;
        let candidate = self
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("no implementation candidate selected".into()))?;
        let key = self.key(&setup, &candidate)?;
        let required: Vec<_> = setup.plan.required_final().cloned().collect();
        let binding = Digest::of_json(&key)?;
        let all = self.ledger.completed_results(run_id, &binding)?;
        let resolved = self.resolved_failures(&setup, &all)?;
        let admitted: Vec<_> = all
            .into_iter()
            .filter(|r| {
                !resolved.contains(&r.result_id)
                    && required.iter().any(|s| s.identity() == r.identity())
            })
            .collect();
        let approvals = self.approvals_current(&setup, Some(&candidate), runner::now_ms()?)?;
        let prior_failures = {
            let mut statement=self.ledger.connection.prepare("SELECT c.result_id FROM check_results c JOIN operations o ON c.operation_id=o.operation_id WHERE o.run_id=?1 AND o.kind NOT IN ('negative_control','baseline') AND (o.state!='completed' OR c.status!='pass')")?;
            let rows = statement.query_map([run_id], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|id| !resolved.contains(id))
                .count()
        };
        let trusted = TrustedPreconditions {
            evidence_provenance_verified: true,
            artifacts_present_and_verified: self.ledger.verify_artifacts().is_ok(),
            scope_verified: crate::snapshot::admit(
                &self.ledger,
                &setup.base,
                &candidate,
                &setup.policy,
                &setup.admission,
            )
            .is_ok(),
            plan_obligations_complete: setup.validate(&self.ledger).is_ok()
                && crate::risk::assess_for_languages(
                    &self.ledger,
                    &setup.base,
                    &candidate,
                    &setup.contract.risk_tags,
                    &setup
                        .profiles
                        .iter()
                        .map(|profile| profile.language)
                        .collect::<Vec<_>>(),
                )
                .and_then(|assessment| {
                    setup
                        .risk_policy
                        .validate(&assessment, &setup.plan, &setup.registry)
                })
                .is_ok(),
            no_blocking_unknowns: setup.contract.require_resolved().is_ok()
                && setup.design.validate(&setup.contract).is_ok(),
            review_requirements_satisfied: approvals
                && self.independent_review_current(&setup, &candidate)?,
            approvals_current: approvals,
            integration_candidate_current: integration.current_revision(&setup.repository_id)?
                == candidate.base_revision,
            prior_failures_resolved: prior_failures == 0,
            accounting_complete: self
                .ledger
                .budgets(run_id)?
                .values()
                .all(|b| b.reserved == 0 && b.spent <= b.limit)
                && self
                    .ledger
                    .operations(run_id)?
                    .iter()
                    .all(|o| o.state == "completed" || o.state == "failed"),
            run_eligible: {
                let state = self.ledger.run(run_id)?.state;
                !matches!(state, State::Cancelled | State::Stopped | State::Rejected)
                    && (state == State::Accepted || runner::now_ms()? < setup.deadline_ms)
            },
        };
        Ok(evidence::evaluate(
            run_id, &key, &required, &admitted, &trusted,
        ))
    }
    /// Execute or continue the current durable stage; all model output remains untrusted.
    pub fn execute(
        &mut self,
        run_id: &str,
        provider: &mut dyn Provider,
        integration: &dyn Integration,
        cancel: &Cancellation,
    ) -> Result<RunReport> {
        let setup = self.setup(run_id)?;
        self.backend.require_environment(&setup.environment)?;
        let (active_cancel, _monitor) = ExecutionMonitor::start(
            self.ledger.root(),
            run_id,
            cancel.clone(),
            setup.deadline_ms,
        )?;
        if cancel.is_cancelled() {
            active_cancel.cancel();
        }
        let result = self.execute_stages(&setup, provider, integration, &active_cancel);
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                let mut current = self.ledger.run(run_id)?;
                if !current.state.terminal() && current.state != State::Blocked {
                    let next = if runner::now_ms()? >= setup.deadline_ms
                        || error.message.contains("budget unavailable")
                        || error.message.contains("exhausted the budget")
                    {
                        Some(State::Stopped)
                    } else if active_cancel.is_cancelled() {
                        Some(State::Cancelled)
                    } else {
                        None
                    };
                    if let Some(next) = next {
                        self.advance(
                            run_id,
                            next,
                            "execution_stopped",
                            serde_json::json!({"reason":error.to_string()}),
                        )?;
                        current = self.ledger.run(run_id)?;
                    }
                }
                let restart = if current.state == State::ReadyForApproval {
                    State::CleanCi
                } else {
                    current.state
                };
                self.block(run_id, restart, &error.to_string())?
            }
        };
        self.ledger
            .store_json("report", &report, runner::now_ms()?)?;
        Ok(report)
    }
    fn accept_atomically(
        &self,
        run_id: &str,
        integration: &dyn Integration,
        cancel: &Cancellation,
    ) -> Result<()> {
        let current = self.ledger.run(run_id)?;
        require(
            current.state == State::ReadyForApproval,
            "run is not ready for acceptance",
        )?;
        let event = self.ledger.store_json(
            "event",
            &serde_json::json!({"kind":"acceptance_committed","binding":current.binding_digest}),
            runner::now_ms()?,
        )?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.ledger.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let gate = self.verify(run_id, integration)?;
        require(
            gate.verdict == Verdict::Ready && !cancel.is_cancelled(),
            "final authority or execution state changed before acceptance",
        )?;
        let now = runner::now_ms()?;
        require(
            now < self.setup(run_id)?.deadline_ms,
            "task deadline exhausted before acceptance",
        )?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::new("run revision overflow".into()))?;
        let before = serde_json::to_string(&State::ReadyForApproval)
            .map_err(|e| Error::new(e.to_string()))?;
        let after =
            serde_json::to_string(&State::Accepted).map_err(|e| Error::new(e.to_string()))?;
        let changed=tx.execute("UPDATE runs SET state=?1,revision=?2,restart_state=NULL,updated_ms=?3 WHERE run_id=?4 AND revision=?5 AND state=?6 AND binding_digest=?7",rusqlite::params![after,revision as i64,now as i64,run_id,current.revision as i64,before,current.binding_digest.as_ref().map(Digest::as_str)])?;
        require(changed == 1, "concurrent acceptance transition")?;
        tx.execute(
            "INSERT INTO events VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                run_id,
                revision as i64,
                before,
                after,
                event.digest.as_str(),
                now as i64
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    fn execute_stages(
        &mut self,
        setup: &TaskSetup,
        provider: &mut dyn Provider,
        integration: &dyn Integration,
        cancel: &Cancellation,
    ) -> Result<RunReport> {
        loop {
            let current = self.ledger.run(&setup.run_id)?;
            if current.state.terminal() || current.state == State::Blocked {
                return Ok(RunReport {
                    run_id: setup.run_id.clone(),
                    state: current.state,
                    candidate_digest: self
                        .current_candidate(&setup.run_id)?
                        .map(|c| c.digest())
                        .transpose()?,
                    gate: None,
                    reasons: vec![],
                });
            }
            if runner::now_ms()? >= setup.deadline_ms {
                self.advance(
                    &setup.run_id,
                    State::Stopped,
                    "deadline_exhausted",
                    serde_json::json!({}),
                )?;
                continue;
            }
            if cancel.is_cancelled() {
                self.advance(
                    &setup.run_id,
                    State::Cancelled,
                    "cancelled",
                    serde_json::json!({}),
                )?;
                continue;
            }
            match current.state {
                State::Intake => {
                    setup.validate(&self.ledger)?;
                    require(
                        integration.current_revision(&setup.repository_id)?
                            == setup.base.base_revision,
                        "repository revision changed since intake",
                    )?;
                    self.advance(
                        &setup.run_id,
                        State::Baseline,
                        "intake_validated",
                        serde_json::json!({}),
                    )?;
                }
                State::Baseline => {
                    require(
                        self.approvals_current(setup, None, runner::now_ms()?)?,
                        "contract, design, oracle or plan approval is missing, expired or revoked",
                    )?;
                    self.establish_baseline(setup, cancel)?;
                    self.advance(
                        &setup.run_id,
                        State::ContractReady,
                        "semantics_approved",
                        serde_json::json!({}),
                    )?;
                }
                State::ContractReady => {
                    setup.design.validate(&setup.contract)?;
                    self.advance(
                        &setup.run_id,
                        State::DesignReady,
                        "design_validated",
                        serde_json::json!({}),
                    )?;
                }
                State::DesignReady => {
                    setup.plan.validate(&setup.plan_registry()?)?;
                    self.advance(
                        &setup.run_id,
                        State::TestsReady,
                        "oracle_registered",
                        serde_json::json!({"oracle":setup.oracle.digest()?}),
                    )?;
                }
                State::TestsReady => {
                    let key = self.select_binding(setup, &setup.negative_control)?;
                    let spec = setup
                        .plan
                        .required_final()
                        .find(|s| {
                            setup
                                .registry
                                .get(&s.command_id)
                                .is_ok_and(|c| c.kind == CheckKind::Behavioral)
                                && setup
                                    .negative_control_cases
                                    .iter()
                                    .all(|c| s.expected_case_ids.contains(c))
                        })
                        .ok_or_else(|| {
                            Error::new("no behavioral check covers the negative control".into())
                        })?;
                    let result = self.check(
                        setup,
                        &setup.negative_control,
                        &key,
                        spec,
                        "negative_control",
                        cancel,
                    )?;
                    require(result.status==Status::Fail&&setup.negative_control_cases.iter().all(|c|result.failed_case_ids.contains(c)),"oracle did not reject its relevant negative control with executed case failures")?;
                    self.advance(&setup.run_id,State::Implement,"negative_control_verified",serde_json::json!({"result":result.result_id,"cases":setup.negative_control_cases}))?;
                }
                State::Implement => {
                    let candidate = match self.implement(
                        setup,
                        provider,
                        cancel,
                        setup.base.clone(),
                        Vec::new(),
                    ) {
                        Ok(candidate) => candidate,
                        Err(error)
                            if matches!(
                                error.category,
                                crate::FailureCategory::Contract | crate::FailureCategory::Design
                            ) =>
                        {
                            self.advance(
                                &setup.run_id,
                                State::Replan,
                                "semantic_replan_requested",
                                serde_json::json!({"reason":error.message}),
                            )?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    self.select_binding(setup, &candidate)?;
                    self.advance(
                        &setup.run_id,
                        State::FastChecks,
                        "candidate_admitted",
                        serde_json::json!({"candidate":candidate.digest()?}),
                    )?;
                }
                State::FastChecks | State::RiskChecks | State::CleanCi => {
                    let candidate = self
                        .current_candidate(&setup.run_id)?
                        .ok_or_else(|| Error::new("candidate missing".into()))?;
                    let key = self.key(setup, &candidate)?;
                    let assessment = crate::risk::assess_for_languages(
                        &self.ledger,
                        &setup.base,
                        &candidate,
                        &setup.contract.risk_tags,
                        &setup
                            .profiles
                            .iter()
                            .map(|profile| profile.language)
                            .collect::<Vec<_>>(),
                    )?;
                    self.ledger
                        .store_json("report", &assessment, runner::now_ms()?)?;
                    setup
                        .risk_policy
                        .validate(&assessment, &setup.plan, &setup.registry)?;
                    let phase = match current.state {
                        State::FastChecks => Phase::Fast,
                        State::RiskChecks => Phase::Risk,
                        _ => Phase::Final,
                    };
                    // Topological order is derived from the validated DAG, never vector order.
                    let mut pending: Vec<_> = setup
                        .plan
                        .checks
                        .iter()
                        .filter(|s| s.phase == phase && s.required)
                        .collect();
                    let mut done: BTreeSet<_> = setup
                        .plan
                        .checks
                        .iter()
                        .filter(|s| s.phase != phase)
                        .map(|s| (s.check_id.clone(), s.configuration_digest.clone()))
                        .collect();
                    while !pending.is_empty() {
                        let at = pending
                            .iter()
                            .position(|s| {
                                s.dependencies.iter().all(|d| {
                                    done.contains(&(d.clone(), s.configuration_digest.clone()))
                                })
                            })
                            .ok_or_else(|| {
                                Error::new("verification dependency cannot be scheduled".into())
                            })?;
                        let spec = pending.remove(at);
                        let result = self.check(setup, &candidate, &key, spec, "check", cancel)?;
                        if result.status != Status::Pass {
                            if cancel.is_cancelled() {
                                let state = self.ledger.run(&setup.run_id)?.state;
                                if !state.terminal() && state != State::Blocked {
                                    self.advance(
                                        &setup.run_id,
                                        if runner::now_ms()? >= setup.deadline_ms {
                                            State::Stopped
                                        } else {
                                            State::Cancelled
                                        },
                                        "verification_interrupted",
                                        serde_json::json!({"result":result.result_id}),
                                    )?;
                                }
                                break;
                            }
                            if result.repairable() {
                                if let Some(budget) =
                                    self.ledger.budgets(&setup.run_id)?.get("repairs")
                                {
                                    if budget.spent < budget.limit && budget.reserved == 0 {
                                        self.advance(
                                            &setup.run_id,
                                            State::Repair,
                                            "repair_requested",
                                            serde_json::json!({"result":result.result_id}),
                                        )?;
                                        break;
                                    }
                                    self.advance(
                                        &setup.run_id,
                                        State::Stopped,
                                        "repair_budget_exhausted",
                                        serde_json::json!({"result":result.result_id}),
                                    )?;
                                    break;
                                }
                            }
                            return self.block(
                                &setup.run_id,
                                current.state,
                                &format!("check {} ended {:?}", spec.check_id, result.status),
                            );
                        }
                        done.insert((spec.check_id.clone(), spec.configuration_digest.clone()));
                    }
                    if self.ledger.run(&setup.run_id)?.state != current.state {
                        continue;
                    }
                    let next = match current.state {
                        State::FastChecks => State::RiskChecks,
                        State::RiskChecks => State::Review,
                        _ => State::ReadyForApproval,
                    };
                    if current.state == State::CleanCi {
                        if let Some(limits) = &setup.reviewer {
                            let draft = self.run_role(
                                setup,
                                &candidate,
                                crate::provider::Role::Reviewer,
                                limits,
                                provider,
                                cancel,
                            )?;
                            if let crate::roles::DraftArtifact::Review { recommendation, .. } =
                                draft.artifact
                            {
                                if recommendation != crate::roles::Recommendation::Ready {
                                    return self.block(&setup.run_id, State::CleanCi, "independent reviewer found changes or semantic decisions; inspect the recorded review before repairing or replanning");
                                }
                            }
                        }
                        let gate = self.verify(&setup.run_id, integration)?;
                        if gate.verdict != Verdict::Ready {
                            let mut report = self.block(
                                &setup.run_id,
                                State::CleanCi,
                                &gate.reasons.join("; "),
                            )?;
                            report.gate = Some(gate);
                            return Ok(report);
                        }
                    }
                    self.advance(
                        &setup.run_id,
                        next,
                        "checks_completed",
                        serde_json::json!({"phase":phase}),
                    )?;
                }
                State::Review => {
                    let candidate = self
                        .current_candidate(&setup.run_id)?
                        .ok_or_else(|| Error::new("candidate missing".into()))?;
                    require(
                        self.approvals_current(setup, Some(&candidate), runner::now_ms()?)?,
                        "candidate review approval is missing, expired or revoked",
                    )?;
                    self.advance(
                        &setup.run_id,
                        State::CleanCi,
                        "review_satisfied",
                        serde_json::json!({}),
                    )?;
                }
                State::ReadyForApproval => {
                    let gate = self.verify(&setup.run_id, integration)?;
                    if gate.verdict != Verdict::Ready {
                        let mut report =
                            self.block(&setup.run_id, State::CleanCi, &gate.reasons.join("; "))?;
                        report.gate = Some(gate);
                        return Ok(report);
                    }
                    self.accept_atomically(&setup.run_id, integration, cancel)?;
                    return Ok(RunReport {
                        run_id: setup.run_id.clone(),
                        state: State::Accepted,
                        candidate_digest: self
                            .current_candidate(&setup.run_id)?
                            .map(|c| c.digest())
                            .transpose()?,
                        gate: Some(gate),
                        reasons: vec![],
                    });
                }
                State::Repair => {
                    let candidate = self
                        .current_candidate(&setup.run_id)?
                        .ok_or_else(|| Error::new("repair has no candidate".into()))?;
                    let before = candidate.digest()?;
                    let binding = self
                        .ledger
                        .run(&setup.run_id)?
                        .binding_digest
                        .ok_or_else(|| Error::new("repair has no binding".into()))?;
                    let results = self.ledger.completed_results(&setup.run_id, &binding)?;
                    let feedback = results
                        .into_iter()
                        .map(|r| serde_json::to_value(r).map_err(|e| Error::new(e.to_string())))
                        .collect::<Result<Vec<_>>>()?;
                    let input = self.ledger.store_json(
                        "operation",
                        &serde_json::json!({"repair_of":before}),
                        runner::now_ms()?,
                    )?;
                    let lease = self.ledger.claim(
                        &setup.run_id,
                        &uuid::Uuid::new_v4().to_string(),
                        "repair",
                        &input.digest,
                        &uuid::Uuid::new_v4().to_string(),
                        runner::now_ms()?,
                        setup.deadline_ms.saturating_sub(runner::now_ms()?).max(1),
                        &[("repairs".into(), 1)].into(),
                    )?;
                    let outcome = self.implement(setup, provider, cancel, candidate, feedback);
                    let output=self.ledger.store_json("operation",&serde_json::json!({"repaired":outcome.is_ok(),"error":outcome.as_ref().err().map(ToString::to_string)}),runner::now_ms()?)?;
                    require(
                        self.ledger.complete(
                            &lease,
                            &output.digest,
                            &[("repairs".into(), 1)].into(),
                            runner::now_ms()?,
                        )?,
                        "repair exhausted the budget",
                    )?;
                    let candidate = match outcome {
                        Err(error)
                            if matches!(
                                error.category,
                                crate::FailureCategory::Contract | crate::FailureCategory::Design
                            ) =>
                        {
                            self.advance(
                                &setup.run_id,
                                State::Replan,
                                "semantic_replan_required",
                                serde_json::json!({"reason":error.message}),
                            )?;
                            continue;
                        }
                        other => other?,
                    };
                    require(
                        candidate.digest()? != before,
                        "repair produced no changed candidate",
                    )?;
                    self.select_binding(setup, &candidate)?;
                    self.advance(
                        &setup.run_id,
                        State::FastChecks,
                        "repair_admitted",
                        serde_json::json!({"candidate":candidate.digest()?}),
                    )?;
                }
                State::Replan => {
                    require(
                        self.ledger
                            .budgets(&setup.run_id)?
                            .get("replans")
                            .is_some_and(|b| b.spent < b.limit),
                        "replan budget exhausted",
                    )?;
                    let draft = self.run_role(
                        setup,
                        &setup.base,
                        crate::provider::Role::Architect,
                        &setup.worker,
                        provider,
                        cancel,
                    )?;
                    let digest = Digest::of_json(&draft)?;
                    return self.block(&setup.run_id, State::Replan, &format!("replan draft {digest} is ready; stage a replacement intake and obtain its exact approvals before activation"));
                }
                _ => return Err(Error::new("unhandled workflow state".into())),
            }
        }
    }
}

// A separate read connection observes operator cancellation/recovery while a native
// command or provider request blocks the controller thread. No worker owns this token.
pub(crate) struct ExecutionMonitor {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl ExecutionMonitor {
    pub(crate) fn start(
        root: &std::path::Path,
        run_id: &str,
        parent: Cancellation,
        deadline: u64,
    ) -> Result<(Cancellation, Self)> {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let cancel = Cancellation::default();
        let observed = cancel.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let path = root.join("ledger.sqlite3");
        let run_id = run_id.to_string();
        let thread = std::thread::Builder::new()
            .name("harness-control".into())
            .spawn(move || {
                let connection = rusqlite::Connection::open_with_flags(
                    path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                );
                let Ok(connection) = connection else {
                    observed.cancel();
                    return;
                };
                if connection
                    .busy_timeout(std::time::Duration::from_millis(200))
                    .is_err()
                {
                    observed.cancel();
                    return;
                }
                while !stopping.load(Ordering::SeqCst) {
                    let state = connection
                        .query_row("SELECT state FROM runs WHERE run_id=?1", [&run_id], |r| {
                            r.get::<_, String>(0)
                        })
                        .map_err(Error::from)
                        .and_then(|s| wire::decode::<State>(s.as_bytes()));
                    if parent.is_cancelled()
                        || runner::now_ms().map_or(true, |now| now >= deadline)
                        || state.map_or(true, |s| s.terminal() || s == State::Blocked)
                    {
                        observed.cancel();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })?;
        Ok((
            cancel,
            Self {
                stop,
                thread: Some(thread),
            },
        ))
    }
}
impl Drop for ExecutionMonitor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("harness control monitor terminated unexpectedly");
            }
        }
    }
}
