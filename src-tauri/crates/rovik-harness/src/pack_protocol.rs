//! Versioned native-pack boundary. Tool output has no authority over the plan.
use crate::{
    identity::Digest,
    profile::{Language, Profile},
    require, wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One request, persisted in protected oracle assets before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Exactly 1.
    pub protocol_version: u32,
    /// Correlates exactly one response.
    pub request_id: String,
    /// Registered native operation.
    pub operation: Operation,
    /// Operation-specific closed schema, validated before use.
    pub payload: serde_json::Value,
}
/// Shared pack operations. Planning and explanations are advisory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Discover installed compiler capabilities.
    Probe,
    /// Collect configured dependencies and validate boundaries.
    Index,
    /// Run protected runtime tests and return their complete native report.
    Test,
    /// Execute a registered native workspace build or test operation.
    NativeCheck,
    /// Propose checks for independent core validation.
    PlanChecks,
    /// Attribute native diagnostics without changing their outcome.
    Explain,
}
/// Explicit dependency boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForbiddenEdge {
    /// Node which must exist in the configured graph.
    pub source: String,
    /// Forbidden reachable node, including transitive dependencies.
    pub target: String,
}
/// Input for discovery and configured indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeInput {
    /// Logical mount capability, never a host path.
    pub snapshot_handle: String,
    /// Complete approved language facts.
    pub profile: Profile,
    /// Selected member of the required matrix.
    pub configuration_id: String,
    /// Architecture rules owned by the core.
    pub forbidden_edges: Vec<ForbiddenEdge>,
    /// Reviewed ownership, privacy, cycles, purity and dependency-change policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<crate::architecture::ArchitecturePolicy>,
    /// Native tests which discovery must retain.
    #[serde(default)]
    pub required_test_ids: BTreeSet<String>,
    /// Protected compiler project for TypeScript import resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typescript_project: Option<String>,
}
impl Request {
    /// Reject protocol errors and unresolved configuration before spawning tools.
    pub fn native_input(&self) -> Result<NativeInput> {
        require(
            self.protocol_version == 1,
            "unsupported native pack protocol",
        )?;
        crate::id(&self.request_id)?;
        require(
            matches!(self.operation, Operation::Probe | Operation::Index),
            "operation cannot produce configured evidence",
        )?;
        let input: NativeInput = wire::decode(
            &serde_json::to_vec(&self.payload).map_err(|e| Error::new(e.to_string()))?,
        )?;
        require(
            input.snapshot_handle == "snapshot:active",
            "unknown snapshot capability",
        )?;
        input.profile.require_resolved()?;
        if let Some(policy) = &input.architecture {
            policy.validate()?;
        }
        require(
            input
                .profile
                .configurations
                .iter()
                .any(|c| c.id == input.configuration_id),
            "unknown pack configuration",
        )?;
        if crate::runtime::version_keys(input.profile.language).is_empty() {
            require(
                input.typescript_project.is_none(),
                "unexpected TypeScript project",
            )?;
        } else {
            crate::runtime::configuration(
                &input.profile,
                &input.configuration_id,
                input.typescript_project.as_deref(),
            )?;
        }
        Ok(input)
    }
}
/// Exactly one response to a bounded request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Exactly 1.
    pub protocol_version: u32,
    /// Echo of controller correlation ID.
    pub request_id: String,
    /// Transport-level success, independently validated by the core.
    pub ok: bool,
    /// Structured output on success only.
    pub result: Option<serde_json::Value>,
    /// Explicit failure on error only.
    pub error: Option<PackError>,
}
/// Unsupported configurations and missing tools are failures, never empty success.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackError {
    /// Stable diagnostic code.
    pub code: String,
    /// Native explanation.
    pub message: String,
    /// Advisory hint; retries still require controller budget.
    pub retryable: bool,
    /// Trusted pack-stage attribution; old reports default to infrastructure failures.
    #[serde(default)]
    pub category: crate::FailureCategory,
}
impl Response {
    /// Preserve the adapter's typed failure after correlation and envelope validation.
    pub fn require_success(&self) -> Result<()> {
        if self.ok {
            return Ok(());
        }
        let error = self
            .error
            .as_ref()
            .ok_or_else(|| Error::new("missing native failure".into()))?;
        crate::id(&error.code)?;
        require(
            !error.message.is_empty() && error.message.len() <= 65_536,
            "invalid native failure diagnostic",
        )?;
        Err(Error::classified(
            error.category,
            format!("{}: {}", error.code, error.message),
        ))
    }
}
/// Precision of graph evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// Full Cargo resolve graph, including enabled optional and build dependencies.
    CargoResolved,
    /// Fresh configured compiler dependencies and CMake codemodel.
    ConfiguredBuild,
    /// Configured CMake relationships plus libclang declaration and type references.
    ClangSemantic,
    /// Parsed project imports resolved against the prepared runtime. External modules are leaves.
    ProjectImports,
    /// Discovery only; cannot establish a boundary.
    Lexical,
}
/// Configured graph node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Unambiguous identity.
    pub id: String,
    /// Attributed source or manifest path if available.
    pub path: Option<String>,
    /// Package, target, source or header.
    pub kind: String,
}
/// One directed dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// Consumer identity.
    pub source: String,
    /// Dependency identity.
    pub target: String,
    /// Native edge category.
    pub kind: String,
}
/// Compiler invocation collected from a freshly generated compilation database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerCommand {
    /// Translation unit.
    pub file: String,
    /// Tokenized arguments; used to check effective instrumentation.
    pub argv: Vec<String>,
}
/// Configured graph data, never substituted with an extension-based index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    /// Evidence precision.
    pub precision: Precision,
    /// Complete discovered nodes.
    pub nodes: Vec<Node>,
    /// Configured directed edges.
    pub edges: Vec<Edge>,
    /// Any unresolved input blocks a graph assertion.
    pub unresolved: Vec<String>,
    /// Exact target, feature and flag selection.
    pub configuration_digest: Digest,
    /// Effective C/C++ compilation arguments.
    pub compiler_commands: Vec<CompilerCommand>,
    /// Cases discovered by the native runner.
    pub test_inventory: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexResult {
    language: Language,
    graph: Graph,
    #[serde(default)]
    versions: BTreeMap<String, String>,
}

/// Decode one correlated response. Extra objects, fields, versions and partial replies fail.
pub fn response(request: &Request, bytes: &[u8]) -> Result<Response> {
    require(bytes.ends_with(b"\n"), "truncated native-pack response")?;
    let response: Response = wire::decode(bytes)?;
    require(
        response.protocol_version == 1
            && request.protocol_version == 1
            && response.request_id == request.request_id,
        "native-pack response identity mismatch",
    )?;
    require(
        if response.ok {
            response.result.is_some() && response.error.is_none()
        } else {
            response.result.is_none() && response.error.is_some()
        },
        "inconsistent native-pack response envelope",
    )?;
    Ok(response)
}
/// Validate configured evidence against the protected request and check identity.
pub fn verify_index(request: &Request, bytes: &[u8], configuration: &Digest) -> Result<Graph> {
    let input = request.native_input()?;
    require(
        request.operation == Operation::Index,
        "advisory operation cannot authorize a check",
    )?;
    let reply = response(request, bytes)?;
    reply.require_success()?;
    let result: IndexResult =
        wire::decode(&serde_json::to_vec(&reply.result).map_err(|e| Error::new(e.to_string()))?)?;
    require(
        result.language == input.profile.language,
        "pack language mismatch",
    )?;
    let expected: BTreeMap<_, _> = crate::runtime::version_keys(input.profile.language)
        .iter()
        .map(|key| (key.to_string(), input.profile.facts[*key].clone()))
        .collect();
    require(
        result.versions == expected,
        "import graph tool versions mismatch",
    )?;
    let config = input
        .profile
        .configurations
        .iter()
        .find(|c| c.id == input.configuration_id)
        .ok_or_else(|| Error::new("unknown configuration".into()))?;
    require(
        &config.digest()? == configuration && &result.graph.configuration_digest == configuration,
        "graph configuration mismatch",
    )?;
    result.graph.validate(&input)?;
    Ok(result.graph)
}
impl Graph {
    /// Known precision, complete references, required discovery, flags and transitive boundaries.
    pub fn validate(&self, input: &NativeInput) -> Result<()> {
        require(self.unresolved.is_empty(), "unresolved configured graph")?;
        require(
            self.precision
                == match input.profile.language {
                    Language::Rust => Precision::CargoResolved,
                    Language::C | Language::Cpp
                        if input
                            .architecture
                            .as_ref()
                            .is_some_and(|policy| policy.require_symbol_precision) =>
                    {
                        Precision::ClangSemantic
                    }
                    Language::C | Language::Cpp => Precision::ConfiguredBuild,
                    Language::JavaScript | Language::TypeScript | Language::Python => {
                        Precision::ProjectImports
                    }
                },
            "graph precision is insufficient",
        )?;
        require(!self.nodes.is_empty(), "empty configured graph")?;
        crate::unique(self.nodes.iter().map(|n| n.id.as_str()))?;
        let nodes: BTreeSet<_> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        for edge in &self.edges {
            require(
                nodes.contains(edge.source.as_str()) && nodes.contains(edge.target.as_str()),
                "unresolved graph edge",
            )?;
        }
        if let Some(policy) = &input.architecture {
            let findings = policy.evaluate(self)?;
            if !findings.is_empty() {
                return Err(Error::classified(
                    crate::FailureCategory::Implementation,
                    serde_json::to_string(&findings).map_err(|e| Error::new(e.to_string()))?,
                ));
            }
        }
        let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for edge in &self.edges {
            outgoing.entry(&edge.source).or_default().push(&edge.target);
        }
        for rule in &input.forbidden_edges {
            for pattern in [&rule.source, &rule.target] {
                require(!pattern.is_empty(), "empty architecture boundary")?;
                if pattern.contains('*') {
                    require(
                        self.precision == Precision::ProjectImports,
                        "directory boundaries require project import evidence",
                    )?;
                    crate::scope::validate_pattern(pattern)?;
                }
            }
            let mut pending: Vec<_> = nodes
                .iter()
                .copied()
                .filter(|node| crate::scope::matches(&rule.source, node))
                .collect();
            require(
                !pending.is_empty(),
                "architecture boundary source disappeared",
            )?;
            let mut visited = BTreeSet::new();
            while let Some(node) = pending.pop() {
                if !visited.insert(node) {
                    continue;
                }
                for target in outgoing.get(node).into_iter().flatten() {
                    if crate::scope::matches(&rule.target, target) {
                        return Err(Error::classified(
                            crate::FailureCategory::Implementation,
                            format!(
                                "forbidden dependency: {} -> {} (observed {} -> {})",
                                rule.source, rule.target, node, target
                            ),
                        ));
                    }
                    pending.push(target);
                }
            }
        }
        crate::unique(self.test_inventory.iter().map(String::as_str))?;
        require(
            input
                .required_test_ids
                .iter()
                .all(|id| self.test_inventory.contains(id)),
            "required native test disappeared from discovery",
        )?;
        if matches!(input.profile.language, Language::C | Language::Cpp) {
            let config = input
                .profile
                .configurations
                .iter()
                .find(|c| c.id == input.configuration_id)
                .ok_or_else(|| Error::new("unknown configuration".into()))?;
            require(
                !self.compiler_commands.is_empty(),
                "configured graph lacks compiler invocations",
            )?;
            for command in &self.compiler_commands {
                require(
                    nodes.contains(command.file.as_str()),
                    "compiler command has no graph node",
                )?;
                let (compiler, standard) = if input.profile.language == Language::C {
                    ("/usr/bin/gcc", "-std=c17")
                } else {
                    ("/usr/bin/g++", "-std=c++23")
                };
                require(
                    command.argv.first().map(String::as_str) == Some(compiler)
                        && command
                            .argv
                            .iter()
                            .filter(|a| a.starts_with("-std="))
                            .collect::<Vec<_>>()
                            == vec![&standard.to_string()],
                    "configured compiler or language standard differs from profile",
                )?;
                require(
                    config.flags.iter().all(|flag| command.argv.contains(flag)),
                    "configured compiler flags were removed",
                )?;
                // A later disabling option can silently undo instrumentation even when the
                // requested positive flag remains in compile_commands.json.
                require(
                    !command.argv.iter().any(|a| {
                        a.starts_with("-fno-sanitize=") || a.starts_with("-fno-instrument")
                    }),
                    "instrumentation disabled by build configuration",
                )?;
            }
        }
        Ok(())
    }
}
