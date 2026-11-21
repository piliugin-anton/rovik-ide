//! Advisory editor feedback from protected policy and authenticated native observations.
use super::*;
use rovik_harness::{architecture, identity::EvidenceKey};

impl IdeProject {
    pub(super) fn architecture_feedback(&self, run_id: &str) -> Result<Value> {
        let input = self.run_input(run_id)?;
        let controller = self.controller(None)?;
        let setup = controller.setup(run_id)?;
        let policies = architecture::protected_inputs(&controller.ledger, &setup)?;
        let approved = architecture::oracle_approved(&controller.ledger, &setup)?;
        let mut diagnostics = Vec::new();
        let candidate = controller.current_candidate(run_id)?;
        let candidate_digest = candidate.as_ref().map(Snapshot::digest).transpose()?;
        // A current authenticated application receipt bridges the private
        // candidate's rebased tombstones to the actual saved Git worktree.
        // Its reader rechecks every candidate byte/mode and current authority.
        let application_binding = self.current_application_binding(run_id)?;
        if let Some((_, applied_candidate, _)) = &application_binding {
            check(
                candidate_digest.as_ref() == Some(applied_candidate),
                "Application receipt belongs to a different architecture candidate.",
            )?;
        }
        let selection = if application_binding.is_some() {
            candidate
                .as_ref()
                .ok_or_else(|| Error::new("Applied candidate is unavailable.".into()))?
                .files
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
        } else {
            input.untracked.clone()
        };
        let current = match self.observe_snapshot(&selection, None) {
            Ok(snapshot) => Some(snapshot.digest()?),
            Err(error) => {
                diagnostics.push(error.message);
                None
            }
        };
        let configuration_current = self
            .configuration()?
            .map(|config| Digest::of_json(&config))
            .transpose()?
            .as_ref()
            == Some(&input.configuration_digest);
        let plan_current = match &input.plan {
            None => true,
            Some(binding) => match self.read_plan(&binding.path) {
                Ok(plan) => plan.digest == binding.digest,
                Err(error) => {
                    diagnostics.push(error.message);
                    false
                }
            },
        };
        let mut observations = Vec::new();
        if let (Some(candidate), Some(binding)) =
            (&candidate, controller.ledger.run(run_id)?.binding_digest)
        {
            let key: EvidenceKey = wire::decode(&controller.ledger.read(&binding)?)?;
            check(
                key.candidate_digest == candidate.digest()?
                    && key.contract_digest == Digest::of_json(&setup.contract)?
                    && key.design_digest == Digest::of_json(&setup.design)?
                    && key.policy_digest == Digest::of_json(&setup.policy)?
                    && key.oracle_digest == setup.oracle.digest()?
                    && key.plan_digest == Digest::of_json(&setup.plan)?
                    && key.environment_digest == Digest::of_json(&setup.environment)?,
                "Architecture evidence is bound to different task inputs. Run verification again.",
            )?;
            let graphs = architecture::observed_graphs(&controller.ledger, run_id, &key)?;
            for observed in graphs {
                let Some(policy) = policies.iter().find(|policy| {
                    policy.check_id == observed.evidence.check_id
                        && policy.configuration_digest
                            == observed.evidence.graph.configuration_digest
                }) else {
                    continue;
                };
                check(
                    observed.result.checker_digest == policy.checker_digest,
                    "Architecture evidence uses a different registered checker. Run verification again.",
                )?;
                // Receipt attribution does not replace collector validation. This
                // also protects feedback loaded from a previous controller release.
                observed.evidence.graph.validate_collection(&policy.input)?;
                let findings = observed
                    .evidence
                    .graph
                    .architecture_findings(&policy.input)?;
                check(
                    (observed.result.status == rovik_harness::evidence::Status::Pass)
                        == findings.is_empty(),
                    "Architecture findings disagree with the authenticated check result.",
                )?;
                let findings = findings.into_iter().map(|finding| {
                    let locations: BTreeMap<_, _> = finding.path.iter().filter_map(|id| {
                        let node = observed.evidence.graph.nodes.iter().find(|node| &node.id == id)?;
                        let path = node.path.as_ref()?;
                        // Package manifests are useful locations too. Component
                        // cycles and external packages may have no source location.
                        let file = candidate.files.get(path)?;
                        Some((path.clone(), file.content.digest.clone()))
                    }).collect();
                    json!({"ruleId":finding.rule_id,"message":finding.message,"path":finding.path,
                        "locations":locations.into_iter().map(|(path,content_digest)|json!({"path":path,"contentDigest":content_digest})).collect::<Vec<_>>()})
                }).collect::<Vec<_>>();
                observations.push(json!({"checkId":observed.result.check_id,"configurationDigest":observed.result.configuration_digest,
                    "resultId":observed.result.result_id,"receiptId":observed.result.runner_receipt_id,"graphDigest":observed.graph_digest,
                    "status":observed.result.status,"precision":observed.evidence.graph.precision,"findings":findings}));
            }
            // Surface collector errors instead of rendering an empty findings
            // list as a claim that architecture passed.
            for result in architecture::feedback_results(&controller.ledger, run_id, &key)? {
                if policies.iter().any(|policy| {
                    policy.check_id == result.check_id
                        && policy.configuration_digest == result.configuration_digest
                }) && !observations
                    .iter()
                    .any(|row| row["resultId"].as_str() == Some(result.result_id.as_str()))
                {
                    if result.diagnostics.is_empty() {
                        diagnostics.push(format!(
                            "{}: no complete architecture graph is available ({:?}).",
                            result.check_id, result.status
                        ));
                    } else {
                        diagnostics.extend(result.diagnostics.into_iter().map(|diagnostic| {
                            format!("{}: {}", result.check_id, diagnostic.message)
                        }));
                    }
                }
            }
        }
        if policies.is_empty() {
            diagnostics.push("No architecture policy is registered for this run. Prepare and approve configured architecture checks first.".into());
        }
        if !approved {
            diagnostics
                .push("The protected architecture policy needs current oracle approval.".into());
        }
        if !configuration_current || !plan_current {
            diagnostics.push("The reviewed configuration or saved plan changed. Refresh verification before using editor findings.".into());
        }
        let source_current = candidate_digest.is_some()
            && (current == candidate_digest
                || application_binding
                    .as_ref()
                    .is_some_and(|(_, applied_candidate, observed)| {
                        candidate_digest.as_ref() == Some(applied_candidate)
                            && current.as_ref() == Some(observed)
                    }));
        if !source_current {
            diagnostics.push("The observed candidate differs from the editor's saved source. Findings are historical review information.".into());
        }
        if observations.is_empty() {
            diagnostics.push("No completed configured architecture observation is available. Run the approved checks to collect evidence.".into());
        }
        let freshness = if !configuration_current || !plan_current || !source_current {
            "stale"
        } else if !approved || observations.is_empty() {
            "unavailable"
        } else {
            "current"
        };
        let policies = policies.into_iter().map(|policy| -> Result<Value> {
            Ok(json!({"checkId":policy.check_id,"configurationDigest":policy.configuration_digest,"requestDigest":policy.request_digest,
                "profileId":policy.input.profile.id,"language":policy.input.profile.language,
                "architecturePolicyDigest":policy.input.architecture.as_ref().map(Digest::of_json).transpose()?,
                "architecture":policy.input.architecture,"forbiddenEdges":policy.input.forbidden_edges}))
        }).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"runId":run_id,"projectId":self.repository_id,"workspace":self.root,
            "currentSnapshotDigest":current,"evidenceCandidateDigest":candidate_digest,"configurationDigest":input.configuration_digest,
            "applicationBinding":application_binding.map(|(receipt_digest,candidate_digest,observed_snapshot_digest)|
                json!({"receiptDigest":receipt_digest,"candidateDigest":candidate_digest,"observedSnapshotDigest":observed_snapshot_digest})),
            "oracleDigest":setup.oracle.digest()?,"policyApproved":approved,"freshness":freshness,
            "policies":policies,"observations":observations,"diagnostics":diagnostics}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> (tempfile::TempDir, IdeProject, String) {
        let (directory, project) = super::super::tests::project();
        let config =
            super::super::tests::prepared(&project, Digest::of(b"image"), "/missing/docker");
        let mut setup = project.load_setup(&config.setup_id).unwrap();
        setup.run_id = "architecture-feedback".into();
        setup.plan.run_id = setup.run_id.clone();
        let mut controller = project.controller(None).unwrap();
        let now = runner::now_ms().unwrap();
        // These read-only fixtures stay at intake without registering an
        // execution backend or granting any acceptance authority.
        setup.validate(&controller.ledger).unwrap();
        let intake = controller
            .ledger
            .store_json("operation", &setup, now)
            .unwrap();
        controller
            .ledger
            .store_json("snapshot", &setup.base, now)
            .unwrap();
        let run = controller
            .ledger
            .create_run(&setup.run_id, &intake.digest, &setup.budgets, now)
            .unwrap();
        let key = EvidenceKey {
            verification_nonce: None,
            candidate_digest: setup.base.digest().unwrap(),
            contract_digest: Digest::of_json(&setup.contract).unwrap(),
            design_digest: Digest::of_json(&setup.design).unwrap(),
            policy_digest: Digest::of_json(&setup.policy).unwrap(),
            oracle_digest: setup.oracle.digest().unwrap(),
            plan_digest: Digest::of_json(&setup.plan).unwrap(),
            environment_digest: Digest::of_json(&setup.environment).unwrap(),
        };
        let binding = controller
            .ledger
            .store_json("operation", &key, now)
            .unwrap();
        controller.ledger.bind(&run, &binding.digest, now).unwrap();
        let input = RunInput {
            plan: None,
            execution_root: project.root.clone(),
            baseline_reference: None,
            tracked_paths: None,
            configuration_digest: Digest::of_json(&config).unwrap(),
            snapshot_digest: config.snapshot_digest.clone(),
            configuration: config.clone(),
            untracked: BTreeSet::new(),
        };
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO configuration(singleton,value) VALUES(1,?1)",
                [serde_json::to_string(&config).unwrap()],
            )
            .unwrap();
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO runs(id,input,created_ms) VALUES(?1,?2,?3)",
                params![
                    setup.run_id,
                    serde_json::to_string(&input).unwrap(),
                    now as i64
                ],
            )
            .unwrap();
        (directory, project, setup.run_id)
    }

    #[test]
    fn feedback_requires_authentic_observations_and_never_advances_the_run() {
        let (_directory, project, run_id) = registered();
        let before = project.ledger().unwrap().run(&run_id).unwrap();
        let value = project.architecture_feedback(&run_id).unwrap();
        assert_eq!(value["freshness"], "unavailable");
        assert_eq!(value["policyApproved"], false);
        assert!(value["observations"].as_array().unwrap().is_empty());
        assert!(!value["diagnostics"].as_array().unwrap().is_empty());
        assert_eq!(
            project.ledger().unwrap().run(&run_id).unwrap().revision,
            before.revision
        );
        assert_eq!(
            project.ledger().unwrap().run(&run_id).unwrap().state,
            before.state
        );
    }

    #[test]
    fn editor_source_or_configuration_changes_make_feedback_stale() {
        let (_directory, project, run_id) = registered();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            "edited after observation\n",
        )
        .unwrap();
        let edited = project.architecture_feedback(&run_id).unwrap();
        assert_eq!(edited["freshness"], "stale");
        assert_ne!(
            edited["currentSnapshotDigest"],
            edited["evidenceCandidateDigest"]
        );
        let controller = project.controller(None).unwrap();
        let candidate = controller.current_candidate(&run_id).unwrap().unwrap();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            controller
                .ledger
                .read(
                    &candidate.files["implementations/quota/lib.rs"]
                        .content
                        .digest,
                )
                .unwrap(),
        )
        .unwrap();
        project
            .database()
            .unwrap()
            .execute("DELETE FROM configuration", [])
            .unwrap();
        let changed = project.architecture_feedback(&run_id).unwrap();
        assert_eq!(
            changed["currentSnapshotDigest"],
            changed["evidenceCandidateDigest"]
        );
        assert_eq!(changed["freshness"], "stale");
    }

    #[test]
    fn another_execution_workspace_cannot_read_architecture_evidence() {
        let (directory, mut project, run_id) = registered();
        project.root = directory.path().join("another-chat");
        let error = project.architecture_feedback(&run_id).unwrap_err();
        assert!(error.message.contains("execution workspace"), "{error:?}");
    }

    #[test]
    fn a_private_implementation_candidate_does_not_mark_unchanged_editor_files_current() {
        let (_directory, project, run_id) = registered();
        let mut controller = project.controller(None).unwrap();
        let mut candidate = controller.current_candidate(&run_id).unwrap().unwrap();
        let now = runner::now_ms().unwrap();
        candidate
            .files
            .get_mut("implementations/quota/lib.rs")
            .unwrap()
            .content = controller
            .ledger
            .store(
                "source",
                "application/octet-stream",
                b"private candidate changes\n",
                now,
            )
            .unwrap();
        let stored = controller
            .ledger
            .store_json("snapshot", &candidate, now)
            .unwrap();
        let run = controller.ledger.run(&run_id).unwrap();
        let mut key: EvidenceKey = wire::decode(
            &controller
                .ledger
                .read(run.binding_digest.as_ref().unwrap())
                .unwrap(),
        )
        .unwrap();
        key.candidate_digest = stored.digest;
        let binding = controller
            .ledger
            .store_json("operation", &key, now)
            .unwrap();
        controller.ledger.bind(&run, &binding.digest, now).unwrap();
        let value = project.architecture_feedback(&run_id).unwrap();
        assert_eq!(
            value["currentSnapshotDigest"],
            project
                .run_input(&run_id)
                .unwrap()
                .snapshot_digest
                .to_string()
        );
        assert_ne!(
            value["currentSnapshotDigest"],
            value["evidenceCandidateDigest"]
        );
        assert_eq!(value["freshness"], "stale");
    }
}
