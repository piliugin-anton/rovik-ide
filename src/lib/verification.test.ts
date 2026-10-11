import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  describeFailure,
  progressLine,
  triageHeadline,
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
