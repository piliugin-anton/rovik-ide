//! Protected Node.js, TypeScript and Python test requests and independent validation.
use crate::{
    identity::Digest,
    normalizer::{Inventory, Normalized},
    pack_protocol::{self, Operation, Request},
    profile::{Configuration, Language, Profile},
    require, wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One approved runtime check. All test and compiler configuration paths name oracle assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestInput {
    /// Logical snapshot mount, never an arbitrary host path.
    pub snapshot_handle: String,
    /// Exact language and toolchain policy.
    pub profile: Profile,
    /// Selected member of the approved configuration matrix.
    pub configuration_id: String,
    /// Relative paths in the protected oracle, passed without shell expansion.
    pub test_files: Vec<String>,
    /// Protected compiler project, required only for TypeScript.
    #[serde(default)]
    pub typescript_project: Option<String>,
    /// Cases which must remain present in the native report.
    pub required_test_ids: BTreeSet<String>,
}
impl TestInput {
    /// Validate the supported, explicit runtime policy before launching tools.
    pub fn validate(&self) -> Result<&Configuration> {
        self.profile.require_resolved()?;
        require(
            self.snapshot_handle == "snapshot:active",
            "unknown runtime snapshot capability",
        )?;
        let config = self
            .profile
            .configurations
            .iter()
            .find(|c| c.id == self.configuration_id)
            .ok_or_else(|| Error("unknown runtime configuration".into()))?;
        let language = self.profile.language;
        require(
            matches!(
                language,
                Language::JavaScript | Language::TypeScript | Language::Python
            ),
            "not a supported runtime language",
        )?;
        require(
            config.target == "linux-x86_64"
                && config.features.is_empty()
                && !config.default_features
                && config.build_targets == ["protected-tests"],
            "unsupported runtime target, features or build targets",
        )?;
        let facts = &self.profile.facts;
        let expected: &[(&str, &str)] = if language == Language::Python {
            &[
                ("test_runner", "pytest"),
                ("dependency_policy", "hashed-wheels-offline"),
                ("import_policy", "isolated-pytest"),
            ]
        } else {
            &[
                ("test_runner", "node:test"),
                ("dependency_policy", "npm-ci-offline-no-scripts"),
                ("module_system", "esm"),
            ]
        };
        require(
            expected
                .iter()
                .all(|(key, value)| facts.get(*key).map(String::as_str) == Some(*value)),
            "unsupported runtime policy facts",
        )?;
        require(
            config.runtime
                == if language == Language::Python {
                    "cpython"
                } else {
                    "node"
                },
            "runtime does not match language",
        )?;
        require(
            !self.test_files.is_empty() && !self.required_test_ids.is_empty(),
            "runtime check needs protected tests and required cases",
        )?;
        crate::unique(self.test_files.iter().map(String::as_str))?;
        for path in &self.test_files {
            crate::scope::validate_path(path)?;
            require(
                path.ends_with(if language == Language::Python {
                    ".py"
                } else {
                    ".mjs"
                }),
                "unsupported protected test file extension",
            )?;
        }
        for id in &self.required_test_ids {
            crate::id(id)?;
        }
        if language == Language::TypeScript {
            require(
                config.flags == ["--strict", "--noEmitOnError"],
                "TypeScript requires strict checking and no emit on error",
            )?;
            crate::scope::validate_path(
                self.typescript_project
                    .as_deref()
                    .ok_or_else(|| Error("TypeScript needs a protected compiler project".into()))?,
            )?;
        } else {
            require(
                config.flags.is_empty() && self.typescript_project.is_none(),
                "unexpected runtime compiler configuration",
            )?;
        }
        for key in version_keys(language) {
            let version = &facts[*key];
            require(
                version.split('.').count() == 3
                    && version
                        .split('.')
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
                format!("{key} must be an exact release version"),
            )?;
        }
        Ok(config)
    }
}

pub(crate) fn version_keys(language: Language) -> &'static [&'static str] {
    match language {
        Language::JavaScript => &["node_version", "npm_version"],
        Language::TypeScript => &["node_version", "npm_version", "typescript_version"],
        Language::Python => &["python_version", "pytest_version"],
        _ => &[],
    }
}

/// Native runner output, parsed again by the controller from the raw report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestOutput {
    /// Observed pack family.
    pub language: Language,
    /// Exact configuration that was executed.
    pub configuration_digest: Digest,
    /// Versions independently queried from the installed tools.
    pub versions: BTreeMap<String, String>,
    /// Native test process status, restricted to completed success or test failure.
    pub exit_code: i32,
    /// Complete native JUnit report; no model-supplied inventory is admitted.
    pub report: String,
}

/// Decode a closed, resolved runtime request.
pub fn input(request: &Request) -> Result<TestInput> {
    require(
        request.protocol_version == 1 && request.operation == Operation::Test,
        "expected runtime test protocol v1",
    )?;
    crate::id(&request.request_id)?;
    let input: TestInput = wire::decode(&wire::canonical(&request.payload)?)?;
    input.validate()?;
    Ok(input)
}

/// Bind runtime evidence to the protected request, versions, configuration and case inventory.
pub fn verify(
    request: &Request,
    bytes: &[u8],
    configuration: &Digest,
    exit: Option<i32>,
) -> Result<Normalized> {
    let input = input(request)?;
    let reply = pack_protocol::response(request, bytes)?;
    require(
        reply.ok,
        reply
            .error
            .as_ref()
            .map(|e| e.message.as_str())
            .unwrap_or("runtime adapter failed"),
    )?;
    let result: TestOutput = wire::decode(&wire::canonical(&reply.result)?)?;
    require(
        result.language == input.profile.language
            && result.configuration_digest == *configuration
            && input.validate()?.digest()? == *configuration,
        "runtime result configuration mismatch",
    )?;
    let versions: BTreeMap<String, String> = version_keys(result.language)
        .iter()
        .map(|key| (key.to_string(), input.profile.facts[*key].clone()))
        .collect();
    require(
        result.versions == versions,
        "runtime result tool versions mismatch",
    )?;
    require(
        matches!(result.exit_code, 0 | 1) && exit == Some(result.exit_code),
        "incomplete runtime execution or mismatched exit status",
    )?;
    let inventory: Inventory = if result.language == Language::Python {
        crate::normalizer::parse_junit(result.report.as_bytes())?
    } else {
        crate::node_report::parse(result.report.as_bytes())?
    };
    inventory.validate()?;
    require(
        inventory.executed() > 0
            && input
                .required_test_ids
                .iter()
                .all(|id| inventory.discovered.contains(id)),
        "zero tests or required runtime case disappeared",
    )?;
    require(
        (result.exit_code == 0) == inventory.failed.is_empty(),
        "runtime exit status disagrees with cases",
    )?;
    Ok(Normalized {
        status: if inventory.failed.is_empty() {
            crate::evidence::Status::Pass
        } else {
            crate::evidence::Status::Fail
        },
        inventory,
        diagnostic: None,
    })
}
