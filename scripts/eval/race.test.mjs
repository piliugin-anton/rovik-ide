// race.mjs tests (R01 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { candidateSettings, decide, holm, runRace, standing, worseP } from './race.mjs';

const near = (x, y, eps = 1e-9) => assert.ok(Math.abs(x - y) < eps, `${x} ≠ ${y}`);

describe('a race between settings (R01)', () => {
  const base = {
    profile: { id: 'p::big', model: 'big' },
    price: { inputPerMillion: 3, outputPerMillion: 15 },
    options: { reasoning_effort: 'high' },
    config: { permissions: { allowAnyCommand: true }, subagents: { enabled: false } },
  };

  it('a candidate may vary only what a race may', () => {
    const s = candidateSettings(base, {
      name: 'medium',
      settings: {
        options: { reasoning_effort: 'medium' },
        config: { subagents: { enabled: true } },
      },
    });
    assert.equal(s.options.reasoning_effort, 'medium');
    assert.deepEqual(s.config, {
      permissions: { allowAnyCommand: true },
      subagents: { enabled: true },
    });
    assert.equal(base.options.reasoning_effort, 'high', 'the base stays');
    assert.throws(
      () =>
        candidateSettings(base, {
          name: 'x',
          settings: { config: { permissions: { allowAnyCommand: false } } },
        }),
      /x: a race can't vary config\.permissions\.allowAnyCommand/
    );
    assert.throws(
      () => candidateSettings(base, { name: 'y', settings: { mode: 'plan' } }),
      /can't vary mode/
    );
  });

  it('the sign test on the tasks two candidates differ on', () => {
    near(worseP([-1, -1, -1, -1, -1, -1, -1, -1]), 1 / 256);
    near(worseP([-1, -1, -1, -1, 1, 1, 1, 1, 0, 0]), 163 / 256);
    assert.equal(worseP([0, 0]), 1, 'nothing to tell them apart by');
  });

  it("Holm's step-down", () => {
    assert.deepEqual(holm({ a: 0.01, b: 0.04, c: 0.03 }), ['a']);
    assert.deepEqual(holm({ a: 0.001, b: 0.02 }), ['a', 'b']);
    assert.deepEqual(holm({ a: 0.2 }), []);
  });

  it('D11: cheaper and not worse; better beyond the smallest change; over the limit', () => {
    const tasks = (rates) =>
      standing(rates.flatMap((p, i) => [{ task: `t${i}`, success: p, costUsd: 1 }]));
    const current = { ...tasks([1, 1, 0, 0, 1, 0, 1, 0, 1, 1]), costUsd: 1 };
    const same = { ...current, costUsd: 0.5 };
    assert.equal(decide(current, same, { margin: 0.05 }).verdict, 'cheaper and not worse');
    assert.equal(decide(current, { ...current, costUsd: 3 }, {}).verdict, 'over its cost limit');
    const better = { ...tasks(Array(10).fill(1)), costUsd: 1 };
    assert.equal(decide(current, better, { mde: 0.1 }).verdict, 'better');
    assert.equal(decide(current, { ...same, costUsd: 1 }, {}).verdict, 'nothing shown');
  });

  it('the done-when: a scripted race drops a clearly worse candidate and keeps a cheaper equal one', async () => {
    const outcome = {
      current: (i) => i % 5 < 3,
      cheaper: (i) => i % 5 < 3,
      worse: (i) => i % 10 === 0,
    };
    const cost = { current: 1, cheaper: 0.5, worse: 1 };
    const tasks = Array.from({ length: 30 }, (_, i) => `t${i}`);
    const record = await runRace({
      candidates: ['current', 'worse', 'cheaper'].map((name) => ({ name, settings: {} })),
      tasks,
      runBlock: async ({ name }, block) =>
        block.map((task) => ({
          task,
          success: outcome[name](Number(task.slice(1))),
          costUsd: cost[name],
        })),
    });
    const at = record.blocks.findIndex((b) => b.dropped.includes('worse'));
    assert.equal(at, 1, 'dropped as soon as it had ten tasks');
    assert.deepEqual(record.racing, ['current', 'cheaper']);
    assert.equal(record.decisions.cheaper.verdict, 'cheaper and not worse');
    assert.equal(record.chosen, 'cheaper');
    assert.equal(record.stopped, 'the tasks ran out');
    assert.equal(record.standings.worse.tasks.length, 10, 'it ran no more after it was dropped');
  });

  it('the budget stops it before a block that would pass it', async () => {
    let allowed = 2;
    const record = await runRace({
      candidates: [{ name: 'current' }, { name: 'other' }],
      tasks: Array.from({ length: 20 }, (_, i) => `t${i}`),
      runBlock: async (_, block) => block.map((task) => ({ task, success: true, costUsd: 1 })),
      canAfford: () => allowed-- > 0,
    });
    assert.equal(record.stopped, 'the budget would be passed');
    assert.equal(record.blocks.length, 2);
  });
});
