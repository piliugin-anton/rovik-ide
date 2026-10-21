// arms.mjs tests (P06 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { attemptsResult, checksRank, pickAttempt } from './arms.mjs';

describe('the same-money arm: attempts picked by the project’s checks (P06)', () => {
  it('passed, then none ran, then flagged, then failed', () => {
    assert.deepEqual(
      ['passed', null, 'skipped', 'flagged', 'failed', 'unresolved', 'rolled-back'].map(checksRank),
      [0, 1, 1, 2, 3, 3, 3]
    );
  });

  it('the best attempt by its checks, the first of equals; never the hidden tests', () => {
    const a = (verification, success) => ({ verification, success });
    assert.equal(pickAttempt([a('failed', true), a('passed', false)]), 1, 'by the checks only');
    assert.equal(pickAttempt([a('passed', false), a('passed', true)]), 0, 'the first of equals');
    assert.equal(pickAttempt([a(null, false), a('unresolved', true)]), 0);
    assert.equal(pickAttempt([a('flagged', true), a(null, false), a('passed', true)]), 2);
  });

  it('the attempts as one run: the pick’s grade, everything’s cost', () => {
    const run = (verification, success, costUsd, tokens) => ({
      task: 't',
      rep: 1,
      success,
      outcome: success ? 'done' : 'verify-unresolved',
      verification,
      costUsd,
      costSource: 'price',
      tokens,
      ms: 1000,
      grade: { success },
      tampered: [],
    });
    const r = attemptsResult([run('unresolved', false, 0.5, 100), run('passed', true, 0.7, 200)]);
    assert.equal(r.picked, 1);
    assert.equal(r.success, true);
    assert.ok(Math.abs(r.costUsd - 1.2) < 1e-9);
    assert.deepEqual([r.tokens, r.ms], [300, 2000]);
    assert.deepEqual(
      r.attempts.map((x) => x.verification),
      ['unresolved', 'passed']
    );
    // One attempt's cost unknown: the arm's is.
    assert.equal(
      attemptsResult([run('passed', true, null, 1), run(null, false, 1, 1)]).costUsd,
      null
    );
  });
});
