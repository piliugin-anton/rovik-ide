//! Architecture rules over declared ownership and honestly scoped native graphs.

use crate::{
    pack_protocol::{Graph, Node, Precision},
    require, scope, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A configured graph bound by the trusted runner to exact execution inputs.
/// The wrapper is useful only when referenced by a completed authenticated receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEvidence {
    /// Exact candidate, contract, policy, oracle, plan and environment.
    pub evidence_key: crate::identity::EvidenceKey,
    /// Trusted normalizer identity.
    pub checker_digest: crate::identity::Digest,
    /// Registered check which produced this graph.
    pub check_id: String,
    /// Configured relationships and their explicit precision limits.
    pub graph: Graph,
}

/// Read only graph artifacts actually attached to current, completed passing receipts.
pub fn admitted_graphs(
    ledger: &crate::ledger::Ledger,
    run_id: &str,
    key: &crate::identity::EvidenceKey,
) -> Result<Vec<GraphEvidence>> {
    let mut graphs = Vec::new();
    for result in ledger.completed_results(run_id, &crate::identity::Digest::of_json(key)?)? {
        if result.status != crate::evidence::Status::Pass {
            continue;
        }
        for digest in &result.evidence {
            if ledger.reference(digest)?.kind != "graph" {
                continue;
            }
            let graph: GraphEvidence = crate::wire::decode(&ledger.read(digest)?)?;
            require(
                graph.evidence_key == *key
                    && graph.checker_digest == result.checker_digest
                    && graph.check_id == result.check_id
                    && graph.graph.configuration_digest == result.configuration_digest,
                "graph attribution disagrees with authenticated evidence",
            )?;
            graphs.push(graph);
        }
    }
    Ok(graphs)
}

/// One configured architecture request, resolved only from protected planned assets.
#[derive(Debug, Clone)]
pub struct ProtectedArchitecture {
    /// Registered check receiving the observation.
    pub check_id: String,
    /// Trusted registered parser identity for this exact planned check.
    pub checker_digest: crate::identity::Digest,
    /// Exact target/flag/runtime configuration.
    pub configuration_digest: crate::identity::Digest,
    /// Exact protected request bytes.
    pub request_digest: crate::identity::Digest,
    /// Validated native collector inputs, including reviewed boundaries.
    pub input: crate::pack_protocol::NativeInput,
}

/// Resolve architecture only from registered checks and their immutable oracle requests.
pub fn protected_inputs(
    ledger: &crate::ledger::Ledger,
    setup: &crate::controller::TaskSetup,
) -> Result<Vec<ProtectedArchitecture>> {
    let mut policies = Vec::new();
    for check in &setup.plan.checks {
        let command = setup.registry.get(&check.command_id)?;
        if command.parser != crate::registry::Parser::PackProtocol {
            continue;
        }
        command.validate()?;
        let request_id = check
            .validated_parameters
            .get("request_id")
            .ok_or_else(|| {
                Error::new("configured check has no protected request identity".into())
            })?;
        crate::id(request_id)?;
        let source = setup
            .oracle
            .files
            .get(&format!("requests/{request_id}.json"))
            .ok_or_else(|| {
                Error::new("configured request is absent from the protected oracle".into())
            })?;
        ledger.verify_reference(&source.content)?;
        let request: crate::pack_protocol::Request =
            crate::wire::decode(&ledger.read(&source.content.digest)?)?;
        require(
            request.request_id == *request_id
                && request.operation == crate::pack_protocol::Operation::Index,
            "configured check must name its exact protected index request",
        )?;
        let input = request.native_input()?;
        let configuration = input
            .profile
            .configurations
            .iter()
            .find(|config| config.id == input.configuration_id)
            .ok_or_else(|| Error::new("unknown architecture configuration".into()))?;
        require(
            configuration.digest()? == check.configuration_digest
                && command.checker_digest == check.checker_digest,
            "architecture request disagrees with the registered check",
        )?;
        if input.architecture.is_some() || !input.forbidden_edges.is_empty() {
            policies.push(ProtectedArchitecture {
                check_id: check.check_id.clone(),
                checker_digest: check.checker_digest.clone(),
                configuration_digest: check.configuration_digest.clone(),
                request_digest: source.content.digest.clone(),
                input,
            });
        }
    }
    Ok(policies)
}

/// Query current approval of the exact protected assets, including expiry and revocation.
pub fn oracle_approved(
    ledger: &crate::ledger::Ledger,
    setup: &crate::controller::TaskSetup,
) -> Result<bool> {
    ledger.approval_current(
        &setup.run_id,
        &crate::plan::ApprovalRequirement {
            subject_digest: setup.oracle.digest()?,
            approval_type: "oracle".into(),
            obligation_ids: vec![],
        },
        &crate::identity::Digest::of_json(&setup.contract.scope)?,
        &crate::identity::Digest::of_json(&setup.policy)?,
        &setup.authority,
        crate::runner::now_ms()?,
    )
}

/// Exact approved architecture constraints enter mandatory context before any proposed edits.
pub fn policy_context(
    ledger: &crate::ledger::Ledger,
    setup: &crate::controller::TaskSetup,
    candidate: &crate::snapshot::Snapshot,
) -> Result<Vec<crate::context::ContextItem>> {
    let inputs = protected_inputs(ledger, setup)?;
    if inputs.is_empty() {
        return Ok(vec![]);
    }
    require(
        oracle_approved(ledger, setup)?,
        "architecture policy needs current approval of the protected oracle before model execution",
    )?;
    let revision = candidate.digest()?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for input in inputs {
        if !seen.insert(input.request_digest.clone()) {
            continue;
        }
        items.push(crate::context::ContextItem {
            item_id: format!("architecture:{}", input.request_digest.hex()),
            text: String::from_utf8(ledger.read(&input.request_digest)?)
                .map_err(|_| Error::new("protected architecture request is not UTF-8".into()))?,
            artifact_digest: input.request_digest,
            trust: crate::context::Trust::ApprovedSpec,
            source_revision: revision.clone(),
            mandatory: true,
        });
    }
    Ok(items)
}

/// An authenticated graph observation for explanation only, including architecture failures.
#[derive(Debug, Clone)]
pub struct ObservedGraph {
    /// Immutable graph artifact attached to the receipt.
    pub graph_digest: crate::identity::Digest,
    /// Completed native result; a failing result remains failing.
    pub result: crate::evidence::CheckResult,
    /// Exact collector output and input binding.
    pub evidence: GraphEvidence,
}

/// Read complete authenticated observations without granting them passing-check authority.
/// Failed collector, partial, stale-generation and control runs never provide these graphs.
pub fn observed_graphs(
    ledger: &crate::ledger::Ledger,
    run_id: &str,
    key: &crate::identity::EvidenceKey,
) -> Result<Vec<ObservedGraph>> {
    let mut graphs = Vec::new();
    for result in feedback_results(ledger, run_id, key)? {
        if !(result.status == crate::evidence::Status::Pass
            || (result.status == crate::evidence::Status::Fail
                && result.failure_category == Some(crate::FailureCategory::Implementation)))
        {
            continue;
        }
        for digest in &result.evidence {
            if ledger.reference(digest)?.kind != "graph" {
                continue;
            }
            let graph: GraphEvidence = crate::wire::decode(&ledger.read(digest)?)?;
            require(
                graph.evidence_key == *key
                    && graph.checker_digest == result.checker_digest
                    && graph.check_id == result.check_id
                    && graph.graph.configuration_digest == result.configuration_digest,
                "advisory graph attribution disagrees with authenticated evidence",
            )?;
            graphs.push(ObservedGraph {
                graph_digest: digest.clone(),
                result: result.clone(),
                evidence: graph,
            });
        }
    }
    Ok(graphs)
}

/// Completed authenticated candidate results for advisory diagnostics, including failed collectors.
/// Baseline/control observations and unfinished operations remain excluded.
pub fn feedback_results(
    ledger: &crate::ledger::Ledger,
    run_id: &str,
    key: &crate::identity::EvidenceKey,
) -> Result<Vec<crate::evidence::CheckResult>> {
    let controls: BTreeSet<_> = ledger
        .operations(run_id)?
        .into_iter()
        .filter(|operation| matches!(operation.kind.as_str(), "baseline" | "negative_control"))
        .map(|operation| operation.operation_id)
        .collect();
    let mut results = Vec::new();
    for result in ledger.completed_results(run_id, &crate::identity::Digest::of_json(key)?)? {
        require(
            result.evidence_key == *key,
            "advisory result has a different evidence binding",
        )?;
        if !controls.contains(&result.operation_id) {
            results.push(result);
        }
    }
    Ok(results)
}

/// A component's approved responsibility; names alone never establish a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Business decisions, optionally declared pure.
    Domain,
    /// Use-case coordination.
    Application,
    /// External effects and infrastructure.
    Adapter,
    /// Object construction and wiring.
    Composition,
    /// Published cross-component contracts.
    Contract,
    /// Test-only code.
    Test,
}

/// Approved exposure of a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Available to other contexts.
    Public,
    /// Available only within its bounded context.
    ContextPrivate,
    /// Available only within its architectural module.
    ModulePrivate,
}

/// Ownership mapping for native package, target, file or symbol identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Architectural module identity, independent of language-specific node names.
    pub id: String,
    /// Bounded context which owns its semantics.
    pub context_id: String,
    /// Native node IDs or repository-relative paths; every project node maps once.
    pub selectors: Vec<String>,
    /// Approved responsibility.
    pub role: Role,
    /// Approved public surface.
    pub visibility: Visibility,
    /// Pure domain components must not transitively depend on adapters.
    #[serde(default)]
    pub pure: bool,
}

/// An exact directed relationship, used for authorized deltas and cycles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Consumer component, or native node for external dependency rules.
    pub source: String,
    /// Provider component, or exact external package identity.
    pub target: String,
}

/// Protected architecture inputs. A worker cannot change this map or its exceptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitecturePolicy {
    /// Exactly 1.
    pub schema_version: u32,
    /// Complete project ownership map at the collector's declared granularity.
    pub components: Vec<Component>,
    /// Exact external edges observed in the approved baseline.
    #[serde(default)]
    pub baseline_external_edges: BTreeSet<Dependency>,
    /// Additional edges explicitly approved as dependency changes.
    #[serde(default)]
    pub approved_external_edges: BTreeSet<Dependency>,
    /// Exact module edges authorized to close an otherwise forbidden cycle.
    #[serde(default)]
    pub approved_cycle_edges: BTreeSet<Dependency>,
    /// Include native test-only dependencies when the task's scope requires them.
    #[serde(default)]
    pub include_test_dependencies: bool,
    /// Reject coarse graphs when the approved task requires symbol-level evidence.
    #[serde(default)]
    pub require_symbol_precision: bool,
}

/// A precise structural counterexample, with no assertion about unobserved runtime calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// Stable architecture rule ID.
    pub rule_id: String,
    /// Native dependency path which violated the rule.
    pub path: Vec<String>,
    /// Explanation tied to declared ownership.
    pub message: String,
}

fn external(node: &Node) -> bool {
    node.kind == "external" || (node.kind == "package" && node.path.is_none())
}

impl ArchitecturePolicy {
    /// Reject ambiguous ownership, undeclared exception endpoints and unsupported precision.
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema_version == 1,
            "unsupported architecture policy version",
        )?;
        require(
            !self.components.is_empty(),
            "architecture ownership map is empty",
        )?;
        crate::unique(
            self.components
                .iter()
                .map(|component| component.id.as_str()),
        )?;
        for component in &self.components {
            crate::id(&component.context_id)?;
            require(
                !component.selectors.is_empty(),
                "component has no selectors",
            )?;
            require(
                !component.pure || component.role == Role::Domain,
                "only domain components may claim purity",
            )?;
            for selector in &component.selectors {
                if selector.contains('*') {
                    scope::validate_pattern(selector)?;
                } else {
                    crate::id(selector)?;
                }
            }
        }
        for edge in &self.approved_cycle_edges {
            require(
                self.components.iter().any(|c| c.id == edge.source)
                    && self.components.iter().any(|c| c.id == edge.target)
                    && edge.source != edge.target,
                "cycle approval names unknown components",
            )?;
        }
        for edge in self
            .baseline_external_edges
            .iter()
            .chain(&self.approved_external_edges)
        {
            crate::id(&edge.source)?;
            crate::id(&edge.target)?;
        }
        Ok(())
    }

    fn owners<'a>(&'a self, graph: &'a Graph) -> Result<BTreeMap<&'a str, &'a Component>> {
        let mut owners = BTreeMap::new();
        for node in graph.nodes.iter().filter(|node| !external(node)) {
            let matches: Vec<_> = self
                .components
                .iter()
                .filter(|component| {
                    component.selectors.iter().any(|selector| {
                        scope::matches(selector, &node.id)
                            || node
                                .path
                                .as_ref()
                                .is_some_and(|path| scope::matches(selector, path))
                    })
                })
                .collect();
            require(
                matches.len() == 1,
                format!(
                    "native node {} needs exactly one architecture owner",
                    node.id
                ),
            )?;
            owners.insert(node.id.as_str(), matches[0]);
        }
        Ok(owners)
    }

    /// Enforce ARCH-001 through ARCH-005 using only collected relationships.
    /// Contract modules conservatively expose all their dependencies; narrower type
    /// claims require a semantic collector rather than treating imports as symbols.
    pub fn evaluate(&self, graph: &Graph) -> Result<Vec<Finding>> {
        self.validate()?;
        require(
            graph.precision != Precision::Lexical && graph.unresolved.is_empty(),
            "architecture needs complete configured evidence",
        )?;
        if self.require_symbol_precision && graph.precision != Precision::ClangSemantic {
            return Err(Error::classified(
                crate::FailureCategory::Capability,
                "symbol-level architecture needs a registered semantic collector",
            ));
        }
        let owners = self.owners(graph)?;
        let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut findings = Vec::new();
        let mut outgoing: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut modules: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for edge in &graph.edges {
            if !self.include_test_dependencies && matches!(edge.kind.as_str(), "dev" | "test") {
                continue;
            }
            let source = nodes
                .get(edge.source.as_str())
                .ok_or_else(|| Error::new("unknown architecture edge source".into()))?;
            let target = nodes
                .get(edge.target.as_str())
                .ok_or_else(|| Error::new("unknown architecture edge target".into()))?;
            outgoing
                .entry(&edge.source)
                .or_default()
                .insert(&edge.target);
            let Some(owner) = owners.get(source.id.as_str()) else {
                continue;
            };
            if external(target) {
                let dependency = Dependency {
                    source: source.id.clone(),
                    target: target.id.clone(),
                };
                if !self.baseline_external_edges.contains(&dependency)
                    && !self.approved_external_edges.contains(&dependency)
                {
                    findings.push(Finding {
                        rule_id: "ARCH-005".into(),
                        path: vec![source.id.clone(), target.id.clone()],
                        message: "new external dependency lacks approval".into(),
                    });
                }
                continue;
            }
            let target_owner = owners[target.id.as_str()];
            let private = match target_owner.visibility {
                Visibility::Public => false,
                Visibility::ContextPrivate => owner.context_id != target_owner.context_id,
                Visibility::ModulePrivate => owner.id != target_owner.id,
            };
            if private {
                findings.push(Finding {
                    rule_id: "ARCH-001".into(),
                    path: vec![source.id.clone(), target.id.clone()],
                    message: "dependency crosses a declared private boundary".into(),
                });
            }
            if owner.role == Role::Contract
                && owner.visibility == Visibility::Public
                && (graph.precision != Precision::ClangSemantic || edge.kind == "public_type")
                && target_owner.id != owner.id
                && target_owner.visibility != Visibility::Public
            {
                findings.push(Finding {
                    rule_id: "ARCH-004".into(),
                    path: vec![source.id.clone(), target.id.clone()],
                    message: "public contract depends on a private implementation".into(),
                });
            }
            if owner.id != target_owner.id {
                let dependency = Dependency {
                    source: owner.id.clone(),
                    target: target_owner.id.clone(),
                };
                if !self.approved_cycle_edges.contains(&dependency) {
                    modules
                        .entry(&owner.id)
                        .or_default()
                        .insert(&target_owner.id);
                }
            }
        }
        for (node, owner) in &owners {
            if owner.pure {
                if let Some(path) = reachable_path(node, &outgoing, |target| {
                    owners.get(target).is_some_and(|c| c.role == Role::Adapter)
                }) {
                    findings.push(Finding {
                        rule_id: "ARCH-002".into(),
                        path,
                        message: "pure domain transitively depends on an I/O adapter".into(),
                    });
                }
            }
        }
        for module in modules.keys() {
            if let Some(path) = reachable_path(module, &modules, |target| target == *module) {
                findings.push(Finding {
                    rule_id: "ARCH-003".into(),
                    path,
                    message: "architectural module dependency cycle lacks approval".into(),
                });
            }
        }
        Ok(findings)
    }
}

fn reachable_path(
    start: &str,
    edges: &BTreeMap<&str, BTreeSet<&str>>,
    predicate: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let mut pending = std::collections::VecDeque::from([(start, vec![start.to_string()])]);
    let mut visited = BTreeSet::new();
    while let Some((node, path)) = pending.pop_front() {
        if !visited.insert(node) {
            continue;
        }
        for target in edges.get(node).into_iter().flatten() {
            let mut next = path.clone();
            next.push(target.to_string());
            if predicate(target) {
                return Some(next);
            }
            pending.push_back((target, next));
        }
    }
    None
}

/// Conservative reverse dependency closure for fast checks and context retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impact {
    /// Native nodes which changed or consume changed nodes transitively.
    pub affected_nodes: BTreeSet<String>,
    /// A changed path was not mapped; fast checks must retain full coverage.
    pub requires_full_checks: bool,
}

/// Compute consumers from complete configured evidence. Final verification remains complete.
pub fn impact(graph: &Graph, paths: &BTreeSet<String>) -> Impact {
    let mut affected = BTreeSet::new();
    let mut full = !graph.unresolved.is_empty() || graph.precision == Precision::Lexical;
    for path in paths {
        let matches: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.path.as_ref() == Some(path)
                    || node.id == *path
                    || (node.kind == "package"
                        && node.path.as_ref().is_some_and(|manifest| {
                            let root = manifest
                                .rsplit_once('/')
                                .map(|(root, _)| root)
                                .unwrap_or("");
                            root.is_empty() || path.starts_with(&format!("{root}/"))
                        }))
            })
            .collect();
        full |= matches.is_empty();
        affected.extend(matches.into_iter().map(|node| node.id.clone()));
    }
    loop {
        let before = affected.len();
        for edge in &graph.edges {
            if affected.contains(&edge.target) {
                affected.insert(edge.source.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    Impact {
        affected_nodes: affected,
        requires_full_checks: full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::Digest, pack_protocol::Edge};

    fn fixture() -> (ArchitecturePolicy, Graph) {
        let components = [
            ("domain", "orders", Role::Domain, Visibility::Public, true),
            (
                "service",
                "orders",
                Role::Application,
                Visibility::Public,
                false,
            ),
            (
                "storage",
                "storage",
                Role::Adapter,
                Visibility::ContextPrivate,
                false,
            ),
            ("api", "orders", Role::Contract, Visibility::Public, false),
            (
                "detail",
                "orders",
                Role::Domain,
                Visibility::ModulePrivate,
                false,
            ),
        ]
        .into_iter()
        .map(|(id, context, role, visibility, pure)| Component {
            id: id.into(),
            context_id: context.into(),
            role,
            visibility,
            pure,
            selectors: vec![format!("{id}.ts")],
        })
        .collect::<Vec<_>>();
        let mut nodes = components
            .iter()
            .map(|component| Node {
                id: format!("{}.ts", component.id),
                path: Some(format!("{}.ts", component.id)),
                kind: "module".into(),
            })
            .collect::<Vec<_>>();
        nodes.push(Node {
            id: "npm:driver@1".into(),
            path: None,
            kind: "external".into(),
        });
        let graph = Graph {
            precision: Precision::ProjectImports,
            nodes,
            edges: vec![],
            unresolved: vec![],
            configuration_digest: Digest::of(b"config"),
            compiler_commands: vec![],
            test_inventory: vec![],
        };
        let policy = ArchitecturePolicy {
            schema_version: 1,
            components,
            baseline_external_edges: BTreeSet::new(),
            approved_external_edges: BTreeSet::new(),
            approved_cycle_edges: BTreeSet::new(),
            include_test_dependencies: false,
            require_symbol_precision: false,
        };
        (policy, graph)
    }
    fn edge(source: &str, target: &str) -> Edge {
        Edge {
            source: source.into(),
            target: target.into(),
            kind: "import".into(),
        }
    }

    #[test]
    fn ownership_purity_cycles_public_contracts_and_dependency_deltas_have_separate_findings() {
        let (policy, mut graph) = fixture();
        graph.edges = vec![
            edge("domain.ts", "service.ts"),
            edge("service.ts", "storage.ts"),
            edge("service.ts", "domain.ts"),
            edge("api.ts", "detail.ts"),
            edge("storage.ts", "npm:driver@1"),
        ];
        let findings = policy.evaluate(&graph).unwrap();
        let rules: BTreeSet<_> = findings
            .iter()
            .map(|finding| finding.rule_id.as_str())
            .collect();
        assert_eq!(
            rules,
            ["ARCH-001", "ARCH-002", "ARCH-003", "ARCH-004", "ARCH-005"].into()
        );
        assert!(findings.iter().any(|finding| finding.rule_id == "ARCH-002"
            && finding.path == ["domain.ts", "service.ts", "storage.ts"]));
    }

    #[test]
    fn approved_edges_are_exact_and_do_not_exempt_another_dependency_or_cycle() {
        let (mut policy, mut graph) = fixture();
        graph.edges = vec![
            edge("domain.ts", "service.ts"),
            edge("service.ts", "domain.ts"),
            edge("storage.ts", "npm:driver@1"),
        ];
        policy.approved_cycle_edges.insert(Dependency {
            source: "service".into(),
            target: "domain".into(),
        });
        policy.approved_external_edges.insert(Dependency {
            source: "storage.ts".into(),
            target: "npm:driver@1".into(),
        });
        assert!(policy.evaluate(&graph).unwrap().is_empty());
        graph.edges.push(edge("service.ts", "npm:driver@1"));
        assert_eq!(policy.evaluate(&graph).unwrap()[0].rule_id, "ARCH-005");
    }

    #[test]
    fn test_edges_are_distinct_and_ambiguous_or_incomplete_ownership_blocks() {
        let (mut policy, mut graph) = fixture();
        let mut dependency = edge("domain.ts", "storage.ts");
        dependency.kind = "dev".into();
        graph.edges.push(dependency);
        assert!(policy.evaluate(&graph).unwrap().is_empty());
        policy.include_test_dependencies = true;
        assert!(policy
            .evaluate(&graph)
            .unwrap()
            .iter()
            .any(|finding| finding.rule_id == "ARCH-002"));
        policy.components[0].selectors.push("service.ts".into());
        assert!(policy.evaluate(&graph).is_err());
        policy.components[0].selectors.pop();
        graph.unresolved.push("unknown dynamic import".into());
        assert!(policy.evaluate(&graph).is_err());
    }

    #[test]
    fn impact_includes_transitive_consumers_and_falls_back_for_unmapped_changes() {
        let (_, mut graph) = fixture();
        graph.edges = vec![
            edge("api.ts", "service.ts"),
            edge("service.ts", "domain.ts"),
        ];
        let changed = impact(&graph, &["domain.ts".into()].into());
        assert_eq!(
            changed.affected_nodes,
            ["api.ts".into(), "service.ts".into(), "domain.ts".into()].into()
        );
        assert!(!changed.requires_full_checks);
        assert!(impact(&graph, &["vite.config.ts".into()].into()).requires_full_checks);
    }
}
