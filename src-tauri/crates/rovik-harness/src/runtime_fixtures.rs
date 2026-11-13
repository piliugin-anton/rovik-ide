//! Illustrative runtime fixtures exercise the same controller and acceptance contract.
use crate::{
    controller::TaskSetup,
    fixtures,
    identity::Digest,
    ledger::Ledger,
    pack_protocol::{Operation, Request},
    profile::Language,
    runner::now_ms,
    runtime::TestInput,
    snapshot::FileEntry,
    wire, Error, Result,
};

/// Correct reference source and its candidate path for a runtime language.
pub fn runtime_quota_source(language: Language) -> Result<(&'static str, &'static str)> {
    match language {
        Language::JavaScript => Ok((
            "implementations/quota/quota.mjs",
            include_str!("../fixtures/runtime/javascript/quota.mjs"),
        )),
        Language::TypeScript => Ok((
            "implementations/quota/quota.ts",
            include_str!("../fixtures/runtime/typescript/quota.ts"),
        )),
        Language::Python => Ok((
            "implementations/quota/quota.py",
            include_str!("../fixtures/runtime/python/quota.py"),
        )),
        _ => Err(Error::new("not a runtime quota fixture".into())),
    }
}

/// Prepare an unapproved task for the pinned runtime image, including real offline packages.
pub fn quota_runtime(
    ledger: &Ledger,
    run_id: &str,
    image: Digest,
    revision: &str,
    language: Language,
) -> Result<TaskSetup> {
    let (path, source) = runtime_quota_source(language)?;
    let python = language == Language::Python;
    let typescript = language == Language::TypeScript;
    let mut setup = fixtures::quota_rust(ledger, run_id, image, revision)?;
    let bad = source.replace("amount > remaining", "amount >= remaining");
    let mut files = vec![(path, bad.as_str())];
    if python {
        files.push((
            "requirements.lock",
            include_str!("../fixtures/runtime/requirements.lock"),
        ));
    } else {
        files.extend([
            (
                "package.json",
                include_str!("../fixtures/runtime/package.json"),
            ),
            (
                "package-lock.json",
                include_str!("../fixtures/runtime/package-lock.json"),
            ),
        ]);
    }
    setup.base = fixtures::snapshot(ledger, revision, &files)?;
    setup.negative_control = setup.base.clone();
    setup.dependencies = fixtures::snapshot(ledger, "offline-runtime-packages-v1", &[])?;
    let (dependency_path, dependency_bytes): (&str, &[u8]) = if python {
        (
            "wheels/fixture_identity-1.0.0-py3-none-any.whl",
            include_bytes!(
                "../fixtures/runtime/dependencies/fixture_identity-1.0.0-py3-none-any.whl"
            ),
        )
    } else {
        (
            "packages/fixture-identity-1.0.0.tgz",
            include_bytes!("../fixtures/runtime/dependencies/fixture-identity-1.0.0.tgz"),
        )
    };
    setup.dependencies.files.insert(
        dependency_path.into(),
        FileEntry {
            mode: 0o100644,
            content: ledger.store(
                "source",
                "application/octet-stream",
                dependency_bytes,
                now_ms()?,
            )?,
        },
    );
    setup.environment.dependency_digest = setup.dependencies.digest()?;
    let mut profile = setup.profiles[0].clone();
    let name = if python {
        "python"
    } else if typescript {
        "typescript"
    } else {
        "javascript"
    };
    profile.id = format!("quota-{name}-host");
    profile.language = language;
    profile.dependency_bundle = Some(setup.dependencies.digest()?);
    let facts: &[(&str, &str)] = if python {
        &[
            ("python_version", "3.11.2"),
            ("pytest_version", "7.2.1"),
            ("test_runner", "pytest"),
            ("dependency_policy", "hashed-wheels-offline"),
            ("import_policy", "isolated-pytest"),
        ]
    } else {
        &[
            ("node_version", "24.19.0"),
            ("npm_version", "11.17.0"),
            ("test_runner", "node:test"),
            ("dependency_policy", "npm-ci-offline-no-scripts"),
            ("module_system", "esm"),
        ]
    };
    profile.facts = facts
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if typescript {
        profile
            .facts
            .insert("typescript_version".into(), "7.0.2".into());
    }
    let config = &mut profile.configurations[0];
    config.id = format!("quota-{name}-tests");
    config.target = "linux-x86_64".into();
    config.runtime = if python { "cpython" } else { "node" }.into();
    config.build_targets = vec!["protected-tests".into()];
    config.flags = if typescript {
        vec!["--strict".into(), "--noEmitOnError".into()]
    } else {
        vec![]
    };
    let lock = ledger.store_json("environment", &serde_json::json!({"image":setup.environment.image_digest,"language":language,"facts":profile.facts}), now_ms()?)?;
    profile.toolchain_lock = Some(lock.digest.clone());
    setup.environment.toolchain_digest = lock.digest;
    let command = crate::packs::runtime_test_command(["quota".into()].into())?;
    profile.public_consumer_commands = vec![command.id.clone()];
    let cases: Vec<String> = fixtures::QUOTA_CASES
        .iter()
        .map(|case| {
            if python {
                format!("test_reserve[{case}]")
            } else {
                case.to_string()
            }
        })
        .collect();
    let input = TestInput {
        snapshot_handle: "snapshot:active".into(),
        profile: profile.clone(),
        configuration_id: profile.configurations[0].id.clone(),
        test_files: vec![if python {
            "tests/test_quota.py"
        } else {
            "tests/consumer.mjs"
        }
        .into()],
        typescript_project: typescript.then(|| "compiler.json".into()),
        required_test_ids: cases.iter().cloned().collect(),
    };
    input.validate()?;
    let request = Request {
        protocol_version: 1,
        request_id: "quota".into(),
        operation: Operation::Test,
        payload: serde_json::to_value(&input).map_err(|e| Error::new(e.to_string()))?,
    };
    let request = format!(
        "{}\n",
        String::from_utf8(wire::canonical(&request)?).map_err(|e| Error::new(e.to_string()))?
    );
    let consumer = if python {
        include_str!("../fixtures/runtime/test_quota.py").to_string()
    } else {
        include_str!("../fixtures/runtime/consumer.mjs").replace(
            "CANDIDATE_MODULE",
            if typescript {
                "/build/project/dist/implementations/quota/quota.js"
            } else {
                "/build/project/implementations/quota/quota.mjs"
            },
        )
    };
    let mut oracle = vec![
        ("requests/quota.json", request.as_str()),
        (input.test_files[0].as_str(), consumer.as_str()),
        (
            "cases.json",
            include_str!("../fixtures/reference/quota/acceptance/quota-cases.json"),
        ),
    ];
    if typescript {
        oracle.push(("compiler.json", r#"{"compilerOptions":{"module":"NodeNext","target":"ES2022","types":[],"skipLibCheck":false},"files":["/build/project/implementations/quota/quota.ts"]}"#));
    }
    setup.oracle = fixtures::snapshot(ledger, "protected-runtime-oracle-v1", &oracle)?;
    setup.registry.commands = vec![command.clone()];
    setup.policy.commands = [command.id.clone()].into();
    setup.contract.profile_ids = vec![profile.id.clone()];
    if python {
        setup.negative_control_cases = vec!["test_reserve[reserve-exact]".into()];
        for invariant in &mut setup.contract.invariants {
            invariant.case_ids = invariant
                .case_ids
                .iter()
                .map(|case| format!("test_reserve[{case}]"))
                .collect();
        }
    }
    setup.suites = [(
        "quota.reserve.cases.v1".into(),
        cases.iter().cloned().collect(),
    )]
    .into();
    setup.design.contract_digest = Digest::of_json(&setup.contract)?;
    setup.design.existing_code_considered[0].file = path.into();
    setup.design.existing_code_considered[0].symbol = "reserve".into();
    setup.design.existing_code_considered[0].source_revision = setup.base.digest()?;
    setup.design.public_api_changes =
        vec!["Preserve the reservation function and its exact integer result.".into()];
    setup.design.resource_contracts = vec!["Runtime-managed values; no external resources.".into()];
    setup.design.variation_decisions = vec![
        "Protected tests consume a pinned local package to verify offline dependency installation."
            .into(),
    ];
    let check = &mut setup.plan.checks[0];
    check.check_id = command.id.clone();
    check.command_id = command.id;
    check.checker_digest = command.checker_digest;
    check.validated_parameters = [("request_id".into(), "quota".into())].into();
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
