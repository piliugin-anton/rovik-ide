// identity.mjs tests (P03 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { identityDiff, identityProblem } from './identity.mjs';

describe('what a campaign’s runs are made with (P03)', () => {
  const was = {
    runnerCommit: 'abc1234',
    runnerHash: 'aaaaaaaaaaaaaaaa1',
    integrityHash: 'bbbbbbbbbbbbbbbb',
    settingsHash: 'cccccccccccccccc',
    policyHash: 'dddddddddddddddd',
  };

  it('the same build and settings go on', () => {
    assert.deepEqual(identityDiff(was, { ...was }), []);
    assert.equal(identityProblem('/r/baseline', was, { ...was }), '');
    // A commit alone (a change outside the runner, say) isn't another build.
    assert.equal(identityProblem('/r/baseline', was, { ...was, runnerCommit: 'def5678' }), '');
  });

  it('a rebuilt runner refuses, and says what changed', () => {
    const now = { ...was, runnerCommit: 'def5678', runnerHash: 'eeeeeeeeeeeeeeee' };
    assert.deepEqual(identityDiff(was, now), ['runnerHash']);
    assert.equal(
      identityProblem('/r/baseline', was, now),
      '/r/baseline was run with another runner (aaaaaaaaaaaa → eeeeeeeeeeee); the runner’s commit ' +
        'was abc1234, now def5678: resume it with the same, or start a new campaign (--out)'
    );
  });

  it('other settings or policy refuse too', () => {
    const now = { ...was, settingsHash: 'ffff', policyHash: 'gggg' };
    assert.deepEqual(identityDiff(was, now), ['settingsHash', 'policyHash']);
    assert.match(identityProblem('/r/x', was, now), /another settings .*, policy/);
  });
});
