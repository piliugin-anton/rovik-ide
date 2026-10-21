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
