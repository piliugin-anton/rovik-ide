// P03 (docs/design/evolve-plan.md): what a campaign's runs are made with.
// Each result records it, and a campaign resumed with any part of it
// changed refuses: its runs would mix two builds, or two settings, without
// a word. eval.mjs does the I/O (hashing the files).

/** What decides how a run goes, by what each is to a person. */
const PARTS = {
  runnerHash: 'runner',
  integrityHash: "grader's integrity check",
  settingsHash: 'settings',
  policyHash: 'policy',
};

/**
 * How `now` differs from the campaign's identity (`was`): the parts that
 * decide how a run goes. The runner's commit is a note for people: the
 * binary is the build.
 */
export function identityDiff(was, now) {
  return Object.keys(PARTS).filter((key) => was?.[key] !== now[key]);
}

const short = (hash) => (typeof hash === 'string' ? hash.slice(0, 12) : 'none');

/** Why the campaign in `dir` can't go on with `now` ('' if it can). */
export function identityProblem(dir, was, now) {
  const diff = identityDiff(was, now);
  if (!diff.length) return '';
  const parts = diff.map((key) => `${PARTS[key]} (${short(was?.[key])} → ${short(now[key])})`);
  const commits =
    was?.runnerCommit !== now.runnerCommit
      ? `; the runner’s commit was ${was?.runnerCommit ?? 'unknown'}, now ${now.runnerCommit ?? 'unknown'}`
      : '';
  return (
    `${dir} was run with another ${parts.join(', ')}${commits}: resume it with the same, ` +
    'or start a new campaign (--out)'
  );
}
