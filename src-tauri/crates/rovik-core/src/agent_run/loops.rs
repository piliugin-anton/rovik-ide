//! Thorough's loops (docs/design/autopilot-plan.md 3.2, D12, T07): the
//! plan review, the change review and the acceptance check go on in a
//! Thorough task while their rounds make progress, not for a count. A
//! round makes progress when it leaves less open than any round before it
//! (the plan's and the change's open high and medium findings, the
//! criteria still unsatisfied); two rounds in a row that don't end the
//! loop, as nothing left open does. The task's budget (U04) ends the run,
//! and with it the loop, at its next reply, whichever comes first; a
//! backstop of MAX_ROUNDS keeps a loop with no budget from going on for
//! ever. The repair loop's rounds were bounded so already, in both
//! efforts (V03: failing keys, `stalled_rounds`).
//!
//! Progress is against the least left open so far, not the round before:
//! a loop that only goes back and forth (two findings, one, two, one…)
//! makes none after its best round, and ends.

/// Rounds in a row without progress that end a Thorough loop.
pub const STALLED: u32 = 2;
/// The most rounds a Thorough loop has, whatever its progress.
pub const MAX_ROUNDS: u32 = 10;

/// A loop's progress so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// The least any round left open.
    best: Option<usize>,
    /// Rounds in a row since a round last left less open than that.
    stalled: u32,
}

impl Progress {
    /// Round `round` left `open` open: whether another round may follow.
    /// None open ends the loop: there's nothing left to go round for.
    pub fn another_round(&mut self, open: usize, round: u32) -> bool {
        match self.best {
            Some(best) if open >= best => self.stalled += 1,
            _ => {
                self.best = Some(open);
                self.stalled = 0;
            }
        }
        open > 0 && self.stalled < STALLED && round < MAX_ROUNDS
    }

    /// Rounds in a row without progress, as the last `another_round` left
    /// them.
    pub fn stalled(&self) -> u32 {
        self.stalled
    }
}

impl super::run::Run {
    /// Whether this run is a Thorough Autopilot task's: its loops go on by
    /// progress (T07).
    pub(super) fn thorough(&self) -> bool {
        self.in_autopilot()
            && self
                .stage
                .as_ref()
                .is_some_and(|s| s.effort == super::autopilot::Effort::Thorough)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_goes_on_while_it_leaves_less_open() {
        let mut p = Progress::default();
        assert!(p.another_round(3, 1), "the first round is the baseline");
        assert!(p.another_round(2, 2));
        assert!(p.another_round(1, 3));
        assert_eq!(p.stalled(), 0);
        assert!(!p.another_round(0, 4), "nothing left open ends it");
    }

    #[test]
    fn two_rounds_without_progress_end_it() {
        let mut p = Progress::default();
        assert!(p.another_round(2, 1));
        assert!(p.another_round(1, 2));
        assert!(p.another_round(1, 3), "one round without progress");
        assert!(!p.another_round(1, 4), "the second ends it");
        assert_eq!(p.stalled(), 2);
        // Progress again starts the count afresh.
        let mut p = Progress::default();
        assert!(p.another_round(3, 1));
        assert!(p.another_round(3, 2));
        assert!(p.another_round(2, 3));
        assert_eq!(p.stalled(), 0);
        assert!(p.another_round(2, 4));
    }

    #[test]
    fn going_back_and_forth_is_no_progress() {
        let mut p = Progress::default();
        assert!(p.another_round(2, 1));
        assert!(p.another_round(1, 2));
        assert!(p.another_round(2, 3), "worse than its best");
        assert!(!p.another_round(1, 4), "as good as its best, not better");
    }

    #[test]
    fn the_backstop_ends_a_loop_that_keeps_going() {
        let mut p = Progress::default();
        for round in 1..MAX_ROUNDS {
            assert!(p.another_round(100 - round as usize, round), "{round}");
        }
        assert!(!p.another_round(1, MAX_ROUNDS));
    }
}
