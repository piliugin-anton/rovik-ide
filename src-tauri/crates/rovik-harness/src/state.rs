//! Controller states and their legal transitions; model text is never an event.

use crate::{require, Result};
use serde::{Deserialize, Serialize};

/// Durable workflow position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum State {
    /// Resolve authorized request, repository and scope.
    Intake,
    /// Establish baseline and approve semantics.
    Baseline,
    /// Contract is ready for design.
    ContractReady,
    /// Design is ready for oracle registration.
    DesignReady,
    /// Protected acceptance assets are ready.
    TestsReady,
    /// Scoped implementation work.
    Implement,
    /// Advisory native feedback.
    FastChecks,
    /// Conditional risk verification.
    RiskChecks,
    /// Required semantic review.
    Review,
    /// Clean immutable candidate verification.
    CleanCi,
    /// Recheck all current approvals and integration context.
    ReadyForApproval,
    /// Terminal acceptance, independent of merge/deployment.
    Accepted,
    /// Missing prerequisite; retain earliest restart state.
    Blocked,
    /// Scoped repair within reserved budget.
    Repair,
    /// Approved redesign within reserved budget.
    Replan,
    /// Authorized cancellation.
    Cancelled,
    /// Exhausted budget or nonrecoverable failure.
    Stopped,
    /// Authorized final rejection.
    Rejected,
}
impl State {
    /// Exit outcome for run execution; management commands report command success separately.
    pub fn execution_exit_code(self) -> i32 {
        match self {
            Self::Accepted => 0,
            Self::Stopped | Self::Rejected => 3,
            Self::Cancelled => 5,
            _ => 2,
        }
    }
    /// Terminal runs never resume; a new run preserves their history.
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Cancelled | Self::Stopped | Self::Rejected
        )
    }
    /// Validate transition topology. Stage prerequisites are checked by the controller.
    pub fn permits(self, next: Self, restart: Option<Self>) -> Result<()> {
        use State::*;
        require(!self.terminal(), "terminal run cannot transition")?;
        let ordinary = matches!(
            (self, next),
            (Intake, Baseline)
                | (Baseline, ContractReady)
                | (ContractReady, DesignReady)
                | (DesignReady, TestsReady)
                | (TestsReady, Implement)
                | (Implement, FastChecks)
                | (FastChecks, RiskChecks)
                | (RiskChecks, Review)
                | (Review, CleanCi)
                | (CleanCi, ReadyForApproval)
                | (ReadyForApproval, Accepted)
                | (Repair, FastChecks)
                | (Replan, DesignReady)
        );
        let repair = next == Repair && matches!(self, FastChecks | RiskChecks | Review | CleanCi);
        let replan = next == Replan && matches!(self, Implement | DesignReady | Review | Repair);
        let resume = self == Blocked
            && restart == Some(next)
            && !matches!(next, Blocked | Accepted | ReadyForApproval)
            && !next.terminal();
        require(
            ordinary
                || repair
                || replan
                || resume
                || matches!(next, Blocked | Cancelled | Stopped)
                || (self == ReadyForApproval && next == Rejected),
            format!("invalid transition: {self:?} -> {next:?}"),
        )
    }
}

/// Changes choose the earliest invalidated stage rather than reusing stale checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalidation {
    /// Business semantics or protected expected behavior changed.
    Contract,
    /// Responsibility/design record changed.
    Design,
    /// Candidate source or declared dependency inputs changed.
    Candidate,
    /// Policy, toolchain, checker, flags or exception changed.
    Verification,
    /// Destination branch moved.
    Integration,
}
impl Invalidation {
    /// Earliest stage that must be repeated.
    pub fn restart(self) -> State {
        match self {
            Self::Contract => State::Baseline,
            Self::Design => State::ContractReady,
            Self::Candidate => State::FastChecks,
            Self::Verification => State::TestsReady,
            Self::Integration => State::Implement,
        }
    }
}
