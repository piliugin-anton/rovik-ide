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
