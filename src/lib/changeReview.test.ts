import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reviewDetails, reviewHeadline, type ReviewFinding, type ReviewMeta } from './changeReview';

const finding = (severity: ReviewFinding['severity']): ReviewFinding => ({
  severity,
  location: 'a.ts:1',
  path: 'a.ts',
  line: 1,
  title: 't',
  scenario: 's',
  fix: 'f',
  confidence: 'confirmed',
});

const base: ReviewMeta = { kind: 'change', status: 'running', round: 1, maxRounds: 3 };

test('the review record’s headline', () => {
  assert.equal(
    reviewHeadline({ ...base, files: ['a.ts'], changedLines: 12 }, false),
    'Reviewing the change (12 changed lines in 1 file)…'
  );
  assert.equal(
    reviewHeadline({ ...base, status: 'clean' }, false),
    'The reviewer found nothing to fix.'
  );
  assert.equal(
    reviewHeadline(
      { ...base, status: 'findings', findings: [finding('high'), finding('low')] },
      false
    ),
    'The reviewer found 2 findings: 1 sent to the agent to fix or answer.'
  );
  assert.equal(
    reviewHeadline({ ...base, status: 'findings', findings: [finding('low')] }, false),
    'The reviewer found 1 finding, none serious enough to send to the agent.'
  );
  assert.equal(
    reviewHeadline(
      { ...base, status: 'skipped', reason: 'the reviewer’s model failed (server)' },
      false
    ),
    'Review skipped: the reviewer’s model failed (server). The checks decide.'
  );
  assert.match(reviewHeadline(base, true), /didn’t finish/);
});

test('the review record’s details', () => {
  assert.equal(
    reviewDetails({
      ...base,
      round: 2,
      toolCalls: 7,
      ms: 41_200,
      model: 'P/strong',
      modelsTried: ['P/bad', 'P/strong'],
    }),
    'Review 2 of at most 3 · 7 tool calls · 41 s · P/strong (after 1 failed)'
  );
});

test('a plan review’s headline says what happened to the plan', () => {
  const plan: ReviewMeta = { kind: 'plan', status: 'running', round: 1, maxRounds: 2 };
  assert.equal(reviewHeadline(plan, false), 'Reviewing the plan against the code…');
  const findings = { ...plan, status: 'findings' as const, findings: [finding('high')] };
  assert.equal(
    reviewHeadline({ ...findings, held: true }, false),
    'The plan reviewer found 1 finding: the plan goes back to the agent before it’s saved.'
  );
  assert.match(reviewHeadline({ ...findings, open: 1 }, false), /under “Open review findings”/);
  assert.equal(
    reviewHeadline(
      { ...plan, status: 'skipped', reason: 'the reviewer’s model failed (http)' },
      false
    ),
    'Plan review skipped: the reviewer’s model failed (http). The plan was saved as it was.'
  );
  assert.equal(
    reviewHeadline({ ...plan, status: 'clean' }, false),
    'The plan reviewer found nothing to fix.'
  );
});

test('a plan saved past the review budget says so', () => {
  const meta: ReviewMeta = {
    kind: 'plan',
    status: 'skipped',
    maxRounds: 2,
    reason: 'this run’s 2 plan reviews are used',
  };
  assert.equal(
    reviewHeadline(meta, false),
    'Plan review skipped: this run’s 2 plan reviews are used. The plan was saved as it was.'
  );
  assert.equal(reviewDetails(meta), 'No review: all 2 used');
});

test('after green, only high findings are sent to the agent', () => {
  const meta: ReviewMeta = {
    kind: 'change',
    status: 'findings',
    round: 1,
    maxRounds: 3,
    afterGreen: true,
    findings: [finding('high'), finding('medium'), finding('low')],
  };
  assert.equal(
    reviewHeadline(meta, false),
    'The reviewer found 3 findings: 1 sent to the agent to fix or answer.'
  );
  assert.match(reviewHeadline({ ...meta, afterGreen: false }, false), /: 2 sent to the agent/);
});
