import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  causeLine,
  describeFailure,
  progressLine,
  repairHeadline,
  reviewAfterGreenLine,
  triageHeadline,
  type RepairCause,
  type RepairMeta,
  type TriageMeta,
  type VerificationMeta,
} from './verification';

const base: VerificationMeta = {
  status: 'failed',
  round: 2,
  maxRounds: 3,
  checks: [],
  changedFiles: [],
};

test('the round’s progress in one line', () => {
  assert.equal(progressLine(base), null);
  assert.equal(
    progressLine({
      ...base,
      progress: { verdict: 'progress', new: [], fixed: ['a', 'b'] },
      failureCount: 1,
      testsRun: 12,
      testsRunBefore: 12,
    }),
    '2 fixed · 1 failing · 12 tests ran'
  );
  assert.equal(
    progressLine({
      ...base,
      progress: { verdict: 'no-progress', new: [], fixed: [] },
      failureCount: 3,
    }),
    'No progress: the same failures as the round before · 3 failing'
  );
  assert.equal(
    progressLine({
      ...base,
      status: 'rolled-back',
      progress: { verdict: 'regression', new: ['x'], fixed: ['y'] },
      failureCount: 1,
      testsRun: 1,
      testsRunBefore: 4,
    }),
    '1 fixed · 1 new · 1 test ran (4 before)'
  );
});

test('failure keys read as words', () => {
  assert.equal(describeFailure('test|test|t::two'), 'test t::two');
  assert.equal(
    describeFailure("typecheck|src/a.ts|TS2322|Type 'x'"),
    "src/a.ts · TS2322 · Type 'x'"
  );
  assert.equal(describeFailure('build|failed'), 'failed');
});

test('the triage’s line says why it ran and how it ended', () => {
  const base: TriageMeta = { status: 'running', check: 'test', reason: 'large output' };
  assert.equal(triageHeadline(base, 'Tests'), 'Triaging the tests failure (its output is large)…');
  assert.equal(
    triageHeadline({ ...base, reason: 'failed again' }, 'Lint'),
    'Triaging the lint failure (it failed again)…'
  );
  assert.equal(
    triageHeadline(
      {
        ...base,
        status: 'done',
        result: {
          check: 'test',
          rootCause: 'x',
          files: [],
          nextStep: 'y',
          confidence: 'high',
          confidenceNote: '',
        },
      },
      'Tests'
    ),
    'Triage of the tests failure: high confidence.'
  );
  assert.equal(
    triageHeadline({ ...base, status: 'skipped', skipped: 'its model failed (http)' }, 'Tests'),
    'Triage of the tests failure skipped: its model failed (http). The agent got the usual report.'
  );
  assert.equal(triageHeadline(base, 'Tests', true), 'Triage of the tests failure didn’t finish.');
});

test('the repair round says who fixes the causes', () => {
  const w2: RepairCause = {
    owner: { kind: 'worker', label: 'w2', step: 3, describe: 'worker w2 (step 3)' },
    keys: ['test|src/posts.rs|E0425|a', 'test|src/posts.rs|E0308|b'],
    files: ['src/posts.rs'],
    summary: 'E0425 cannot find value `p`',
    fixer: { status: 'done', changed: ['src/posts.rs'], said: 'Fixed.', parallel: false },
  };
  const lead: RepairCause = {
    owner: { kind: 'lead', reason: 'contract', describe: 'the lead (the contract)' },
    keys: ['test|src/api.rs|E0412|c'],
    files: ['src/api.rs'],
    summary: 'E0412 cannot find type `Api`',
  };
  const r: RepairMeta = {
    group: 'pages',
    round: 1,
    status: 'recheck',
    parallel: false,
    causes: [w2],
  };
  assert.equal(repairHeadline(r), '1 cause: the workers fixed them; the checks run again.');
  assert.equal(
    repairHeadline({ ...r, status: 'lead', causes: [w2, lead] }),
    '2 causes: the workers fixed theirs; the agent fixes the rest.'
  );
  assert.match(
    repairHeadline({ ...r, status: 'lead', leadOnly: 'the last round made no progress' }),
    /the agent fixes them all \(the last round made no progress\)/
  );
  assert.equal(
    causeLine(w2),
    'E0425 cannot find value `p` (2 failures) · worker w2 (step 3) — fixed (src/posts.rs)'
  );
  assert.equal(
    causeLine(lead),
    'E0412 cannot find type `Api` (1 failure) · the lead (the contract)'
  );
});

test('the review after green says what it found, or why it didn’t run', () => {
  assert.equal(reviewAfterGreenLine(base), null);
  assert.match(
    reviewAfterGreenLine({ ...base, reviewAfterGreen: { status: 'findings' } }) ?? '',
    /checks run again/
  );
  assert.equal(
    reviewAfterGreenLine({
      ...base,
      reviewAfterGreen: { status: 'skipped', reason: 'the run’s 3 change reviews are used up' },
    }),
    'No review after the fix: the run’s 3 change reviews are used up.'
  );
});
