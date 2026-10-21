// P04 (docs/design/evolve-plan.md): comparing two campaigns task by task.
// Two averages and the spread of three repetition scores say little; the
// difference on each task both ran says more, and tasks from one module
// aren't independent, so the error is clustered by module (Miller, "Adding
// Error Bars to Evals", 2024). Pure: eval.mjs and lib.mjs call it.

import { dirname } from 'node:path';

const sum = (xs) => xs.reduce((a, b) => a + b, 0);
const mean = (xs) => (xs.length ? sum(xs) / xs.length : 0);

/**
 * Each task both campaigns ran (each a `summarize()` result's `tasks`):
 * its success rate in each and the difference, b − a, in task order.
 */
export function pairedDifferences(aTasks, bTasks) {
  const b = new Map(bTasks.map((t) => [t.task, t]));
  return aTasks
    .filter((t) => b.has(t.task))
    .map((t) => ({
      task: t.task,
      a: t.successRate,
      b: b.get(t.task).successRate,
      diff: b.get(t.task).successRate - t.successRate,
    }))
    .sort((x, y) => x.task.localeCompare(y.task));
}

/** The differences' groups: cluster -> the differences of its tasks. */
function groups(diffs, clusterOf) {
  const by = new Map();
  for (const d of diffs) {
    const key = clusterOf(d.task);
    if (!by.has(key)) by.set(key, []);
    by.get(key).push(d.diff);
  }
  return [...by.values()];
}

/**
 * The standard error of the mean difference, clustered: tasks in one
 * cluster may move together, so their residuals are summed before
 * squaring, with the usual G/(G − 1) correction for few clusters. With
 * every task its own cluster it is the plain standard error (the sample
 * deviation over √T). Null with fewer than two clusters: nothing to
 * estimate the spread from.
 */
export function clusteredStandardError(diffs, clusterOf = (task) => task) {
  const gs = groups(diffs, clusterOf);
  const n = diffs.length;
  if (gs.length < 2) return null;
  const m = mean(diffs.map((d) => d.diff));
  const between = sum(gs.map((g) => sum(g.map((x) => x - m)) ** 2));
  return Math.sqrt((gs.length / (gs.length - 1)) * (between / (n * n)));
}

/** A small seeded generator (mulberry32): the same interval for the same results. */
function random(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The `p` quantile of sorted `xs`, interpolated. */
function quantile(xs, p) {
  const at = (xs.length - 1) * p;
  const lo = Math.floor(at);
  return xs[lo] + (xs[Math.min(lo + 1, xs.length - 1)] - xs[lo]) * (at - lo);
}

/**
 * A percentile interval of the mean difference by the cluster bootstrap:
 * whole clusters drawn again with replacement (`draws` times, seeded), the
 * mean over the tasks drawn each time. Null with fewer than two clusters.
 */
export function bootstrapInterval(
  diffs,
  clusterOf = (task) => task,
  { level = 0.95, draws = 4000, seed = 1 } = {}
) {
  const gs = groups(diffs, clusterOf);
  if (gs.length < 2) return null;
  const next = random(seed);
  const means = [];
  for (let i = 0; i < draws; i++) {
    let total = 0;
    let count = 0;
    for (let j = 0; j < gs.length; j++) {
      const g = gs[Math.floor(next() * gs.length)];
      total += sum(g);
      count += g.length;
    }
    means.push(total / count);
  }
  means.sort((x, y) => x - y);
  const tail = (1 - level) / 2;
  return [quantile(means, tail), quantile(means, 1 - tail)];
}

/**
 * The paired comparison of two campaigns' tasks: how many both ran, the
 * mean difference in success (b − a), its standard error plain and
 * clustered (`clusterOf`: a task's module), the clusters, a 95% bootstrap
 * interval by cluster, and the tasks b does better on, worse on, and the
 * same.
 */
export function pairedComparison(aTasks, bTasks, clusterOf = (task) => task) {
  const diffs = pairedDifferences(aTasks, bTasks);
  const values = diffs.map((d) => d.diff);
  return {
    tasks: diffs.length,
    meanDiff: mean(values),
    se: clusteredStandardError(diffs),
    seClustered: clusteredStandardError(diffs, clusterOf),
    clusters: groups(diffs, clusterOf).length,
    interval: bootstrapInterval(diffs, clusterOf),
    wins: values.filter((d) => d > 0).length,
    losses: values.filter((d) => d < 0).length,
    ties: values.filter((d) => d === 0).length,
    diffs,
  };
}

/**
 * A task's top-level module, what its errors are clustered by: the one
 * its file names (`module`, as mined tasks will), else its repository and
 * the folder of its first hidden test ("self:src-tauri/src/clm").
 */
export function moduleOf(task) {
  if (task.module) return task.module;
  const repo = task.repo === 'self' ? 'self' : (task.repo?.name ?? 'unknown');
  return `${repo}:${dirname(task.hidden?.[0] ?? '.')}`;
}

// ------------------------------------------------------ P07: power

/**
 * The standard normal distribution's quantile (Acklam's approximation,
 * good to about 1e-9): z with P(Z ≤ z) = p.
 */
export function normalQuantile(p) {
  if (!(p > 0 && p < 1)) throw new Error(`no quantile at ${p}`);
  const a = [
    -39.69683028665376, 220.9460984245205, -275.9285104469687, 138.357751867269, -30.66479806614716,
    2.506628277459239,
  ];
  const b = [
    -54.47609879822406, 161.5858368580409, -155.6989798598866, 66.80131188771972,
    -13.28068155288572,
  ];
  const c = [
    -0.007784894002430293, -0.3223964580411365, -2.400758277161838, -2.549732539343734,
    4.374664141464968, 2.938163982698783,
  ];
  const d = [0.007784695709041462, 0.3224671290700398, 2.445134137142996, 3.754408661907416];
  const tail = (q) =>
    (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) /
    ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1);
  if (p < 0.02425) return tail(Math.sqrt(-2 * Math.log(p)));
  if (p > 1 - 0.02425) return -tail(Math.sqrt(-2 * Math.log(1 - p)));
  const q = p - 0.5;
  const r = q * q;
  return (
    ((((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q) /
    (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1)
  );
}

/**
 * Each task's run-to-run noise from its runs (a `summarize()` task: runs,
 * successes): the variance of one run's outcome, p(1 − p) with the
 * n/(n − 1) correction; null below two runs.
 */
export function runVariance({ runs, successes }) {
  if (runs < 2) return null;
  const p = successes / runs;
  return (p * (1 - p) * runs) / (runs - 1);
}

/**
 * The design effect of the modules on a campaign's own noise: the
 * campaign against itself, repetition against repetition (an A/A
 * comparison, so any difference is noise), its clustered variance over
 * its plain one, both summed over the pairs that vary (steadier than a
 * mean of ratios with few modules); at least 1, and 1 when nothing
 * varies. `runs`: [{ task, rep, success }].
 */
export function designEffect(runs, clusterOf = (task) => task) {
  const reps = [...new Set(runs.map((r) => r.rep))].sort((x, y) => x - y);
  const by = new Map(runs.map((r) => [`${r.task}#${r.rep}`, r.success ? 1 : 0]));
  const tasks = [...new Set(runs.map((r) => r.task))];
  let plainSum = 0;
  let clusteredSum = 0;
  for (let i = 0; i < reps.length; i++)
    for (let j = i + 1; j < reps.length; j++) {
      const diffs = tasks
        .filter((t) => by.has(`${t}#${reps[i]}`) && by.has(`${t}#${reps[j]}`))
        .map((t) => ({ task: t, diff: by.get(`${t}#${reps[j]}`) - by.get(`${t}#${reps[i]}`) }));
      const plain = clusteredStandardError(diffs);
      const clustered = clusteredStandardError(diffs, clusterOf);
      if (plain > 0 && clustered !== null) {
        plainSum += plain ** 2;
        clusteredSum += clustered ** 2;
      }
    }
  return plainSum > 0 ? Math.max(1, clusteredSum / plainSum) : 1;
}

/** The mean run-to-run variance of the tasks with two runs or more; null with none. */
export function meanRunVariance(tasks) {
  const noise = tasks.map(runVariance).filter((v) => v !== null);
  return noise.length ? mean(noise) : null;
}

/**
 * P07: the minimum detectable effect of a paired comparison of two
 * campaigns of `tasks` tasks, `repeats` repetitions each, whose runs vary
 * by `variance` (a run's outcome's): the smallest difference in success
 * a two-sided test at `alpha` finds with `power`, widened by the modules'
 * design effect (`deff`). Null without the noise or the tasks to tell.
 */
export function detectableEffect({
  variance,
  tasks,
  repeats,
  alpha = 0.05,
  power = 0.8,
  deff = 1,
}) {
  if (variance === null || !(tasks > 0) || !(repeats >= 1)) return null;
  const z = normalQuantile(1 - alpha / 2) + normalQuantile(power);
  return z * Math.sqrt(((2 * variance) / repeats / tasks) * Math.max(1, deff));
}

/**
 * P07: `detectableEffect` for a campaign's tasks (`summarize()` tasks,
 * whose runs give the noise), compared with another as large.
 */
export function minimumDetectableEffect(tasks, { repeats, alpha, power, deff } = {}) {
  const counted = tasks.filter((t) => runVariance(t) !== null).length;
  return detectableEffect({
    variance: meanRunVariance(tasks),
    tasks: counted,
    repeats,
    alpha,
    power,
    deff,
  });
}

/**
 * P07: how many tasks a paired comparison needs, at `repeats`
 * repetitions an arm, to show a change of `target` (the inverse of
 * `detectableEffect`); null without the noise.
 */
export function tasksFor({ variance, repeats, target, alpha = 0.05, power = 0.8, deff = 1 }) {
  if (variance === null || !(repeats >= 1) || !(target > 0)) return null;
  const z = normalQuantile(1 - alpha / 2) + normalQuantile(power);
  return Math.ceil((2 * variance * Math.max(1, deff) * z * z) / (repeats * target * target));
}
