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
    runner::{self, Cancellation, DockerBackend, Environment, ExecutionInput},
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
        Ok(PlanRegistry {
            commands,
            configurations,
            authorized_dispositions: BTreeSet::new(),
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
        self.plan.validate(&self.plan_registry()?)?;
        for check in &self.plan.checks {
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
        require(
            self.worker.context_bytes > 0
                && self.worker.context_bytes <= wire::MAX_JSON_BYTES
                && self.worker.output_tokens > 0
                && self.worker.timeout_ms > 0
                && self.worker.timeout_ms <= 86_400_000
                && self.worker.format_repairs <= 1,
            "unresolved worker limits",
        )?;
        require(
            self.worker.token_reservation >= self.worker.output_tokens,
            "token reservation omits requested output",
        )?;
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
    /// Exact approval requirements that cannot be removed by a model plan.
    pub fn mandatory_approvals(&self) -> Result<Vec<ApprovalRequirement>> {
        Ok(vec![
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
    pub backend: DockerBackend,
}

impl Controller {
    /// Register exact intake artifacts and create the durable run.
    pub fn create(&mut self, setup: &TaskSetup, now: u64) -> Result<RunRecord> {
        setup.validate(&self.ledger)?;
        require(setup.deadline_ms > now, "task deadline already exhausted")?;
        for (kind, value) in [
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
                .store_json(kind, &value.map_err(|e| Error(e.to_string()))?, now)?;
        }
        let input = self.ledger.store_json("operation", setup, now)?;
        self.ledger
            .create_run(&setup.run_id, &input.digest, &setup.budgets, now)
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
    fn select_binding(&mut self, setup: &TaskSetup, candidate: &Snapshot) -> Result<EvidenceKey> {
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
    fn approvals_current(
        &self,
        setup: &TaskSetup,
        candidate: Option<&Snapshot>,
        now: u64,
    ) -> Result<bool> {
        let scope = Digest::of_json(&setup.contract.scope)?;
        let policy = Digest::of_json(&setup.policy)?;
        let mut requirements = setup.mandatory_approvals()?;
        requirements.extend(setup.plan.approval_requirements.iter().cloned());
        if let Some(candidate) = candidate {
            for kind in &setup.plan.review_requirements {
                requirements.push(ApprovalRequirement {
                    subject_digest: candidate.digest()?,
                    approval_type: kind.clone(),
                    obligation_ids: vec![],
                });
            }
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
    fn check(
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
        let operation_key = Digest::of_json(&(kind, &input.digest))?;
        let lease = self.ledger.claim(
            &setup.run_id,
            operation_key.as_str(),
            kind,
            &input.digest,
            "local-controller",
            now,
            spec.timeout_ms.saturating_add(720_000),
            &[("checks".into(), 1)].into(),
        )?;
        let observation = self.backend.execute(
            &self.ledger,
            candidate,
            &setup.oracle,
            &setup.dependencies,
            &setup.environment,
            &setup.registry,
            spec,
            cancel,
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
                .map_err(|_| Error("mandatory context is not UTF-8".into()))?;
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
                .map_err(|_| Error("profile context is not UTF-8".into()))?,
            artifact_digest: profiles.digest,
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
                .map_err(|_| Error("source index is not UTF-8".into()))?,
            artifact_digest: index.digest,
            trust: Trust::ObservedCode,
            source_revision: revision.clone(),
            mandatory: false,
        });
        for (path, file) in candidate
            .files
            .iter()
            .filter(|(path, _)| {
                setup
                    .contract
                    .scope
                    .allow_write
                    .iter()
                    .any(|p| crate::scope::matches(p, path))
            })
            .take(12)
        {
            let bytes = self.ledger.read(&file.content.digest)?;
            if let Ok(text) = String::from_utf8(bytes) {
                items.push(ContextItem {
                    item_id: format!("source:{path}"),
                    artifact_digest: file.content.digest.clone(),
                    trust: Trust::ObservedCode,
                    source_revision: revision.clone(),
                    mandatory: false,
                    text,
                });
            }
        }
        context::build(
            &self.ledger,
            items,
            setup.worker.context_bytes.saturating_sub(4096),
        )
    }
    fn implement(
        &mut self,
        setup: &TaskSetup,
        provider: &mut dyn Provider,
        cancel: &Cancellation,
    ) -> Result<Snapshot> {
        let candidate = setup.base.clone();
        let context = self.context(setup, &candidate)?;
        let mut feedback = Vec::new();
        let mut diagnostics = BTreeSet::new();
        let mut format_repairs = 0;
        loop {
            require(!cancel.is_cancelled(), "worker cancelled")?;
            require(
                runner::now_ms()? < setup.deadline_ms,
                "task deadline exhausted",
            )?;
            let request = ModelRequest {
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
            let now = runner::now_ms()?;
            let input = self.ledger.store_json("context", &request, now)?;
            let reservation: BTreeMap<_, _> = [
                ("model_calls".into(), 1),
                ("tokens".into(), setup.worker.token_reservation),
                ("cost_microunits".into(), setup.worker.cost_reservation),
            ]
            .into();
            let lease = self.ledger.claim(
                &setup.run_id,
                &request.request_id,
                "model",
                &input.digest,
                "local-controller",
                now,
                setup.worker.timeout_ms.saturating_add(10_000),
                &reservation,
            )?;
            let response = provider.generate(&request, cancel)?;
            require(
                response.request_id == request.request_id
                    && response.output.len() <= wire::MAX_JSON_BYTES,
                "provider response correlation or size violation",
            )?;
            let output =
                self.ledger
                    .store_json("provider-response", &response, runner::now_ms()?)?;
            let usage = response.usage.as_ref().ok_or_else(|| {
                Error("provider supplied no usage; reservation retained for reconciliation".into())
            })?;
            let tokens = usage
                .input_tokens
                .checked_add(usage.output_tokens)
                .ok_or_else(|| Error("provider token usage overflow".into()))?;
            let cost = usage.cost_microunits.ok_or_else(|| {
                Error("provider cost unresolved; reservation retained for reconciliation".into())
            })?;
            let actual = [
                ("model_calls".into(), 1),
                ("tokens".into(), tokens),
                ("cost_microunits".into(), cost),
            ]
            .into();
            require(
                self.ledger
                    .complete(&lease, &output.digest, &actual, runner::now_ms()?)?,
                "actual model usage exhausted the budget",
            )?;
            require(
                response.finish_reason == "stop",
                "provider did not complete its structured response",
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
                return Err(Error(format!(
                    "semantic decision required: {}",
                    turn.questions.join("; ")
                )));
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
                    feedback.push(serde_json::to_value(result).map_err(|e| Error(e.to_string()))?);
                    continue;
                }
                let broker = Broker::new(&self.ledger, &candidate, diagnostics.clone());
                if matches!(
                    tool,
                    crate::broker::ToolRequest::RequestScopeChange { .. }
                        | crate::broker::ToolRequest::RequestContractChange { .. }
                ) {
                    return Err(Error(format!(
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
            .ok_or_else(|| Error("no implementation candidate selected".into()))?;
        let key = self.key(&setup, &candidate)?;
        let required: Vec<_> = setup.plan.required_final().cloned().collect();
        let binding = Digest::of_json(&key)?;
        let all = self.ledger.admitted_results(run_id, &binding)?;
        let admitted: Vec<_> = all
            .into_iter()
            .filter(|r| required.iter().any(|s| s.identity() == r.identity()))
            .collect();
        let approvals = self.approvals_current(&setup, Some(&candidate), runner::now_ms()?)?;
        let prior_failures:i64=self.ledger.connection.query_row(
            "SELECT COUNT(*) FROM check_results c JOIN operations o ON c.operation_id=o.operation_id WHERE o.run_id=?1 AND o.kind!='negative_control' AND (o.state!='completed' OR c.status!='pass')",
            [run_id],|r|r.get(0)).map_err(|e|Error(e.to_string()))?;
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
            plan_obligations_complete: setup.validate(&self.ledger).is_ok(),
            no_blocking_unknowns: setup.contract.require_resolved().is_ok()
                && setup.design.validate(&setup.contract).is_ok(),
            review_requirements_satisfied: approvals,
            approvals_current: approvals,
            integration_candidate_current: integration.current_revision(&setup.repository_id)?
                == candidate.base_revision,
            prior_failures_resolved: prior_failures == 0,
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
        let result = self.execute_stages(&setup, provider, integration, cancel);
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                let current = self.ledger.run(run_id)?;
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
            if cancel.is_cancelled() {
                self.advance(
                    &setup.run_id,
                    State::Cancelled,
                    "cancelled",
                    serde_json::json!({}),
                )?;
                continue;
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
                            Error("no behavioral check covers the negative control".into())
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
                    let candidate = self.implement(setup, provider, cancel)?;
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
                        .ok_or_else(|| Error("candidate missing".into()))?;
                    let key = self.key(setup, &candidate)?;
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
                                Error("verification dependency cannot be scheduled".into())
                            })?;
                        let spec = pending.remove(at);
                        let result = self.check(setup, &candidate, &key, spec, "check", cancel)?;
                        if result.status != Status::Pass {
                            return self.block(
                                &setup.run_id,
                                current.state,
                                &format!("check {} ended {:?}", spec.check_id, result.status),
                            );
                        }
                        done.insert((spec.check_id.clone(), spec.configuration_digest.clone()));
                    }
                    let next = match current.state {
                        State::FastChecks => State::RiskChecks,
                        State::RiskChecks => State::Review,
                        _ => State::ReadyForApproval,
                    };
                    if current.state == State::CleanCi {
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
                        .ok_or_else(|| Error("candidate missing".into()))?;
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
                    self.advance(
                        &setup.run_id,
                        State::Accepted,
                        "acceptance_committed",
                        serde_json::to_value(&gate).map_err(|e| Error(e.to_string()))?,
                    )?;
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
                State::Repair | State::Replan => {
                    return Err(Error(
                        "repair or replan requires an explicit new candidate or approved intake"
                            .into(),
                    ))
                }
                _ => return Err(Error("unhandled workflow state".into())),
            }
        }
    }
}
