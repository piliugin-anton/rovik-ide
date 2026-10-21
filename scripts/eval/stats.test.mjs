// stats.mjs tests (P04 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  bootstrapInterval,
  clusteredStandardError,
  moduleOf,
  pairedComparison,
  pairedDifferences,
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
