// stats.mjs tests (P04 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  bootstrapInterval,
  clusteredStandardError,
  designEffect,
  minimumDetectableEffect,
  moduleOf,
  normalQuantile,
  pairedComparison,
  pairedDifferences,
  runVariance,
  tasksFor,
} from './stats.mjs';

const tasks = (rates) =>
  Object.entries(rates).map(([task, successRate]) => ({ task, successRate }));
const near = (x, y, eps = 1e-9) => assert.ok(Math.abs(x - y) < eps, `${x} ≠ ${y}`);

describe('paired comparisons (P04)', () => {
  it('the difference on each task both ran', () => {
    const d = pairedDifferences(tasks({ t2: 0, t1: 1 / 3, t3: 1 }), tasks({ t1: 1, t2: 0, t4: 1 }));
    assert.deepEqual(
      d.map((x) => [x.task, x.diff]),
      [
        ['t1', 1 - 1 / 3],
        ['t2', 0],
      ]
    );
  });

  it('each task its own cluster: the plain standard error', () => {
    // Differences 0.5, 0.5, -0.1, -0.1: mean 0.2, sample deviation √0.12.
    const d = [0.5, 0.5, -0.1, -0.1].map((diff, i) => ({ task: `t${i}`, diff }));
    near(clusteredStandardError(d), Math.sqrt(0.12) / 2);
  });

  it('tasks that move together in a module widen the error', () => {
    // Two modules, each task in one alike: residual sums ±0.6, so
    // SE² = 2/(2−1) × (0.36 + 0.36)/16 = 0.09.
    const d = [0.5, 0.5, -0.1, -0.1].map((diff, i) => ({ task: `t${i}`, diff }));
    const clusterOf = (task) => (task === 't0' || task === 't1' ? 'clm' : 'agent_run');
    near(clusteredStandardError(d, clusterOf), 0.3);
    assert.ok(clusteredStandardError(d, clusterOf) > clusteredStandardError(d));
    assert.equal(
      clusteredStandardError(d, () => 'one'),
      null,
      'one cluster says nothing'
    );
  });

  it('the bootstrap interval: seeded, by cluster, around a known difference', () => {
    const same = Array.from({ length: 20 }, (_, i) => ({ task: `t${i}`, diff: 0.1 }));
    assert.deepEqual(
      bootstrapInterval(same).map((x) => +x.toFixed(9)),
      [0.1, 0.1]
    );
    // A true difference of 0.2, give or take 0.3 in each of 30 tasks.
    const noisy = Array.from({ length: 30 }, (_, i) => ({
      task: `t${i}`,
      diff: 0.2 + (i % 2 ? 0.3 : -0.3),
    }));
    const [lo, hi] = bootstrapInterval(noisy);
    assert.ok(lo < 0.2 && 0.2 < hi && hi - lo < 0.3, `${lo} ${hi}`);
    assert.deepEqual(bootstrapInterval(noisy), [lo, hi], 'the same each time');
    // By modules of 10 that each moved together: wider.
    const moved = noisy.map((d, i) => ({ ...d, diff: [0.6, 0.2, -0.2][Math.floor(i / 10)] }));
    const byTen = (task) => Math.floor(Number(task.slice(1)) / 10);
    const [clo, chi] = bootstrapInterval(moved, byTen);
    assert.ok(chi - clo > hi - lo, `${clo} ${chi}`);
    assert.equal(
      bootstrapInterval(noisy, () => 'one'),
      null
    );
  });

  it('the comparison: the mean, both errors, the interval, wins and losses', () => {
    const a = tasks({ t1: 0, t2: 1, t3: 0.5, t4: 0 });
    const b = tasks({ t1: 1, t2: 1, t3: 0, t4: 1 });
    const c = pairedComparison(a, b, (task) => (task < 't3' ? 'x' : 'y'));
    near(c.meanDiff, (1 + 0 - 0.5 + 1) / 4);
    assert.equal(c.tasks, 4);
    assert.equal(c.clusters, 2);
    assert.deepEqual([c.wins, c.losses, c.ties], [2, 1, 1]);
    assert.ok(c.se > 0 && c.seClustered >= 0 && c.interval.length === 2);
  });

  it('a task’s module: its own, else its repository and its tests’ folder', () => {
    assert.equal(moduleOf({ module: 'agent_run', repo: 'self', hidden: ['x/y.rs'] }), 'agent_run');
    assert.equal(
      moduleOf({ repo: 'self', hidden: ['src-tauri/src/clm/tests.rs'] }),
      'self:src-tauri/src/clm'
    );
    assert.equal(moduleOf({ repo: { name: 'lo', url: 'u' }, hidden: ['slice_test.go'] }), 'lo:.');
  });
});

describe('power: the smallest change a comparison can show (P07)', () => {
  it('the normal quantiles', () => {
    near(normalQuantile(0.975), 1.959964, 1e-6);
    near(normalQuantile(0.8), 0.841621, 1e-6);
    near(normalQuantile(0.01), -2.326348, 1e-6);
    near(normalQuantile(0.5), 0, 1e-12);
    assert.throws(() => normalQuantile(1));
  });

  it('a task’s noise from its runs', () => {
    assert.equal(runVariance({ runs: 3, successes: 3 }), 0, 'always passes: no noise');
    near(runVariance({ runs: 3, successes: 1 }), (1 / 3) * (2 / 3) * 1.5);
    assert.equal(runVariance({ runs: 1, successes: 1 }), null);
  });

  it('the plan’s table: p(1 − p) ≈ 0.1 a task', () => {
    // 1,000 runs a task, so the n/(n − 1) correction is next to nothing.
    const p = (1 + Math.sqrt(1 - 0.4)) / 2; // p(1 − p) = 0.1
    const tasks = (n) =>
      Array.from({ length: n }, (_, i) => ({
        task: `t${i}`,
        runs: 1000,
        successes: Math.round(1000 * p),
      }));
    const mde = (n, repeats) =>
      Math.round(1000 * minimumDetectableEffect(tasks(n), { repeats })) / 10;
    assert.deepEqual(
      [mde(10, 3), mde(50, 3), mde(100, 3), mde(100, 6), mde(200, 3)],
      [22.9, 10.2, 7.2, 5.1, 5.1]
    );
    // The modules widen it by the root of their design effect.
    near(
      minimumDetectableEffect(tasks(10), { repeats: 3, deff: 4 }),
      2 * minimumDetectableEffect(tasks(10), { repeats: 3 })
    );
    assert.equal(
      minimumDetectableEffect([{ task: 'a', runs: 1, successes: 0 }], { repeats: 3 }),
      null
    );
  });

  it('how many tasks a 5-point claim needs: the plan’s 100 × 6, or 200 × 3', () => {
    const v = 0.1;
    assert.equal(tasksFor({ variance: v, repeats: 6, target: 0.05 }), 105);
    assert.equal(tasksFor({ variance: v, repeats: 3, target: 0.05 }), 210);
    assert.equal(tasksFor({ variance: null, repeats: 3, target: 0.05 }), null);
  });

  it('the design effect: repetition against repetition, by module', () => {
    // Two modules whose tasks flip together between repetitions.
    const runs = [];
    for (let rep = 1; rep <= 3; rep++)
      for (let i = 0; i < 8; i++)
        runs.push({ task: `t${i}`, rep, success: i < 4 ? rep !== 2 : rep === 2 });
    const byHalf = (task) => (Number(task.slice(1)) < 4 ? 'a' : 'b');
    assert.ok(designEffect(runs, byHalf) > 1.5, `${designEffect(runs, byHalf)}`);
    assert.equal(designEffect(runs), 1, 'each task its own module');
    // Runs that never change say nothing: no effect.
    assert.equal(
      designEffect(
        runs.map((r) => ({ ...r, success: true })),
        byHalf
      ),
      1
    );
  });
});
