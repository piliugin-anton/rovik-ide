// P06 (docs/design/evolve-plan.md, 5.12 A): the same-money arm. The
// current agent given the money of k runs, as k attempts at each task, one
// of them picked by the project's own checks (what the run's verification
// found), never by the hidden tests. A candidate is better only if it
// beats this too: otherwise "better agent" and "more compute" look the
// same. eval.mjs runs the attempts; this picks and adds them up.

/**
 * The same-money arms beside Autopilot's: the current agent with the
 * money of k runs, as k attempts at each task (`budget.json` `attempts`:
 * its cost budget, cap and repetitions).
 */
export const ATTEMPT_ARMS = [
  { id: 'attempts-2', attempts: 2, label: 'Two attempts, picked by the checks' },
];

/**
 * How good an attempt looks by the project's checks, its last
 * verification's status (lower is better): they passed; none ran (no
 * change, or checks off); they passed but the change weakened tests
 * (`flagged`); they failed.
 */
export function checksRank(status) {
  if (status === 'passed') return 0;
  if (status === null || status === undefined || status === 'skipped') return 1;
  if (status === 'flagged') return 2;
  return 3;
}

/** The attempt to keep: the best by the project's checks, the first of equals. */
export function pickAttempt(attempts) {
  let best = 0;
  for (let i = 1; i < attempts.length; i++)
    if (checksRank(attempts[i].verification) < checksRank(attempts[best].verification)) best = i;
  return best;
}

const total = (attempts, key) => attempts.reduce((a, r) => a + (r[key] ?? 0), 0);

/**
 * The result of a task's attempts, as one run's: the picked attempt's
 * grade and outcome, what all of them cost (unknown if any one's is), and
 * each attempt in brief.
 */
export function attemptsResult(attempts) {
  const picked = pickAttempt(attempts);
  const p = attempts[picked];
  const costs = attempts.map((r) => r.costUsd);
  return {
    ...p,
    costUsd: costs.every((c) => typeof c === 'number') ? costs.reduce((a, b) => a + b, 0) : null,
    costSource: attempts.every((r) => r.costSource === p.costSource) ? p.costSource : 'mixed',
    tokens: total(attempts, 'tokens'),
    ms: total(attempts, 'ms'),
    interventions: total(attempts, 'interventions'),
    toolCalls: total(attempts, 'toolCalls'),
    toolErrors: total(attempts, 'toolErrors'),
    editMisses: total(attempts, 'editMisses'),
    verifyRounds: total(attempts, 'verifyRounds'),
    peakLeadContext: Math.max(0, ...attempts.map((r) => r.peakLeadContext ?? 0)),
    picked,
    attempts: attempts.map(({ success, outcome, verification, costUsd }) => ({
      success,
      outcome,
      verification,
      costUsd,
    })),
  };
}
