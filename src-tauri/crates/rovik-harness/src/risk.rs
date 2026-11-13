//! Conservative risk routing from immutable source changes and declared task risks.
//! Lexical signals can add obligations; their absence never certifies safety.
use crate::{
    identity::Digest,
    ledger::Ledger,
    plan::{CheckPlan, Phase},
    require,
    snapshot::Snapshot,
    Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Trusted routing rules, unavailable to model output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskPolicy {
    /// Additional required routes, keyed by detected or declared risk.
    pub routes: BTreeMap<String, RiskRoute>,
}
/// Evidence required for a risk, over every declared configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRoute {
    /// Required final command IDs with native behavioral inventory.
    pub commands: BTreeSet<String>,
    /// Exact commands for every configuration. Omission retains the requirement
    /// that every global command run in every configured target.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands_by_configuration: BTreeMap<Digest, BTreeSet<String>>,
    /// Candidate-bound authenticated semantic review types.
    pub reviews: BTreeSet<String>,
    /// Protected concurrency cases grouped by the property they exercise.
    /// Required groups are schedules, linearizability, cancellation and shutdown.
    /// This records tested schedules and never claims exhaustive race freedom.
    #[serde(default)]
    pub concurrency_cases: BTreeMap<String, BTreeSet<String>>,
}
impl RiskRoute {
    fn commands_for_configuration(&self, configuration: &Digest) -> Result<&BTreeSet<String>> {
        if self.commands_by_configuration.is_empty() {
            return Ok(&self.commands);
        }
        self.commands_by_configuration
            .get(configuration)
            .ok_or_else(|| crate::Error::new("risk route omits a required configuration".into()))
    }
}
/// Attributed risk assessment, recorded before verification and recomputed at the gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    /// Declared risks union all derived signals.
    pub risks: BTreeSet<String>,
    /// Files responsible for each derived risk.
    pub sources: BTreeMap<String, BTreeSet<String>>,
}
/// Inspect both removed and added text, so deleting cleanup cannot reduce obligations.
pub fn assess(
    ledger: &Ledger,
    base: &Snapshot,
    candidate: &Snapshot,
    declared: &[String],
) -> Result<Assessment> {
    assess_for_languages(ledger, base, candidate, declared, &[])
}

/// Attribute managed-language signals only when approved profiles establish that family.
/// Mixed native/runtime tasks retain the conservative native ownership scan.
pub fn assess_for_languages(
    ledger: &Ledger,
    base: &Snapshot,
    candidate: &Snapshot,
    declared: &[String],
    languages: &[crate::profile::Language],
) -> Result<Assessment> {
    use crate::profile::Language;
    let managed_profiles = !languages.is_empty()
        && languages.iter().all(|language| {
            matches!(
                language,
                Language::JavaScript | Language::TypeScript | Language::Python
            )
        });
    let mut assessment = Assessment {
        risks: declared.iter().cloned().collect(),
        sources: BTreeMap::new(),
    };
    let paths: BTreeSet<_> = base.files.keys().chain(candidate.files.keys()).collect();
    for path in paths {
        if base.files.get(path) == candidate.files.get(path) {
            continue;
        }
        let mut texts = vec![];
        for snapshot in [base, candidate] {
            if let Some(file) = snapshot.files.get(path) {
                if ledger.is_binary(&file.content.digest)? {
                    continue;
                }
                texts.push(
                    String::from_utf8_lossy(&ledger.read(&file.content.digest)?).into_owned(),
                );
            }
        }
        let text = texts.join("\n");
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        // Managed constructors do not have C++ new/delete ownership semantics.
        // This is lexical risk attribution only; extensions never grant tool support.
        let managed = managed_profiles
            && [".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx", ".py"]
                .iter()
                .any(|extension| path.ends_with(extension));
        for (risk, tokens) in [
            (
                "resource-lifetime",
                &[
                    "malloc(",
                    "calloc(",
                    "realloc(",
                    "free(",
                    "new ",
                    "delete ",
                    "Box::into_raw",
                    "Vec::from_raw_parts",
                ][..],
            ),
            (
                "ffi",
                &[
                    "extern \"C\"",
                    "extern \"system\"",
                    "#[repr(C)]",
                    "#[no_mangle]",
                    "dlopen(",
                ][..],
            ),
            (
                "unsafe",
                &["unsafe ", "unsafe{", "transmute", "from_raw_parts"][..],
            ),
            (
                "concurrency",
                &[
                    "std::thread",
                    "pthread_",
                    "std::atomic",
                    "Atomic",
                    "_Atomic",
                    "volatile ",
                    "spawn(",
                ][..],
            ),
        ] {
            if tokens.iter().any(|token| {
                !(managed && ["new ", "delete "].contains(token))
                    && (text.contains(token)
                        || (token.contains('(')
                            && compact.contains(
                                &token
                                    .chars()
                                    .filter(|c| !c.is_whitespace())
                                    .collect::<String>(),
                            )))
            }) {
                assessment.risks.insert(risk.into());
                assessment
                    .sources
                    .entry(risk.into())
                    .or_default()
                    .insert(path.clone());
            }
        }
        if managed {
            for (risk, tokens) in [
                (
                    "resource-lifetime",
                    &[
                        "open(",
                        "openSync(",
                        "createReadStream(",
                        "createWriteStream(",
                        "new WebSocket(",
                        "subprocess.Popen(",
                    ][..],
                ),
                (
                    "ffi",
                    &[
                        "import ctypes",
                        "from ctypes",
                        "import cffi",
                        "from cffi",
                        "ffi-napi",
                        "process.dlopen(",
                    ][..],
                ),
                (
                    "concurrency",
                    &[
                        "new Worker(",
                        "worker_threads",
                        "threading.",
                        "multiprocessing.",
                        "asyncio.create_task(",
                        "ThreadPoolExecutor",
                        "ProcessPoolExecutor",
                    ][..],
                ),
            ] {
                if tokens.iter().any(|token| {
                    text.contains(token)
                        || (token.contains('(') && compact.contains(&token.replace(' ', "")))
                }) {
                    assessment.risks.insert(risk.into());
                    assessment
                        .sources
                        .entry(risk.into())
                        .or_default()
                        .insert(path.clone());
                }
            }
        }
        if managed || path.ends_with(".h") || path.ends_with(".hpp") || path.ends_with(".rs") {
            let before = texts.first().map(String::as_str).unwrap_or("");
            let after = if texts.len() == 2 {
                texts[1].as_str()
            } else {
                ""
            };
            let public = |s: &str| -> BTreeSet<String> {
                s.lines()
                    .map(str::trim)
                    .filter(|l| {
                        [
                            "pub struct ",
                            "pub enum ",
                            "pub fn ",
                            "pub unsafe fn ",
                            "pub extern ",
                            "typedef ",
                            "extern ",
                            "struct ",
                            "class ",
                            "export function ",
                            "export class ",
                            "export interface ",
                            "export type ",
                            "export const ",
                            "def ",
                            "async def ",
                        ]
                        .iter()
                        .any(|token| l.starts_with(token))
                    })
                    .map(str::to_string)
                    .collect()
            };
            if public(before) != public(after) {
                assessment.risks.insert("public-api".into());
                assessment
                    .sources
                    .entry("public-api".into())
                    .or_default()
                    .insert(path.clone());
            }
        }
    }
    Ok(assessment)
}
impl RiskPolicy {
    /// Reject incomplete explicit matrices before a run or conditional risk check.
    /// Existing documents without mappings retain their original interpretation.
    pub fn validate_configuration_mappings(&self, plan: &CheckPlan) -> Result<()> {
        let configurations: BTreeSet<_> = plan.configurations.iter().cloned().collect();
        for route in self.routes.values() {
            if route.commands_by_configuration.is_empty() {
                continue;
            }
            require(
                !configurations.is_empty()
                    && route
                        .commands_by_configuration
                        .keys()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        == configurations,
                "risk command mapping must cover exactly the registered configurations",
            )?;
            require(
                route
                    .commands_by_configuration
                    .values()
                    .all(|commands| !commands.is_empty()),
                "risk configuration has no required commands",
            )?;
            require(
                route
                    .commands_by_configuration
                    .values()
                    .flatten()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    == route.commands,
                "risk configuration command union differs from registered commands",
            )?;
        }
        Ok(())
    }

    /// Added risks cannot be suppressed by a worker or satisfied by advisory checks.
    pub fn validate(
        &self,
        assessment: &Assessment,
        plan: &CheckPlan,
        registry: &crate::registry::CommandRegistry,
    ) -> Result<()> {
        self.validate_configuration_mappings(plan)?;
        for risk in &assessment.risks {
            let required = matches!(
                risk.as_str(),
                "resource-lifetime" | "ffi" | "unsafe" | "concurrency" | "public-api"
            );
            let Some(route) = self.routes.get(risk) else {
                require(
                    !required,
                    format!("unrouted risk requires operator planning: {risk}"),
                )?;
                continue;
            };
            require(
                !route.commands.is_empty() && !route.reviews.is_empty(),
                "risk route requires native evidence and semantic review",
            )?;
            if risk == "concurrency" {
                let groups: BTreeSet<_> =
                    route.concurrency_cases.keys().map(String::as_str).collect();
                require(groups == ["schedules", "linearizability", "cancellation", "shutdown"].into()
                    && route.concurrency_cases.values().all(|cases| !cases.is_empty())
                    && route.reviews.contains("concurrency-review"),
                    "concurrency requires protected schedule, linearizability, cancellation and shutdown cases plus concurrency-review")?;
                let cases: Vec<_> = route.concurrency_cases.values().flatten().collect();
                require(
                    cases.iter().collect::<BTreeSet<_>>().len() == cases.len(),
                    "concurrency properties need distinct observable cases",
                )?;
            }
            require(
                route
                    .reviews
                    .iter()
                    .all(|r| plan.review_requirements.contains(r)),
                format!("missing candidate review for {risk}"),
            )?;
            for configuration in &plan.configurations {
                let commands = route.commands_for_configuration(configuration)?;
                if risk == "concurrency" {
                    let planned: BTreeSet<_> = plan
                        .required_final()
                        .filter(|check| {
                            check.configuration_digest == *configuration
                                && commands.contains(&check.command_id)
                        })
                        .flat_map(|check| check.expected_case_ids.iter())
                        .collect();
                    require(route.concurrency_cases.values().flatten().all(|case| planned.contains(case)),
                        "concurrency property has no protected case in a required final configuration")?;
                }
                for command in commands {
                    require(
                        registry.get(command)?.kind == crate::plan::CheckKind::Behavioral,
                        "risk requires behavioral native evidence",
                    )?;
                    require(
                        plan.checks.iter().any(|check| {
                            check.phase == Phase::Final
                                && check.required
                                && check.command_id == *command
                                && check.configuration_digest == *configuration
                        }),
                        format!("risk {risk} lacks required final evidence for {configuration}"),
                    )?;
                }
            }
        }
        Ok(())
    }
}
