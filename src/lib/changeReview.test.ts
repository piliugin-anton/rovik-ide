import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  formatScore,
  guideHeadline,
  reviewAnswersSummary,
  reviewDetails,
  reviewFacts,
  reviewHeadline,
  type ReviewFinding,
  type ReviewMeta,
} from './changeReview';

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
      { ...base, status: 'clean', reason: 'its reply had no Findings section' },
      false
    ),
    'The reviewer reported nothing to fix: its reply had no Findings section.'
  );
  assert.equal(
    reviewHeadline(
      { ...base, status: 'findings', findings: [finding('high'), finding('low')] },
      false
    ),
    'The reviewer found 2 findings: 2 sent to the agent to fix or answer, and a follow-up review checks them.'
  );
  assert.equal(
    reviewHeadline({ ...base, status: 'findings', findings: [finding('low')] }, false),
    'The reviewer found 1 finding: 1 sent to the agent to fix or answer, and a follow-up review checks them.'
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
  // Tokens: whole under a thousand, else "12.3k"; the model alone when only it was tried.
  const usage = (inputTokens: number, outputTokens: number) => ({
    ...base,
    round: 1,
    model: 'P/strong',
    modelsTried: ['P/strong'],
    usage: { inputTokens, outputTokens },
  });
  assert.equal(reviewDetails(usage(600, 399)), 'Review 1 of at most 3 · P/strong · 999 tokens');
  assert.equal(
    reviewDetails(usage(12_000, 345)),
    'Review 1 of at most 3 · P/strong · 12.3k tokens'
  );
  assert.equal(
    reviewDetails({ ...usage(1, 1), modelsTried: undefined, partial: true }),
    'Review 1 of at most 3 · P/strong · 2 tokens · out of budget'
  );
  // T07: a Thorough task's reviews go on by progress: no count to show.
  assert.equal(reviewDetails({ ...base, round: 4, maxRounds: 10, byProgress: true }), 'Review 4');
  assert.deepEqual(
    reviewFacts({ ...base, round: 4, maxRounds: 10, byProgress: true }).find(
      ([k]) => k === 'Round'
    ),
    ['Round', '4, going on while the findings get fewer']
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

test('a follow-up plan review says what became of the earlier findings', () => {
  const check = (finding: number, resolved: boolean) => ({
    finding,
    severity: 'high' as const,
    location: `Step ${finding}`,
    title: 't',
    response: { action: 'fixed' as const, note: 'Added step 3.' },
    resolved,
    reason: 'r',
  });
  const clean: ReviewMeta = {
    kind: 'plan',
    status: 'clean',
    round: 2,
    maxRounds: 2,
    earlier: [check(1, true)],
  };
  assert.equal(
    reviewHeadline(clean, false),
    'The plan reviewer found the earlier finding resolved and nothing new to fix.'
  );
  assert.equal(
    reviewHeadline({ ...clean, notHeld: [finding('medium')] }, false),
    'The plan reviewer found the earlier finding resolved and nothing new to fix (1 finding about what the revision didn’t change, not held).'
  );
  const open: ReviewMeta = {
    ...clean,
    status: 'findings',
    open: 1,
    findings: [finding('high')],
    earlier: [check(1, true), check(2, false)],
    notHeld: [finding('medium'), finding('low')],
  };
  const facts = new Map(reviewFacts(open));
  assert.equal(facts.get('Earlier findings'), '1 resolved, 1 still open');
  assert.equal(facts.get('Not held'), '2, about what the revision didn’t change');

  // No verdict from the reviewer: settled, but not called resolved.
  const unchecked = { ...check(2, true), checked: false };
  assert.equal(
    reviewHeadline({ ...clean, earlier: [check(1, true), unchecked] }, false),
    'The plan reviewer found nothing new to fix, but gave no verdict on 1 earlier finding: taken as settled.'
  );
  assert.equal(
    new Map(reviewFacts({ ...open, earlier: [check(1, false), unchecked] })).get(
      'Earlier findings'
    ),
    '0 resolved, 1 still open, 1 not checked'
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

test('every finding is sent to the agent, whatever its severity, after green too', () => {
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
    'The reviewer found 3 findings: 3 sent to the agent to fix or answer, and a follow-up review checks them.'
  );
  assert.match(reviewHeadline({ ...meta, afterGreen: false }, false), /: 3 sent to the agent/);
  assert.match(
    reviewHeadline({ ...meta, findings: [finding('low')] }, false),
    /^The reviewer found 1 finding: 1 sent to the agent/
  );
});

test('Autopilot’s blocking change review: sent and followed up, settled, blocked (S06)', () => {
  const found = { ...base, status: 'findings' as const, blocking: true };
  assert.equal(
    reviewHeadline({ ...found, findings: [finding('high'), finding('low')] }, false),
    'The reviewer found 2 findings: 2 sent to the agent to fix or answer, and a follow-up review checks them.'
  );
  assert.equal(
    reviewHeadline(
      { ...found, round: 3, blocked: true, open: 1, findings: [finding('high')] },
      false
    ),
    'The reviewer still finds 1 problem after the run’s last review: the change goes no further, and the run ends here.'
  );
  const settled = {
    finding: 1,
    severity: 'high' as const,
    location: 'a.ts:1',
    title: 't',
    response: { action: 'fixed' as const, note: 'n' },
    resolved: true,
    checked: true,
    reason: 'it does now',
  };
  assert.equal(
    reviewHeadline({ ...base, status: 'clean', blocking: true, earlier: [settled] }, false),
    'The reviewer found the earlier finding resolved and nothing new to fix.'
  );
});

test('answer_review’s answers in a line (S06)', () => {
  assert.equal(
    reviewAnswersSummary({
      responses: [
        { finding: 1, status: 'fixed', note: 'x' },
        { finding: '2', status: 'not-a-problem', note: 'y' },
        { finding: 3, status: 'maybe' },
      ],
    }),
    '1 fixed, 2 not a problem'
  );
  assert.equal(reviewAnswersSummary({}), undefined);
  assert.equal(reviewAnswersSummary({ responses: 'fixed' }), undefined);
});

test('a change review’s score shows in its line and its facts, as a plain number', () => {
  assert.equal(formatScore(1), '1');
  assert.equal(formatScore(0.125), '0.125');
  assert.equal(formatScore(1.375), '1.375');
  const scored: ReviewMeta = { ...base, status: 'findings', round: 2, score: 1.25 };
  assert.equal(reviewDetails(scored), 'Review 2 of at most 3 · score 1.25');
  assert.deepEqual(
    reviewFacts(scored).find(([label]) => label === 'Score'),
    [
      'Score',
      '1.25: the open findings’ ranks added up (high 1, medium 0.5, low 0.25, halved when plausible)',
    ]
  );
  // A clean review scores 0; a record from before scores has none.
  assert.equal(
    reviewDetails({ ...base, status: 'clean', score: 0 }),
    'Review 1 of at most 3 · score 0'
  );
  assert.equal(reviewDetails({ ...base, status: 'clean' }), 'Review 1 of at most 3');
});

test('the guide’s line says what it did, or why it didn’t', () => {
  assert.equal(
    guideHeadline({ status: 'running' }),
    'The reviewer found problems again: a guide is reading the reviews and the agent’s answers…'
  );
  assert.equal(
    guideHeadline({ status: 'done' }),
    'The guide told the agent where and why it goes wrong.'
  );
  assert.equal(
    guideHeadline({ status: 'done', partial: true }),
    'The guide told the agent where and why it goes wrong (out of budget: what it found so far).'
  );
  assert.equal(
    guideHeadline({ status: 'skipped', reason: 'its model failed (overloaded)' }),
    'Guide skipped: its model failed (overloaded). The agent got the findings alone.'
  );
  assert.equal(guideHeadline({ status: 'stopped' }), 'The guide was stopped.');
  assert.match(guideHeadline({ status: 'running' }, true), /didn’t finish/);
});
