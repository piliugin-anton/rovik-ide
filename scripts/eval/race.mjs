// R01 (docs/design/evolve-plan.md, 5.2 F, D11): a race between settings.
// Every candidate runs on the same tasks, a block at a time; after each
// block, a paired test (corrected for the number of candidates by Holm's
// method) drops those clearly worse than the best, as F-Race does; the
// current settings always run, as what the others are measured against.
// At the end, D11 decides: a candidate is taken when it's cheaper and not
// worse by more than a margin stated in advance, or better beyond the
// smallest change the tasks can show, within the cost limit. No model
// writes anything: a fixed algorithm over a list of allowed settings.
// Pure: eval.mjs runs the blocks.
import {
  bootstrapInterval,
  detectableEffect,
  meanRunVariance,
  pairedDifferences,
} from './stats.mjs';

/**
 * What a race may vary (4.3's stage 1): the lead's model and its price,
 * its reasoning effort, the context mode, the subagents. Paths into the
 * runner's settings; a candidate that changes anything else is refused.
 */
export const RACEABLE = [
  'profile',
  'price',
  'options.reasoning_effort',
  'contextMode',
  'config.subagents',
];

const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

/** The paths a settings patch sets: its leaves, or whole values where `RACEABLE` names them. */
function pathsOf(patch, prefix = '') {
  return Object.entries(patch).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return isObject(value) && !RACEABLE.includes(path) ? pathsOf(value, path) : [path];
  });
}

/** `base` with `patch` merged into it, objects key by key. */
export function merged(base, patch) {
  const out = structuredClone(base);
  for (const [key, value] of Object.entries(patch))
    out[key] =
      isObject(value) && isObject(out[key]) ? merged(out[key], value) : structuredClone(value);
  return out;
}

/**
 * A candidate's settings: the base's with its patch, which may set only
 * what a race may vary (`RACEABLE`). Throws naming what it may not.
 */
export function candidateSettings(base, candidate) {
  const allowed = (path) => RACEABLE.some((r) => path === r || path.startsWith(`${r}.`));
  const refused = pathsOf(candidate.settings ?? {}).filter((p) => !allowed(p));
  if (refused.length)
    throw new Error(
      `${candidate.name}: a race can't vary ${refused.join(', ')} (it may: ${RACEABLE.join(', ')})`
    );
  return merged(base, candidate.settings ?? {});
}

/** C(n, k) as a float, for the binomial tail. */
function choose(n, k) {
  let c = 1;
  for (let i = 1; i <= k; i++) c = (c * (n - k + i)) / i;
  return c;
}

/**
 * The paired sign test that a candidate is worse than another, on the
 * tasks they differ on (`diffs`: its success minus the other's, a task
 * each): the chance of losing on at least as many of those tasks if
 * neither were better (a one-sided exact binomial).
 */
export function worseP(diffs) {
  const losses = diffs.filter((d) => d < 0).length;
  const n = losses + diffs.filter((d) => d > 0).length;
  if (n === 0) return 1;
  let tail = 0;
  for (let k = losses; k <= n; k++) tail += choose(n, k);
  return Math.min(1, tail / 2 ** n);
}

/**
 * Holm's step-down correction: which of `ps` (name -> p-value) are
 * rejected at `alpha` overall: the smallest at alpha/m, the next at
 * alpha/(m − 1), and so on, until one isn't.
 */
export function holm(ps, alpha = 0.05) {
  const sorted = Object.entries(ps).sort(([, a], [, b]) => a - b);
  const rejected = [];
  for (let i = 0; i < sorted.length; i++) {
    if (sorted[i][1] > alpha / (sorted.length - i)) break;
    rejected.push(sorted[i][0]);
  }
  return rejected;
}

const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);

/**
 * `items` in an order drawn from `seed` (Fisher–Yates on a small seeded
 * generator): the race's tasks mixed, languages and modules, the same
 * order each time it's resumed.
 */
export function shuffled(items, seed = 1) {
  let s = seed >>> 0;
  const next = () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = Math.imul(s ^ (s >>> 15), s | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const out = [...items];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(next() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/**
 * A candidate's standing from its runs ([{ task, success, costUsd }]):
 * its tasks (each one's success rate, as `summarize` has them), its
 * success and its cost a run (null if any run's is unknown).
 */
export function standing(runs) {
  const by = new Map();
  for (const r of runs) {
    if (!by.has(r.task)) by.set(r.task, []);
    by.get(r.task).push(r);
  }
  const tasks = [...by].map(([task, rs]) => ({
    task,
    runs: rs.length,
    successes: rs.filter((r) => r.success).length,
    successRate: mean(rs.map((r) => (r.success ? 1 : 0))),
  }));
  const costs = runs.map((r) => r.costUsd);
  return {
    tasks,
    successRate: mean(runs.map((r) => (r.success ? 1 : 0))),
    costUsd: costs.length && costs.every((c) => typeof c === 'number') ? mean(costs) : null,
  };
}

/** The best so far: the highest success, the cheapest of equals. */
function best(standings) {
  return Object.entries(standings).sort(
    ([, a], [, b]) =>
      b.successRate - a.successRate || (a.costUsd ?? Infinity) - (b.costUsd ?? Infinity)
  )[0][0];
}

/**
 * Which candidates to drop after a block: those a paired sign test shows
 * worse than the best, Holm-corrected across them; never the current
 * settings (`keep`), and none before `minTasks` tasks.
 */
export function dropped(standings, { keep, alpha = 0.05, minTasks = 10 }) {
  const leader = best(standings);
  const ps = {};
  for (const [name, s] of Object.entries(standings)) {
    if (name === leader || name === keep) continue;
    const diffs = pairedDifferences(standings[leader].tasks, s.tasks);
    if (diffs.length < minTasks) continue;
    ps[name] = worseP(diffs.map((d) => d.diff));
  }
  return { leader, ps, drop: holm(ps, alpha) };
}

/**
 * D11's decision for a candidate against the current settings, over the
 * tasks both ran: "better" when the paired interval is above zero and the
 * gain at least the smallest change the tasks can show, within the cost
 * limit; else "cheaper and not worse" when the interval stays above
 * −margin and it costs less; else nothing shown.
 */
export function decide(current, candidate, { margin = 0.05, costRatioMax = 2, clusterOf, mde }) {
  const diffs = pairedDifferences(current.tasks, candidate.tasks);
  const interval = bootstrapInterval(diffs, clusterOf) ?? [-Infinity, Infinity];
  const meanDiff = mean(diffs.map((d) => d.diff));
  const smallest =
    mde ??
    detectableEffect({
      variance: meanRunVariance([...current.tasks, ...candidate.tasks]),
      tasks: diffs.length,
      repeats: Math.max(1, ...candidate.tasks.map((t) => t.runs)),
    });
  const costRatio =
    current.costUsd > 0 && candidate.costUsd !== null ? candidate.costUsd / current.costUsd : null;
  const numbers = { tasks: diffs.length, meanDiff, interval, mde: smallest, costRatio };
  if (costRatio !== null && costRatio > costRatioMax)
    return { verdict: 'over its cost limit', ...numbers };
  if (interval[0] > 0 && smallest !== null && meanDiff >= smallest)
    return { verdict: 'better', ...numbers };
  if (interval[0] > -margin && costRatio !== null && costRatio < 1)
    return { verdict: 'cheaper and not worse', ...numbers };
  return { verdict: 'nothing shown', ...numbers };
}

/**
 * The race. `candidates`: [{ name, settings }] (their patches), the
 * current settings' named `current`; `tasks`: ids in the order they run;
 * `runBlock(candidate, tasks)`: the runs of one candidate on one block,
 * [{ task, success, costUsd }]; `canAfford(runs)`: whether so many more
 * runs fit the budget. Records each block, who dropped out and why, and
 * the decision: the better candidate with the most success, else the
 * cheapest that's cheaper and not worse, else the current settings.
 */
export async function runRace({
  candidates,
  tasks,
  runBlock,
  blockSize = 5,
  minTasks = 10,
  alpha = 0.05,
  margin = 0.05,
  costRatioMax = 2,
  clusterOf = (t) => t,
  canAfford = () => true,
}) {
  const keep = 'current';
  if (!candidates.some((c) => c.name === keep))
    throw new Error('a race needs the current settings, named "current"');
  const runs = Object.fromEntries(candidates.map((c) => [c.name, []]));
  let racing = candidates.map((c) => c.name);
  const blocks = [];
  let stopped = 'the tasks ran out';
  for (let at = 0; at < tasks.length; at += blockSize) {
    const block = tasks.slice(at, at + blockSize);
    if (!canAfford(block.length * racing.length)) {
      stopped = 'the budget would be passed';
      break;
    }
    for (const name of racing)
      runs[name].push(
        ...(await runBlock(
          candidates.find((c) => c.name === name),
          block
        ))
      );
    const standings = Object.fromEntries(racing.map((n) => [n, standing(runs[n])]));
    const step = dropped(standings, { keep, alpha, minTasks });
    racing = racing.filter((n) => !step.drop.includes(n));
    blocks.push({ tasks: block, leader: step.leader, ps: step.ps, dropped: step.drop });
    // With one challenger left, the rest of the tasks still narrow D11's
    // interval: the race goes on while the current settings have company.
    if (racing.length === 1) {
      stopped = 'only the current settings are left';
      break;
    }
  }
  const standings = Object.fromEntries(candidates.map((c) => [c.name, standing(runs[c.name])]));
  const decisions = Object.fromEntries(
    racing
      .filter((n) => n !== keep)
      .map((n) => [n, decide(standings[keep], standings[n], { margin, costRatioMax, clusterOf })])
  );
  const by = (verdict) => Object.entries(decisions).filter(([, d]) => d.verdict === verdict);
  const better = by('better').sort(
    ([a], [b]) => standings[b].successRate - standings[a].successRate
  );
  const cheaper = by('cheaper and not worse').sort(
    ([a], [b]) => standings[a].costUsd - standings[b].costUsd
  );
  const chosen = better[0]?.[0] ?? cheaper[0]?.[0] ?? keep;
  return { blocks, stopped, racing, standings, decisions, chosen, margin, alpha };
}

const pct = (x) => `${Math.round(x * 100)}%`;
const pts = (x) => `${x >= 0 ? '+' : '−'}${(Math.abs(x) * 100).toFixed(1)} pts`;
const money = (x) => (typeof x === 'number' ? `$${x.toFixed(4)}` : 'unknown');

/** The race's record as Markdown: each block, each candidate's standing, D11's decisions. */
export function raceMarkdown(record, { title = 'Settings race', fake = false } = {}) {
  const lines = [
    `# ${title}`,
    '',
    fake
      ? '> **Dry run with a scripted model: these numbers test the pipeline and say nothing about any real model.**\n'
      : '',
    `Generated by \`node scripts/eval/eval.mjs race\` (R01 in docs/design/evolve-plan.md). ` +
      `Stopped: ${record.stopped}. A candidate is dropped when a sign test on the tasks it and the ` +
      `leader differ on shows it worse (Holm-corrected, α ${record.alpha}); D11 takes one cheaper ` +
      `and not worse by more than ${(record.margin * 100).toFixed(0)} points, or better beyond the ` +
      'smallest change the tasks can show.',
    '',
    '## Blocks',
    '',
    '| Block | Tasks | Leader | Dropped (p) |',
    '| --- | --- | --- | --- |',
    ...record.blocks.map(
      (b, i) =>
        `| ${i + 1} | ${b.tasks.length} | ${b.leader} | ` +
        `${b.dropped.map((n) => `${n} (${b.ps[n].toFixed(4)})`).join(', ') || '—'} |`
    ),
    '',
    '## Candidates',
    '',
    '| Candidate | Tasks | Success | Cost a run | Against current | D11 |',
    '| --- | --- | --- | --- | --- | --- |',
    ...Object.entries(record.standings).map(([name, s]) => {
      const d = record.decisions[name];
      const against = d
        ? `${pts(d.meanDiff)} (95% ${pts(d.interval[0])} to ${pts(d.interval[1])}), ` +
          `cost ${d.costRatio === null ? 'n/a' : `${d.costRatio.toFixed(2)}×`}`
        : name === 'current'
          ? '—'
          : 'dropped';
      return (
        `| ${name} | ${s.tasks.length} | ${pct(s.successRate)} | ${money(s.costUsd)} | ${against} | ` +
        `${d?.verdict ?? (name === 'current' ? 'the reference' : 'dropped')} |`
      );
    }),
    '',
    '## Decision',
    '',
    record.chosen === 'current'
      ? 'No candidate beat the current settings by D11: they stay.'
      : `**${record.chosen}**: ${record.decisions[record.chosen].verdict}. Into the defaults only ` +
        'by hand, in a pull request that cites this (R03).',
    '',
  ];
  return lines.join('\n');
}
