//! Pure context-pressure decisions. The run supplies measurements and writes logs.

use serde_json::{json, Value};

/// A step-compaction request waiting for its edit (Phase D asks, Phase E
/// escalates when it is ignored).
#[derive(Debug, Clone)]
struct PendingStepCompaction {
    tool_calls: u32,
}

/// When a compaction pressure started (Phase E): the tool-call count then,
/// and the run's turn then (for turns-to-edit). Any edit clears it.
#[derive(Debug, Clone)]
struct ClmWait {
    tool_calls: u32,
    turn: u32,
}

/// Phase E pressure thresholds: a step-compaction request or the 75%
/// reminder escalates after this many tool calls with no edit (at 50%+);
/// 90%+ restricts at once until an edit lands.
pub(super) const CLM_ESCALATE_AFTER_CALLS: u32 = 4;
const CLM_ESCALATE_MIN_PCT: u32 = 50;
pub(super) const CLM_RESTRICT_LIFT_BELOW: u32 = 75;
/// Restricted turns with no edit before the safety valve lifts it.
pub(super) const CLM_MAX_RESTRICTED_STALL: u32 = 2;

/// Measurements for the next model turn.
pub(super) struct ClmTurn<'a> {
    pub pct: u32,
    pub nudged: &'a [u32],
    pub revision: Option<u64>,
    pub tokens: usize,
    pub tool_calls: u32,
}

/// A change to the context-tool restriction, reported once to the run.
#[derive(Debug, PartialEq)]
pub(super) enum Restriction {
    Restricted { reason: &'static str, turn: u32 },
    NonCompliant { turn: u32, restricted_turns: u32 },
}

impl Restriction {
    pub(super) fn log(&self, pct: u32, tool_calls: u32) -> Value {
        match self {
            Self::Restricted { reason, turn } => {
                json!({"event":"clmRestricted", "reason":reason, "turn":turn, "pct":pct, "toolCalls":tool_calls})
            }
            Self::NonCompliant {
                turn,
                restricted_turns,
            } => {
                json!({"event":"clmNonCompliant", "turn":turn, "pct":pct, "restrictedTurns":restricted_turns})
            }
        }
    }
}

/// Pressures, edits, and compliance accumulated by one run.
#[derive(Default)]
pub(super) struct ContextState {
    /// A step-compaction request is outstanding (Phase D, for Phase E
    /// escalation): the step that just finished, the context revision it
    /// was asked at, and how many tool calls had run then.
    pending_step_compaction: Option<PendingStepCompaction>,
    /// Phase E: this turn offers only context tools (the model must compact
    /// before anything else). Set when a step-compaction request or the 75%
    /// reminder is ignored, or at 90%+ with no edit since the urgent
    /// reminder; lifted by an edit, by dropping below 75%, or by the safety
    /// valve (`clm_non_compliant`).
    clm_restricted: bool,
    /// Restricted turns so far, and consecutive restricted turns with no
    /// edit (the safety valve lifts at CLM_MAX_RESTRICTED_STALL).
    restricted_turns: u32,
    restricted_stall: u32,
    /// The safety valve fired: weak models aren't restricted again.
    clm_non_compliant: bool,
    /// CLM turns so far (for turns-to-edit), and the peak context percent.
    clm_turns: u32,
    clm_peak_pct: u32,
    /// Edits seen (new context revisions) and their token deltas
    /// (revision, tokens before, tokens after), oldest first.
    clm_edits_seen: u32,
    clm_edit_deltas: Vec<(u64, usize, usize)>,
    /// Redone work: recall-hint notes offered after a compaction removed
    /// an earlier result the model then asked for again.
    clm_recall_hits: u32,
    /// Reminder tiers seen this run (25, 50, 75, 90), and turns-to-edit after
    /// each pressure started.
    clm_reminders_fired: Vec<u32>,
    step_asked_turn: Option<u32>,
    step_turns_to_edit: Option<u32>,
    tier75_turns_to_edit: Option<u32>,
    urgent_turns_to_edit: Option<u32>,
    /// The 75% reminder and the 90%+ urgent state while no edit has landed
    /// since (revision, tool calls and turn when first seen).
    tier75_since: Option<ClmWait>,
    urgent_since: Option<ClmWait>,
    /// CLM tracking started (the first CLM state was seen).
    clm_tracking_started: bool,
    /// A write to the context file (or a restore) succeeded since the last
    /// turn (set in execute.rs). The file is rewritten before every request,
    /// so a new revision alone doesn't mean the agent edited anything.
    context_edited: bool,
    /// An edit seen last turn, waiting for its size: the context is only
    /// re-measured by the next request (tokens before the edit).
    edit_delta_pending: Option<usize>,
}

impl ContextState {
    pub(super) fn restricted(&self) -> bool {
        self.clm_restricted
    }
    pub(super) fn tracking_started(&self) -> bool {
        self.clm_tracking_started
    }
    pub(super) fn edited(&mut self) {
        self.context_edited = true;
    }
    pub(super) fn recall_hit(&mut self) {
        self.clm_recall_hits += 1;
    }
    pub(super) fn step_compaction_asked(&mut self, tool_calls: u32) {
        self.pending_step_compaction = Some(PendingStepCompaction { tool_calls });
        self.step_asked_turn = Some(self.clm_turns);
    }
    pub(super) fn turn(&mut self, seen: ClmTurn<'_>) -> Option<Restriction> {
        self.clm_turns += 1;
        let pct = seen.pct;
        self.clm_peak_pct = self.clm_peak_pct.max(pct);
        for t in seen
            .nudged
            .iter()
            .copied()
            .chain((pct >= crate::clm::prompt::URGENT).then_some(90))
        {
            if !self.clm_reminders_fired.contains(&t) {
                self.clm_reminders_fired.push(t);
            }
        }
        self.clm_reminders_fired.sort_unstable();

        // An edit: a successful write to the context file, a restore, or a
        // command on the file's real path, flagged as it happens
        // (execute.rs). Not a new revision as such: the file is rewritten
        // before every request with the new messages, so its revision moves
        // every turn whether the agent edited it or not.
        let edited = std::mem::take(&mut self.context_edited);
        // Last turn's edit, now measured by the request that read it back.
        if let (Some(before), Some(r)) = (self.edit_delta_pending.take(), seen.revision) {
            self.clm_edit_deltas.push((r, before, seen.tokens));
        }
        if !self.clm_tracking_started {
            self.clm_tracking_started = true;
            if !edited {
                return None;
            }
        }
        if edited {
            self.clm_edits_seen += 1;
            self.edit_delta_pending = Some(seen.tokens);
            if self.pending_step_compaction.take().is_some() {
                if let Some(asked) = self.step_asked_turn.take() {
                    self.step_turns_to_edit
                        .get_or_insert(self.clm_turns.saturating_sub(asked));
                }
            }
            if let Some(w) = self.urgent_since.take() {
                self.urgent_turns_to_edit
                    .get_or_insert(self.clm_turns.saturating_sub(w.turn));
            }
            if let Some(w) = self.tier75_since.take() {
                self.tier75_turns_to_edit
                    .get_or_insert(self.clm_turns.saturating_sub(w.turn));
            }
            self.clm_restricted = false;
            self.restricted_stall = 0;
            return None;
        }

        // Safety valve first: two restricted turns already, still no edit.
        if self.clm_restricted && self.restricted_stall >= CLM_MAX_RESTRICTED_STALL {
            self.clm_restricted = false;
            self.restricted_stall = 0;
            self.clm_non_compliant = true;
            return Some(Restriction::NonCompliant {
                turn: self.clm_turns,
                restricted_turns: self.restricted_turns,
            });
        }
        // Below 75% the restriction no longer applies.
        if self.clm_restricted && pct < CLM_RESTRICT_LIFT_BELOW {
            self.clm_restricted = false;
            self.restricted_stall = 0;
        }

        // Remember when each pressure started waiting for its edit; an edit
        // clears it, and it re-arms if the pressure is still there.
        if seen.nudged.contains(&75) && self.tier75_since.is_none() {
            self.tier75_since = Some(ClmWait {
                tool_calls: seen.tool_calls,
                turn: self.clm_turns,
            });
        }
        if pct >= crate::clm::prompt::URGENT && self.urgent_since.is_none() {
            self.urgent_since = Some(ClmWait {
                tool_calls: seen.tool_calls,
                turn: self.clm_turns,
            });
        }

        if self.clm_non_compliant || self.clm_restricted {
            if self.clm_restricted {
                self.restricted_turns += 1;
                self.restricted_stall += 1;
            }
            return None;
        }
        let reason = restriction_reason(
            pct,
            self.urgent_since.is_some(),
            self.pending_step_compaction.as_ref().map(|p| p.tool_calls),
            self.tier75_since.as_ref().map(|w| w.tool_calls),
            seen.tool_calls,
        );
        if let Some(reason) = reason {
            self.clm_restricted = true;
            self.restricted_turns += 1;
            self.restricted_stall += 1;
            return Some(Restriction::Restricted {
                reason,
                turn: self.clm_turns,
            });
        }
        None
    }
    pub(super) fn compliance(&self) -> Value {
        json!({
                    "remindersFired": self.clm_reminders_fired,
                    "turnsToEditStep": self.step_turns_to_edit,
                    "turnsToEdit75": self.tier75_turns_to_edit,
                    "turnsToEditUrgent": self.urgent_turns_to_edit,
                    "edits": self.clm_edits_seen,
                    "editDeltas": self.clm_edit_deltas.iter().map(|(r, before, after)| json!({
                        "revision": r, "before": before, "after": after,
                    })).collect::<Vec<_>>(),
                    "recallHints": self.clm_recall_hits,
                    "peakPct": self.clm_peak_pct,
                    "restrictedTurns": self.restricted_turns,
                    "nonCompliant": self.clm_non_compliant,
        })
    }
}

/// Whether this CLM turn is restricted to context tools, and why: at 90%+
/// while the urgent state waits for an edit, or at 50%+ once a
/// step-compaction request or the 75% reminder has gone unanswered for
/// CLM_ESCALATE_AFTER_CALLS tool calls. The waits are cleared by an edit,
/// so a wait that's still here means no edit since it started.
fn restriction_reason(
    pct: u32,
    urgent_waiting: bool,
    step_asked_at: Option<u32>,
    tier75_seen_at: Option<u32>,
    tool_calls: u32,
) -> Option<&'static str> {
    let waited = |since: u32| tool_calls.saturating_sub(since) >= CLM_ESCALATE_AFTER_CALLS;
    if pct >= crate::clm::prompt::URGENT && urgent_waiting {
        Some("urgent")
    } else if pct >= CLM_ESCALATE_MIN_PCT && step_asked_at.is_some_and(waited) {
        Some("step")
    } else if pct >= CLM_ESCALATE_MIN_PCT && tier75_seen_at.is_some_and(waited) {
        Some("tier75")
    } else {
        None
    }
}

#[cfg(test)]
mod clm_pressure_tests {
    use super::*;

    #[test]
    fn restriction_needs_an_unanswered_pressure() {
        // Urgent: restricted at once while it waits for an edit.
        assert_eq!(restriction_reason(92, true, None, None, 0), Some("urgent"));
        assert_eq!(
            restriction_reason(92, false, None, None, 0),
            None,
            "an edit cleared the wait"
        );
        // A step-compaction request escalates after 4 calls with no edit, at 50%+.
        assert_eq!(restriction_reason(60, false, Some(10), None, 13), None);
        assert_eq!(
            restriction_reason(60, false, Some(10), None, 14),
            Some("step")
        );
        assert_eq!(
            restriction_reason(40, false, Some(10), None, 30),
            None,
            "below 50%"
        );
        // The 75% reminder, likewise.
        assert_eq!(
            restriction_reason(78, false, None, Some(5), 9),
            Some("tier75")
        );
        assert_eq!(restriction_reason(78, false, None, Some(5), 8), None);
        // Nothing waiting: never restricted, whatever the size.
        assert_eq!(restriction_reason(85, false, None, None, 100), None);
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    fn turn(state: &mut ContextState, pct: u32, calls: u32) -> Option<Restriction> {
        state.turn(ClmTurn {
            pct,
            nudged: &[],
            revision: Some(7),
            tokens: 1000,
            tool_calls: calls,
        })
    }
    #[test]
    fn an_edit_lifts_the_restriction_and_counts_turns_to_edit() {
        let mut state = ContextState::default();
        turn(&mut state, 90, 0);
        state.step_compaction_asked(0);
        turn(&mut state, 90, 4);
        assert!(state.restricted());
        state.edited();
        assert_eq!(turn(&mut state, 90, 5), None);
        assert!(!state.restricted());
        let log = state.compliance();
        assert_eq!(log["turnsToEditStep"], 2);
        assert_eq!(log["turnsToEditUrgent"], 1);
        assert_eq!(log["edits"], 1);
        turn(&mut state, 50, 5);
        assert_eq!(
            state.compliance()["editDeltas"],
            json!([{"revision":7,"before":1000,"after":1000}])
        );
    }
    #[test]
    fn ninety_percent_with_no_edit_restricts_the_next_turn() {
        let mut state = ContextState::default();
        assert_eq!(turn(&mut state, 90, 0), None);
        assert_eq!(
            turn(&mut state, 90, 0),
            Some(Restriction::Restricted {
                reason: "urgent",
                turn: 2
            })
        );
        assert!(state.restricted());
    }
    #[test]
    fn an_ignored_step_compaction_restricts_after_four_calls() {
        let mut state = ContextState::default();
        turn(&mut state, 60, 10);
        state.step_compaction_asked(10);
        assert_eq!(turn(&mut state, 60, 13), None);
        assert_eq!(
            turn(&mut state, 60, 14),
            Some(Restriction::Restricted {
                reason: "step",
                turn: 3
            })
        );
    }
    #[test]
    fn below_seventy_five_percent_the_restriction_lifts() {
        let mut state = ContextState::default();
        turn(&mut state, 90, 0);
        turn(&mut state, 90, 0);
        assert!(state.restricted());
        assert_eq!(turn(&mut state, 74, 0), None);
        assert!(!state.restricted());
    }
    #[test]
    fn the_safety_valve_lifts_the_restriction_for_good() {
        let mut state = ContextState::default();
        turn(&mut state, 90, 0);
        turn(&mut state, 90, 0);
        turn(&mut state, 90, 0);
        assert_eq!(
            turn(&mut state, 90, 0),
            Some(Restriction::NonCompliant {
                turn: 4,
                restricted_turns: 2
            })
        );
        assert!(!state.restricted());
        for calls in 1..10 {
            assert_eq!(turn(&mut state, 99, calls), None);
        }
        state.edited();
        turn(&mut state, 99, 10);
        turn(&mut state, 99, 11);
        assert!(!state.restricted());
        assert_eq!(state.compliance()["nonCompliant"], true);
    }
}
