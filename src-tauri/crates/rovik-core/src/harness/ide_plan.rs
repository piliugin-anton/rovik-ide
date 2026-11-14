//! Saved-plan implementation and review, using the same durable controller as verification.
use super::*;
use crate::harness::{HarnessProvider, Rates};
use rovik_harness::{
    contract::ContractRegistry, provider::Role, review_bundle::SnapshotPatch, roles,
};
use rovik_protocol::harness::{DraftRole, ImplementationProvider, ModelRates};

/// Non-secret provider identity retained to prevent a resumed run changing models or prices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProviderBinding {
    pub(super) profile_id: String,
    pub(super) model: String,
    pub(super) endpoint: String,
    pub(super) rates: ModelRates,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PlanBinding {
    pub path: String,
    pub digest: Digest,
    pub provider: ProviderBinding,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SavedPlan {
    pub path: String,
    pub digest: Digest,
    pub content: String,
}

pub(super) fn binding(selection: &ImplementationProvider) -> Result<ProviderBinding> {
    let profile = &selection.profile;
    check(profile.connector_kind().is_none() && profile.connector.is_none()
        && !crate::anthropic::is_anthropic(&profile.base_url)
        && profile.api_route.as_deref().is_none_or(|route| route == "chat_completions"),
        "Verified implementation currently requires an OpenAI-compatible chat-completions provider.")?;
    check(
        !profile.model.trim().is_empty()
            && profile.model.len() <= 512
            && !profile.id.trim().is_empty()
            && profile.id.len() <= 1024,
        "Select a configured model for verified implementation.",
    )?;
    let endpoint = format!(
        "{}/chat/completions",
        profile.base_url.trim_end_matches('/')
    );
    let parsed = reqwest::Url::parse(&endpoint)
        .map_err(|_| Error::new("Invalid provider endpoint.".into()))?;
    check(
        parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "Provider credentials must use the configured API key, not the endpoint URL.",
    )?;
    // The shared provider enforces HTTPS or explicit loopback HTTP.
    let _ = provider(selection)?;
    Ok(ProviderBinding {
        profile_id: profile.id.clone(),
        model: profile.model.clone(),
        endpoint,
        rates: selection.rates.clone(),
    })
}

pub(super) fn provider(selection: &ImplementationProvider) -> Result<HarnessProvider> {
    HarnessProvider::new(
        crate::rt::handle().clone(),
        format!(
            "{}/chat/completions",
            selection.profile.base_url.trim_end_matches('/')
        ),
        selection.profile.api_key.clone(),
        Rates {
            input: selection.rates.input,
            cached_input: selection.rates.cached_input,
            output: selection.rates.output,
        },
    )
}

// The controller bounds the complete serialized request by context_bytes. One
// UTF-8 byte per input token plus role instructions and message framing is a
// conservative reservation; actual provider accounting releases unused budget.
// Never increase the operator's total budget to make an underfunded task run.
pub(super) fn reserve_provider_limits(
    limits: &mut rovik_harness::controller::WorkerLimits,
    rates: &ModelRates,
    budgets: &BTreeMap<String, u64>,
) -> Result<()> {
    limits.validate()?;
    let control = [
        Role::Implementer,
        Role::DomainAnalyst,
        Role::Architect,
        Role::OracleAuthor,
        Role::Reviewer,
    ]
    .into_iter()
    .map(|role| role.control().len())
    .max()
    .unwrap_or(0);
    let required = Rates {
        input: rates.input,
        cached_input: rates.cached_input,
        output: rates.output,
    }
    .reservation(limits.context_bytes, control, limits.output_tokens)?;
    limits.token_reservation = limits.token_reservation.max(required.tokens);
    limits.cost_reservation = limits.cost_reservation.max(required.cost_microunits);
    for (name, required) in [
        ("model_calls", 1),
        ("tokens", limits.token_reservation),
        ("cost_microunits", limits.cost_reservation),
    ] {
        check(
            budgets.get(name).is_some_and(|total| *total >= required),
            &format!("The reviewed {name} budget cannot reserve one complete model request ({required}). Reduce context/output limits or prepare a larger explicit budget."),
        )?;
    }
    Ok(())
}

pub(super) fn require_unchanged_protected_inputs(
    setup: &TaskSetup,
    captured: &Snapshot,
) -> Result<()> {
    let protected = setup
        .policy
        .scopes
        .iter()
        .chain(std::iter::once(&setup.contract.scope))
        .flat_map(|scope| &scope.protected)
        .collect::<Vec<_>>();
    for path in setup
        .base
        .files
        .keys()
        .chain(captured.files.keys())
        .collect::<BTreeSet<_>>()
    {
        if setup.base.files.get(path) != captured.files.get(path)
            && protected
                .iter()
                .any(|pattern| rovik_harness::scope::matches(pattern, path))
        {
            return Err(Error::new(format!("Protected acceptance or build input changed in the working tree: {path}. Prepare a new independently reviewed setup before implementation.")));
        }
    }
    Ok(())
}

impl IdeProject {
    pub(super) fn read_plan(&self, path: &str) -> Result<SavedPlan> {
        let candidate = Path::new(path);
        let relative = if candidate.is_absolute() {
            candidate
                .strip_prefix(&self.root)
                .map_err(|_| Error::new("Select a plan inside this execution workspace.".into()))?
        } else {
            candidate
        };
        let name = relative
            .to_str()
            .ok_or_else(|| Error::new("Plan paths must be UTF-8.".into()))?
            .replace('\\', "/");
        rovik_harness::scope::validate_path(&name)?;
        check(
            !name
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "Git administration files are not implementation plans.",
        )?;
        let path = self.root.join(&name);
        let mut current = self.root.clone();
        for part in Path::new(&name).components() {
            current.push(part);
            check(
                !std::fs::symlink_metadata(&current)?
                    .file_type()
                    .is_symlink(),
                "Saved plans cannot traverse symlinks.",
            )?;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        check(
            metadata.is_file() && metadata.len() <= 256 * 1024,
            "Select a saved plan smaller than 256 KiB.",
        )?;
        let bytes = std::fs::read(&path)?;
        let after = std::fs::symlink_metadata(&path)?;
        check(
            bytes.len() as u64 == metadata.len()
                && metadata.len() == after.len()
                && metadata.modified()? == after.modified()?
                && !after.file_type().is_symlink(),
            "The plan changed during capture. Review it again.",
        )?;
        let digest = Digest::of(&bytes);
        let content = String::from_utf8(bytes)
            .map_err(|_| Error::new("Saved plans must be UTF-8 text.".into()))?;
        check(
            !content.trim().is_empty(),
            "Write and save a plan before implementation.",
        )?;
        Ok(SavedPlan {
            path: name,
            digest,
            content,
        })
    }

    pub(super) fn start_implementation(
        &self,
        path: &str,
        expected_plan: &Digest,
        expected_source: &Digest,
        untracked: BTreeSet<String>,
        selection: ImplementationProvider,
    ) -> Result<Value> {
        let plan = self.read_plan(path)?;
        check(
            &plan.digest == expected_plan,
            "The plan changed after review. Review its current content before implementation.",
        )?;
        let provider = binding(&selection)?;
        let configuration = self.configuration()?.ok_or_else(|| {
            Error::new("Prepare and select the task's protected acceptance criteria first.".into())
        })?;
        let captured = self.current_capture(expected_source, &untracked)?;
        let setup = self.implementation_setup(&configuration, captured, &plan, &provider)?;
        let mut controller = self.controller(Some(&configuration.engine))?;
        setup
            .authority
            .authorize(&Principal::current()?, "operate")?;
        let now = runner::now_ms()?;
        controller.create(&setup, now)?;
        let input = RunInput {
            plan: Some(PlanBinding {
                path: plan.path,
                digest: plan.digest,
                provider,
            }),
            execution_root: self.root.clone(),
            baseline_reference: self.baseline_reference.clone(),
            tracked_paths: self.tracked_paths.clone(),
            configuration_digest: Digest::of_json(&configuration)?,
            configuration,
            snapshot_digest: expected_source.clone(),
            untracked,
        };
        self.database()?.execute(
            "INSERT INTO runs(id,input,created_ms) VALUES(?1,?2,?3)",
            params![
                setup.run_id,
                serde_json::to_string(&input).map_err(|e| Error::new(e.to_string()))?,
                i64::try_from(now).map_err(|_| Error::new("Run timestamp overflow.".into()))?
            ],
        )?;
        // Every new plan changes the contract and policy subjects. The first response
        // is a concrete review; credentials are supplied again only when resumed.
        self.status(&setup.run_id)
    }

    pub(super) fn implementation_setup(
        &self,
        configuration: &Configuration,
        captured: Snapshot,
        plan: &SavedPlan,
        provider: &ProviderBinding,
    ) -> Result<TaskSetup> {
        let mut setup = self.load_setup(&configuration.setup_id)?;
        self.require_setup_baseline(&setup)?;
        check(
            setup.deadline_ms > runner::now_ms()?,
            "The prepared task deadline expired. Review a new execution budget.",
        )?;
        setup.run_id = format!("ide-{}", uuid::Uuid::new_v4());
        setup.plan.run_id = setup.run_id.clone();
        let before = setup.base.clone();
        require_unchanged_protected_inputs(&setup, &captured)?;
        let source_digest = captured.digest()?;
        let mut captured = captured;
        // Existing user deletions belong to the starting tree, not to the future
        // worker patch. Keep the original capture bound separately for freshness.
        captured.deletions.clear();
        let revision = captured.digest()?;
        for citation in &mut setup.design.existing_code_considered {
            check(before.files.get(&citation.file) == captured.files.get(&citation.file), "A cited implementation changed in the selected working tree. Review a new design against those exact bytes.")?;
            citation.source_revision = revision.clone();
        }
        setup.base = captured;
        setup.initial_candidate = None;
        // The saved request is input, never a worker-editable acceptance target.
        if !setup.contract.scope.protected.contains(&plan.path) {
            setup.contract.scope.protected.push(plan.path.clone());
        }
        setup.policy.scopes.push(setup.contract.scope.clone());
        setup.worker.model_id = provider.model.clone();
        if let Some(reviewer) = &setup.reviewer {
            check(reviewer.model_id == provider.model, "The selected setup requires a different reviewer model. Prepare matching model settings before implementation.")?;
        }
        reserve_provider_limits(&mut setup.worker, &provider.rates, &setup.budgets)?;
        if let Some(reviewer) = &mut setup.reviewer {
            reserve_provider_limits(reviewer, &provider.rates, &setup.budgets)?;
        }
        setup.contract.intent = format!(
            "{}\n\nReviewed implementation plan:\n{}",
            setup.contract.intent, plan.content
        );
        setup
            .policy
            .facts
            .insert("ide.plan_digest".into(), plan.digest.to_string());
        setup
            .policy
            .facts
            .insert("ide.source_digest".into(), source_digest.to_string());
        setup.policy.facts.insert(
            "ide.provider_binding".into(),
            Digest::of_json(provider)?.to_string(),
        );
        setup.policy.facts.insert(
            "ide.model_limits".into(),
            Digest::of_json(&(&setup.worker, &setup.reviewer, &setup.budgets))?.to_string(),
        );
        setup.design.contract_digest = Digest::of_json(&setup.contract)?;
        setup.plan.contract_digest = setup.design.contract_digest.clone();
        setup.plan.design_digest = Digest::of_json(&setup.design)?;
        setup.plan.policy_digest = Digest::of_json(&setup.policy)?;
        let ledger = self.ledger()?;
        ledger.store(
            "context",
            "text/plain",
            plan.content.as_bytes(),
            runner::now_ms()?,
        )?;
        setup.validate(&ledger)?;
        Ok(setup)
    }

    pub(super) fn resume_implementation(
        &self,
        run_id: &str,
        selection: ImplementationProvider,
        role: Option<DraftRole>,
    ) -> Result<Value> {
        let input = self.run_input(run_id)?;
        let plan = input.plan.as_ref().ok_or_else(|| {
            Error::new(
                "This is a model-free verification; start a verified implementation first.".into(),
            )
        })?;
        check(plan.provider == binding(&selection)?, "Resume with the original provider, model and billing rates. A new selection needs a separately reviewed task.")?;
        check(
            self.read_plan(&plan.path)?.digest == plan.digest,
            "The plan changed; prepare a new reviewed implementation.",
        )?;
        self.current_capture(&input.snapshot_digest, &input.untracked)?;
        check(
            self.configuration()?
                .map(|c| Digest::of_json(&c))
                .transpose()?
                .as_ref()
                == Some(&input.configuration_digest),
            "The reviewed execution setup changed. Prepare a new run.",
        )?;
        check(!self.is_running(run_id)?, "This task is already running.")?;
        let controller = self.controller(Some(&input.configuration.engine))?;
        let setup = controller.setup(run_id)?;
        setup
            .authority
            .authorize(&Principal::current()?, "operate")?;
        let state = controller.ledger.run(run_id)?.state;
        check(
            matches!(state, State::Intake | State::Baseline | State::Blocked),
            "Only a reviewed intake or blocked implementation can resume.",
        )?;
        if role.is_some() {
            check(state == State::Intake, "Preparation drafts are available before execution; active runs require reviewed replanning.")?;
        } else {
            check(controller.pending_approvals(run_id)?.is_empty(), "Review the complete plan, criteria, baseline and protected tests before implementation.")?;
        }
        self.launch_with_provider(run_id, input, Some(selection), role)?;
        self.status(run_id)
    }

    pub(super) fn review_changes(&self, run_id: &str) -> Result<Value> {
        let status = self.status(run_id)?;
        let controller = self.controller(None)?;
        let setup = controller.setup(run_id)?;
        let candidate = controller
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("This run has no candidate to review yet.".into()))?;
        let patch = SnapshotPatch::between(&setup.base, &candidate)?;
        let mut remaining = 2 * 1024 * 1024;
        let mut files = Vec::new();
        for (path, change) in &patch.changes {
            let mut row = json!({"path":path,"beforeDigest":change.before.as_ref().map(|f|&f.content.digest),"afterDigest":change.after.as_ref().map(|f|&f.content.digest),"beforeMode":change.before.as_ref().map(|f|f.mode),"afterMode":change.after.as_ref().map(|f|f.mode)});
            for (name, file) in [("before", &change.before), ("after", &change.after)] {
                if let Some(file) = file {
                    if file.content.byte_count <= 128 * 1024 && file.content.byte_count <= remaining
                    {
                        let bytes = controller.ledger.read(&file.content.digest)?;
                        remaining -= bytes.len() as u64;
                        match String::from_utf8(bytes) {
                            Ok(text) => row[name] = json!(text),
                            Err(_) => {
                                row[format!("{name}Omitted")] =
                                    json!("Binary content; export the exact review bundle.")
                            }
                        }
                    } else {
                        row[format!("{name}Omitted")] = json!(
                            "Content exceeds preview bounds; export the exact review bundle."
                        );
                    }
                }
            }
            files.push(row);
        }
        Ok(
            json!({"runId":run_id,"status":status,"baseDigest":patch.base_digest,"candidateDigest":patch.candidate_digest,"files":files,"applied":false}),
        )
    }

    pub(super) fn export_review(&self, run_id: &str) -> Result<Value> {
        self.require_run_context(&self.run_input(run_id)?)?;
        let destination = self.state.join(format!("review-{}", uuid::Uuid::new_v4()));
        let manifest = self.controller(None)?.export_bundle(run_id, &destination)?;
        Ok(json!({"runId":run_id,"path":destination,"manifest":manifest,"applied":false}))
    }

    pub(super) fn prepare_delivery(
        &self,
        run_id: &str,
        reference: &str,
        expected_head: &str,
        message: &str,
    ) -> Result<Value> {
        check(self.status(run_id)?["status"] == "verified", "Only a current accepted candidate can prepare delivery. Reverify changed project inputs.")?;
        let controller = self.controller(None)?;
        let setup = controller.setup(run_id)?;
        setup
            .authority
            .authorize(&Principal::current()?, "operate")?;
        let integration =
            GitIntegration::for_reference(self.repository_id.clone(), &self.root, reference)?;
        use rovik_harness::controller::Integration;
        check(
            integration.current_revision(&self.repository_id)? == expected_head,
            "The destination moved. Reconstruct and verify a new merge candidate.",
        )?;
        let delivery = integration.prepare_delivery(&controller, run_id, message)?;
        check(
            delivery.expected_head == expected_head,
            "The destination differs from the verified baseline.",
        )?;
        Ok(
            json!({"runId":run_id,"delivery":delivery,"applied":false,"requiresReviewedIntegration":true}),
        )
    }
}

fn registry(setup: &TaskSetup) -> Result<ContractRegistry> {
    Ok(ContractRegistry {
        configurations: setup
            .profiles
            .iter()
            .flat_map(|profile| &profile.configurations)
            .map(|configuration| configuration.digest())
            .collect::<Result<_>>()?,
        profiles: setup.profiles.iter().map(|p| p.id.clone()).collect(),
        suites: setup.suites.clone(),
        constraints: setup.constraint_names.clone(),
        risks: setup.risk_names.clone(),
    })
}

pub(super) fn draft(
    controller: &mut Controller,
    run_id: &str,
    role: DraftRole,
    provider: &mut HarnessProvider,
    cancel: &Cancellation,
) -> Result<Value> {
    let setup = controller.setup(run_id)?;
    let vocabulary = registry(&setup)?;
    let artifacts = BTreeMap::from([
        ("contract".into(), Digest::of_json(&setup.contract)?),
        ("design".into(), Digest::of_json(&setup.design)?),
        ("policy".into(), Digest::of_json(&setup.policy)?),
    ]);
    let input = roles::RoleInput {
        run_id,
        role: match role {
            DraftRole::Domain => Role::DomainAnalyst,
            DraftRole::Architect => Role::Architect,
            DraftRole::Oracle => Role::OracleAuthor,
        },
        snapshot: &setup.base,
        registry: &vocabulary,
        artifacts: &artifacts,
        limits: &setup.worker,
        deadline_ms: setup.deadline_ms,
    };
    let output = roles::generate(&mut controller.ledger, &input, provider, cancel)?;
    Ok(json!({"draft":output,"advisory":true,"requiresReview":true}))
}

pub(super) fn drafts(controller: &Controller, run_id: &str) -> Result<Vec<Value>> {
    let setup = controller.setup(run_id)?;
    let vocabulary = registry(&setup)?;
    let artifacts = BTreeMap::from([
        ("contract".into(), Digest::of_json(&setup.contract)?),
        ("design".into(), Digest::of_json(&setup.design)?),
        ("policy".into(), Digest::of_json(&setup.policy)?),
    ]);
    let mut drafts = Vec::new();
    for role in [Role::DomainAnalyst, Role::Architect, Role::OracleAuthor] {
        let input = roles::RoleInput {
            run_id,
            role,
            snapshot: &setup.base,
            registry: &vocabulary,
            artifacts: &artifacts,
            limits: &setup.worker,
            deadline_ms: setup.deadline_ms,
        };
        if let Some(draft) = roles::completed(&controller.ledger, &input)? {
            drafts.push(json!({"role":role,"draft":draft,"advisory":true,"requiresReview":true}));
        }
    }
    Ok(drafts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(key: &str) -> ImplementationProvider {
        serde_json::from_value(json!({"profile":{"id":"local::fixture-scripted","label":"Local fixture","model":"fixture-scripted","baseUrl":"http://localhost:8000/v1","apiKey":key,"contextTokens":64000,"firstByteTimeoutMs":null},"rates":{"input":0,"cachedInput":0,"output":0}})).unwrap()
    }

    fn funded(project: &IdeProject, image: Digest, executable: &str) -> Configuration {
        let mut configuration = super::super::tests::prepared(project, image, executable);
        let mut setup = project.load_setup(&configuration.setup_id).unwrap();
        setup.budgets.insert("tokens".into(), 256_000);
        let registered = project.register_setup(setup).unwrap();
        configuration.setup_id = Digest::parse(registered["id"].as_str().unwrap()).unwrap();
        configuration
    }

    #[test]
    fn provider_binding_excludes_rotatable_credentials_and_rejects_unsupported_routes() {
        let first = binding(&selected("private-first-key")).unwrap();
        assert_eq!(first, binding(&selected("rotated-key")).unwrap());
        let stored = serde_json::to_string(&first).unwrap();
        assert!(!stored.contains("private-first-key"));
        assert!(!stored.contains("apiKey"));
        let mut changed = selected("key");
        changed.profile.model = "different".into();
        assert_ne!(first, binding(&changed).unwrap());
        changed.profile.api_route = Some("responses".into());
        assert!(binding(&changed).is_err());
        changed.profile.api_route = None;
        changed.profile.base_url = "https://api.example/v1?secret=forbidden".into();
        assert!(binding(&changed).is_err());
    }

    #[test]
    fn saved_plan_review_binds_exact_bytes_and_rejects_outside_paths() {
        let (_dir, project) = super::super::tests::project();
        std::fs::write(
            project.root.join("plan.md"),
            "# Plan\nPreserve exact capacity.\n",
        )
        .unwrap();
        let original = project.read_plan("plan.md").unwrap();
        assert_eq!(original.digest, Digest::of(original.content.as_bytes()));
        std::fs::write(project.root.join("plan.md"), "# Changed\n").unwrap();
        assert_ne!(
            original.digest,
            project.read_plan("plan.md").unwrap().digest
        );
        assert!(project.read_plan("../outside.md").is_err());
        assert!(project.read_plan(".git/config").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                project.root.join("plan.md"),
                project.root.join("linked.md"),
            )
            .unwrap();
            assert!(project.read_plan("linked.md").is_err());
        }
    }

    #[test]
    fn implementation_uses_selected_dirty_bytes_and_requires_new_plan_approvals() {
        let (_dir, project) = super::super::tests::project();
        let configuration = funded(&project, Digest::of(b"image"), "/bin/true");
        let old = project.load_setup(&configuration.setup_id).unwrap();
        std::fs::write(project.root.join("notes.txt"), "existing user work\n").unwrap();
        let captured = project.capture(&["notes.txt".into()].into()).unwrap();
        let digest = captured.digest().unwrap();
        std::fs::write(
            project.root.join("plan.md"),
            "# Plan\nRepair exact capacity without changing unrelated notes.\n",
        )
        .unwrap();
        let plan = project.read_plan("plan.md").unwrap();
        let selection = binding(&selected("never persist this key")).unwrap();
        let setup = project
            .implementation_setup(&configuration, captured, &plan, &selection)
            .unwrap();
        assert!(
            setup.initial_candidate.is_none(),
            "implementation must generate instead of verify-only bypassing the worker"
        );
        assert!(setup.base.files.contains_key("notes.txt"));
        assert_eq!(setup.policy.facts["ide.source_digest"], digest.to_string());
        assert_eq!(
            setup.policy.facts["ide.plan_digest"],
            plan.digest.to_string()
        );
        assert!(setup.contract.intent.contains(&plan.content));
        assert_ne!(setup.plan.contract_digest, old.plan.contract_digest);
        assert_ne!(setup.plan.policy_digest, old.plan.policy_digest);
        assert_eq!(
            setup.baseline, old.baseline,
            "historical failures must never be rewritten implicitly"
        );
        assert!(!serde_json::to_string(&setup)
            .unwrap()
            .contains("never persist this key"));
    }

    #[test]
    fn changed_reuse_citations_require_new_design_instead_of_silent_rebinding() {
        let (_dir, project) = super::super::tests::project();
        let configuration =
            super::super::tests::prepared(&project, Digest::of(b"image"), "/bin/true");
        std::fs::write(
            project.root.join("plan.md"),
            "# Plan\nRepair exact capacity.\n",
        )
        .unwrap();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            rovik_harness::fixtures::QUOTA_RUST,
        )
        .unwrap();
        let captured = project.capture(&BTreeSet::new()).unwrap();
        let plan = project.read_plan("plan.md").unwrap();
        let error = project
            .implementation_setup(
                &configuration,
                captured,
                &plan,
                &binding(&selected("key")).unwrap(),
            )
            .err()
            .unwrap();
        assert!(error.message.contains("cited implementation changed"));
    }

    #[test]
    fn dirty_protected_tests_and_build_inputs_cannot_become_a_new_baseline() {
        let (_dir, project) = super::super::tests::project();
        let configuration = funded(&project, Digest::of(b"image"), "/bin/true");
        let mut setup = project.load_setup(&configuration.setup_id).unwrap();
        let native = rovik_harness::fixtures::snapshot(
            &project.ledger().unwrap(),
            "native-inputs",
            &[
                (
                    "tests/native.rs",
                    "#[test] fn behavioral_oracle() { assert!(real_check()); }",
                ),
                (
                    "Cargo.toml",
                    "[package]\nname = 'protected-consumer'\nversion = '0.0.0'\n",
                ),
            ],
        )
        .unwrap();
        setup.base.files.extend(native.files);
        setup
            .contract
            .scope
            .protected
            .extend(["tests/native.rs".into(), "Cargo.toml".into()]);
        for path in ["tests/native.rs", "Cargo.toml"] {
            let mut changed = setup.base.clone();
            let harmless = rovik_harness::fixtures::snapshot(
                &project.ledger().unwrap(),
                "tampered",
                &[(path, "// no real tests")],
            )
            .unwrap();
            changed.files.extend(harmless.files);
            assert!(require_unchanged_protected_inputs(&setup, &changed)
                .unwrap_err()
                .message
                .contains(path));
            changed.files.remove(path);
            assert!(require_unchanged_protected_inputs(&setup, &changed).is_err());
            let mut mode = setup.base.clone();
            mode.files.get_mut(path).unwrap().mode = 0o100755;
            assert!(require_unchanged_protected_inputs(&setup, &mode).is_err());
        }
        let mut unrelated = setup.base.clone();
        unrelated.files.extend(
            rovik_harness::fixtures::snapshot(
                &project.ledger().unwrap(),
                "notes",
                &[("notes.txt", "unrelated existing user work")],
            )
            .unwrap()
            .files,
        );
        require_unchanged_protected_inputs(&setup, &unrelated).unwrap();
    }

    #[test]
    fn preparation_reserves_complete_context_and_rejects_underfunded_prices() {
        let (_dir, project) = super::super::tests::project();
        let configuration = funded(&project, Digest::of(b"image"), "/bin/true");
        let mut setup = project.load_setup(&configuration.setup_id).unwrap();
        let rates = ModelRates {
            input: 1_000_000,
            cached_input: 2_000_000,
            output: 3_000_000,
        };
        setup.worker.cost_reservation = 0;
        reserve_provider_limits(&mut setup.worker, &rates, &setup.budgets).unwrap();
        assert!(
            setup.worker.token_reservation
                > setup.worker.context_bytes as u64 + setup.worker.output_tokens
        );
        assert!(
            setup.worker.cost_reservation
                >= 2 * setup.worker.context_bytes as u64 + 3 * setup.worker.output_tokens
        );
        setup.budgets.insert("cost_microunits".into(), 0);
        assert!(
            reserve_provider_limits(&mut setup.worker, &rates, &setup.budgets)
                .unwrap_err()
                .message
                .contains("cost_microunits")
        );
        setup.budgets.insert("cost_microunits".into(), 1_000_000);
        setup
            .budgets
            .insert("tokens".into(), setup.worker.output_tokens);
        assert!(
            reserve_provider_limits(&mut setup.worker, &rates, &setup.budgets)
                .unwrap_err()
                .message
                .contains("tokens")
        );
    }

    #[tokio::test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native Rust toolchain image"]
    async fn native_plan_implementation_calls_the_selected_model_and_preserves_dirty_source() {
        use tokio::io::AsyncWriteExt;
        assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let executable =
            std::env::var("ROVIK_HARNESS_DOCKER_BIN").unwrap_or_else(|_| "/usr/bin/docker".into());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = seen.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (path, body) = crate::testing::read_request(&mut socket).await;
                assert_eq!(path, "/v1/chat/completions");
                assert_eq!(body["model"], "fixture-scripted");
                assert!(body.get("tools").is_none());
                let request: rovik_harness::provider::ModelRequest =
                    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                assert_eq!(request.role, Role::Implementer);
                let output = json!({
                    "schema_version":1,"request_id":request.request_id,"tool_requests":[],"questions":[],
                    "proposal":{
                        "base_digest":request.snapshot_digest,
                        "changes":[{"path":"implementations/quota/lib.rs","content":rovik_harness::fixtures::QUOTA_RUST,"mode":33188}],
                        "summary":"Restore exact-capacity reservation without changing existing notes.",
                        "requirement_mapping":{"Q-1":["implementations/quota/lib.rs"],"Q-2":["implementations/quota/lib.rs"],"Q-3":["implementations/quota/lib.rs"]}
                    }
                });
                observed.lock().unwrap().push(body);
                let chunk = json!({"choices":[{"delta":{"content":output.to_string()},"finish_reason":null}]});
                let finish = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":100}});
                let stream = format!("data: {chunk}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{stream}", stream.len());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let (_directory, project) = super::super::tests::project();
        let config = funded(&project, image, &executable);
        project
            .dispatch(Operation::Configure {
                setup_id: config.setup_id.to_string(),
                engine: config.engine,
                expected_snapshot_digest: config.snapshot_digest.to_string(),
            })
            .unwrap();
        std::fs::write(
            project.root.join("notes.txt"),
            "Existing unsaved-to-Git user work must survive.\n",
        )
        .unwrap();
        std::fs::write(
            project.root.join("plan.md"),
            "# Reviewed plan\nRepair exact-capacity reservation and preserve the existing notes.\n",
        )
        .unwrap();
        let source_before =
            std::fs::read(project.root.join("implementations/quota/lib.rs")).unwrap();
        let index_before = std::fs::read(project.root.join(".git/index")).unwrap();
        let notes_before = std::fs::read(project.root.join("notes.txt")).unwrap();
        let snapshot = project
            .capture(&["notes.txt".into()].into())
            .unwrap()
            .digest()
            .unwrap();
        let saved = project.read_plan("plan.md").unwrap();
        let mut selection = selected("ephemeral-native-test-key");
        selection.profile.base_url = endpoint;
        selection.rates = ModelRates {
            input: 1_000_000,
            cached_input: 1_000_000,
            output: 2_000_000,
        };
        let intake = project
            .dispatch(Operation::ImplementPlan {
                plan_path: "plan.md".into(),
                expected_plan_digest: saved.digest.to_string(),
                expected_snapshot_digest: snapshot.to_string(),
                untracked: vec!["notes.txt".into()],
                saved_buffers: true,
                plan_criteria_reviewed: true,
                provider: selection.clone(),
            })
            .unwrap();
        assert_eq!(intake["kind"], "implementation");
        assert_eq!(intake["status"], "blocked");
        assert!(
            seen.lock().unwrap().is_empty(),
            "preparing a task must not call the model before approvals"
        );
        let run_id = intake["runId"].as_str().unwrap().to_string();
        for approval in intake["pendingApprovals"].as_array().unwrap() {
            project
                .dispatch(Operation::Approve {
                    run_id: run_id.clone(),
                    request: approval["request"].clone(),
                })
                .unwrap();
        }
        project
            .dispatch(Operation::ResumeImplementation {
                run_id: run_id.clone(),
                provider: selection,
            })
            .unwrap();
        let started = std::time::Instant::now();
        while project.is_running(&run_id).unwrap() {
            assert!(started.elapsed() < std::time::Duration::from_secs(180));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let status = project.status(&run_id).unwrap();
        assert_eq!(status["status"], "verified", "{status:#}");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(status["budgets"]["model_calls"]["spent"], 1);
        assert_eq!(status["budgets"]["cost_microunits"]["spent"], 300);
        assert_eq!(
            std::fs::read(project.root.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            std::fs::read(project.root.join("implementations/quota/lib.rs")).unwrap(),
            source_before
        );
        assert_eq!(
            std::fs::read(project.root.join("notes.txt")).unwrap(),
            notes_before
        );
        let controller = project.controller(None).unwrap();
        let candidate = controller.current_candidate(&run_id).unwrap().unwrap();
        assert_eq!(
            controller
                .ledger
                .read(
                    &candidate.files["implementations/quota/lib.rs"]
                        .content
                        .digest
                )
                .unwrap(),
            rovik_harness::fixtures::QUOTA_RUST.as_bytes()
        );
        assert_eq!(
            controller
                .ledger
                .read(&candidate.files["notes.txt"].content.digest)
                .unwrap(),
            notes_before
        );
        let review = project.review_changes(&run_id).unwrap();
        assert_eq!(review["applied"], false);
        assert_eq!(review["files"].as_array().unwrap().len(), 1);
        server.abort();
    }
}
