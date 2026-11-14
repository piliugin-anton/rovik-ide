//! Operator-facing native pack conformance using pinned positive and negative fixtures.
use crate::{
    approvals::Principal,
    controller::{Controller, TaskSetup},
    evidence::{CheckResult, Status},
    identity::Digest,
    require,
    runner::{self, Cancellation},
    snapshot::Snapshot,
    verification::run_matrix,
    wire, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
/// Exact expected native outcome; infrastructure errors never count as a rejected mutant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedCheck {
    /// Required final check ID.
    pub check_id: String,
    /// Exact matrix member.
    pub configuration_digest: Digest,
    /// Pass for a valid fixture or Fail for a known violated property.
    pub status: Status,
    /// Complete known failing-case set; no invented behavioral failures from compilation.
    pub failed_case_ids: BTreeSet<String>,
}
/// One frozen fixture with protected registry, profile and oracle assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// Unique name within the conformance suite.
    pub fixture_id: String,
    /// Reviewed TaskSetup stored in the controller ledger.
    pub setup: Digest,
    /// Frozen source snapshot to check, which must pass ordinary scope admission.
    pub candidate: Digest,
    /// Exactly one expectation per mandatory final check/configuration.
    pub expected: Vec<ExpectedCheck>,
}
/// Pinned suite for one or more packs; each language includes a valid and wrong fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Exactly 1.
    pub schema_version: u32,
    /// Unique run prefix; repeating a spent identity cannot silently execute again.
    pub suite_id: String,
    /// Protected fixtures and their expected outcomes.
    pub fixtures: Vec<Fixture>,
}
/// Every result remains attached to an authenticated native receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureReport {
    /// Fixture identity.
    pub fixture_id: String,
    /// Durable native execution run.
    pub run_id: String,
    /// Whether every expected outcome and exact failing inventory matched.
    pub matches: bool,
    /// Raw normalized results, including infrastructure failures.
    pub results: Vec<CheckResult>,
}
/// Conformance evidence, distinct from production candidate acceptance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Exact suite manifest.
    pub suite: Digest,
    /// All requested fixture outcomes.
    pub fixtures: Vec<FixtureReport>,
    /// True only when all positive/negative fixtures matched.
    pub conforms: bool,
}
impl Suite {
    /// Validate all pinned inputs and expected matrix coverage before starting containers.
    pub fn validate(&self, controller: &Controller, principal: &Principal) -> Result<()> {
        require(
            self.schema_version == 1 && !self.fixtures.is_empty(),
            "conformance needs a nonempty v1 suite",
        )?;
        crate::id(&self.suite_id)?;
        crate::unique(
            self.fixtures
                .iter()
                .map(|fixture| fixture.fixture_id.as_str()),
        )?;
        let mut coverage = BTreeMap::new();
        for fixture in &self.fixtures {
            let config: TaskSetup = wire::decode(&controller.ledger.read(&fixture.setup)?)?;
            config.authority.authorize(principal, "operate")?;
            config.validate(&controller.ledger)?;
            controller
                .backend
                .require_environment(&config.environment)?;
            let candidate: Snapshot = wire::decode(&controller.ledger.read(&fixture.candidate)?)?;
            crate::snapshot::admit(
                &controller.ledger,
                &config.base,
                &candidate,
                &config.policy,
                &config.admission,
            )?;
            let required: BTreeSet<_> = config
                .plan
                .required_final()
                .map(|check| (check.check_id.clone(), check.configuration_digest.clone()))
                .collect();
            let expected: BTreeSet<_> = fixture
                .expected
                .iter()
                .map(|check| (check.check_id.clone(), check.configuration_digest.clone()))
                .collect();
            require(
                expected == required && expected.len() == fixture.expected.len(),
                "conformance fixture does not cover the exact final matrix",
            )?;
            for check in &fixture.expected {
                require(matches!(check.status,Status::Pass|Status::Fail) && (check.status!=Status::Pass || check.failed_case_ids.is_empty()),"conformance expects an executed pass or failure, never infrastructure uncertainty")?;
            }
            for profile in &config.profiles {
                for configuration in &profile.configurations {
                    let digest = configuration.digest()?;
                    let group = coverage
                        .entry((profile.language, digest.clone()))
                        .or_insert((false, false));
                    let outcomes: Vec<_> = fixture
                        .expected
                        .iter()
                        .filter(|check| check.configuration_digest == digest)
                        .collect();
                    group.0 |= !outcomes.is_empty()
                        && outcomes.iter().all(|check| check.status == Status::Pass);
                    group.1 |= outcomes.iter().any(|check| check.status == Status::Fail);
                }
            }
        }
        require(coverage.values().all(|(positive,negative)|*positive && *negative),"each selected language/configuration needs both a passing fixture and a known wrong implementation")
    }
}
impl Controller {
    /// Run registered native pack fixtures without provider calls or host repository execution.
    pub fn pack_conformance(
        &mut self,
        principal: &Principal,
        suite: &Suite,
        cancel: &Cancellation,
    ) -> Result<Report> {
        suite.validate(self, principal)?;
        let suite_digest = self
            .ledger
            .store_json("evaluation", suite, runner::now_ms()?)?
            .digest;
        let mut fixtures = vec![];
        for fixture in &suite.fixtures {
            let mut config: TaskSetup = wire::decode(&self.ledger.read(&fixture.setup)?)?;
            config.run_id = format!("conformance-{}-{}", suite.suite_id, fixture.fixture_id);
            config.plan.run_id = config.run_id.clone();
            let candidate: Snapshot = wire::decode(&self.ledger.read(&fixture.candidate)?)?;
            self.create(&config, runner::now_ms()?)?;
            let (cancel, _monitor) = crate::controller::ExecutionMonitor::start(
                self.ledger.root(),
                &config.run_id,
                cancel.clone(),
                config.deadline_ms,
            )?;
            let checks: Vec<_> = config.plan.required_final().cloned().collect();
            let results = run_matrix(self, &config, &candidate, &checks, &cancel)?;
            let matches = fixture.expected.iter().all(|expected| {
                results
                    .iter()
                    .filter(|result| {
                        result.check_id == expected.check_id
                            && result.configuration_digest == expected.configuration_digest
                            && result.status == expected.status
                            && result
                                .failed_case_ids
                                .iter()
                                .cloned()
                                .collect::<BTreeSet<_>>()
                                == expected.failed_case_ids
                    })
                    .count()
                    == 1
            }) && results.len() == fixture.expected.len();
            let observation = FixtureReport {
                fixture_id: fixture.fixture_id.clone(),
                run_id: config.run_id.clone(),
                matches,
                results,
            };
            let artifact = self
                .ledger
                .store_json("evaluation", &observation, runner::now_ms()?)?;
            self.ledger
                .finish_benchmark(&config.run_id, &artifact.digest)?;
            fixtures.push(observation);
        }
        let report = Report {
            suite: suite_digest,
            conforms: fixtures.iter().all(|fixture| fixture.matches),
            fixtures,
        };
        self.ledger
            .store_json("evaluation", &report, runner::now_ms()?)?;
        Ok(report)
    }
}
