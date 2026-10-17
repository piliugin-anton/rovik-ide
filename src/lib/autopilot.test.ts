// Autopilot's stage records as the chat holds them (lib/autopilot.ts).
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  currentTask,
  pipeline,
  shownStatus,
  stageHover,
  stageRows,
  STAGES,
  stripStages,
  taskLine,
  taskOver,
  taskRecords,
  taskWaits,
  acceptanceHeadline,
  plannerHeadline,
  judgeHeadline,
  breakerHeadline,
  candidateLine,
  candidatesHeadline,
  pickHeadline,
  scoreLine,
  type CandidatesMeta,
  type PickMeta,
  reportDuration,
  reportHeadline,
  reportSpent,
  tokensText,
  testsHeadline,
  unfinishedTask,
  type StageMeta,
  type StageRow,
} from './autopilot';
import type { ChatMessage } from '../types';

let n = 0;
const user = (content: string, meta?: ChatMessage['meta']): ChatMessage => ({
  id: `u${++n}`,
  role: 'user',
  content,
  meta,
});
const stage = (task: string, name: StageMeta['name'], status: StageMeta['status'], extra = {}) =>
  ({
    id: `s${++n}`,
    role: 'assistant',
    content: '',
    meta: { stage: { task, name, status, round: 1, effort: 'balanced', ...extra } },
  }) as ChatMessage;

test('autopilot: Thorough has every stage, Balanced all but Break and Pick', () => {
  assert.deepEqual(pipeline('thorough'), STAGES);
  assert.deepEqual(
    pipeline('balanced'),
    STAGES.filter((s) => s !== 'break' && s !== 'pick')
  );
});

test('autopilot: stage records are read from assistant rows only, and strictly', () => {
  const rows = stageRows([
    user('Do it', { mode: 'autopilot' }),
    stage('t', 'plan', 'done'),
    {
      id: 'x',
      role: 'assistant',
      content: '',
      meta: { stage: { task: 't', name: 'deploy', status: 'done' } },
    } as unknown as ChatMessage,
    {
      id: 'y',
      role: 'user',
      content: '',
      meta: { stage: { task: 't', name: 'plan', status: 'done' } },
    } as unknown as ChatMessage,
    stage('t', 'planReview', 'running'),
  ]);
  assert.deepEqual(
    rows.map((r) => [r.stage.name, r.stage.status]),
    [
      ['plan', 'done'],
      ['planReview', 'running'],
    ]
  );
});

test('autopilot: the current task is the latest, until a message of the user’s comes after it', () => {
  const first = stage('a', 'plan', 'done');
  const messages = [
    user('Task A', { mode: 'autopilot' }),
    first,
    stage('a', 'deliver', 'done'),
    user('Task B', { mode: 'autopilot' }),
    stage('b', 'plan', 'running'),
    user('The IDE’s', { harness: 'autopilot' }),
  ];
  assert.deepEqual(
    currentTask(messages)?.map((r) => r.stage.task),
    ['b']
  );
  assert.equal(unfinishedTask(messages), true);
  const later = [...messages, user('Something else', { mode: 'agent' })];
  assert.equal(currentTask(later), null);
  assert.equal(unfinishedTask(later), false);
  assert.equal(currentTask([user('Hello')]), null);
});

test('autopilot: a task is over once delivered or stopped, unfinished otherwise', () => {
  const over = (...rows: ChatMessage[]) => taskOver(stageRows(rows));
  assert.equal(over(stage('t', 'plan', 'running')), false);
  assert.equal(over(stage('t', 'plan', 'done')), false, 'between two stages');
  assert.equal(over(stage('t', 'plan', 'failed', { outcome: 'plan-failed' })), false, 'to deliver');
  assert.equal(over(stage('t', 'plan', 'failed', { outcome: 'stopped' })), true);
  assert.equal(over(stage('t', 'deliver', 'running')), false);
  assert.equal(over(stage('t', 'deliver', 'done')), true);
  assert.equal(over(stage('t', 'deliver', 'failed')), true);
});

/*
 * The strip (A05)
 */

const rowOf = (m: ChatMessage): StageRow => ({ id: m.id, stage: m.meta!.stage! });

test('the strip: every stage of the effort in order, each as its latest record says', () => {
  const rows = stageRows([
    stage('t', 'plan', 'done', { summary: 'Plan saved' }),
    stage('t', 'planReview', 'skipped', { summary: 'Not available yet' }),
    stage('t', 'implement', 'failed', { outcome: 'error' }),
    stage('t', 'implement', 'running', { round: 2 }),
  ]);
  const strip = stripStages(rows);
  assert.deepEqual(
    strip.map((s) => s.name),
    pipeline('balanced')
  );
  const by = (name: string) => strip.find((s) => s.name === name)!;
  assert.deepEqual(by('plan'), {
    name: 'plan',
    label: 'Plan',
    status: 'done',
    summary: 'Plan saved',
    recordId: rows[0].id,
  });
  assert.equal(by('planReview').status, 'skipped');
  // Its second run: the latest record, with its round.
  assert.equal(by('implement').status, 'running');
  assert.equal(by('implement').round, 2);
  assert.equal(by('implement').recordId, rows[3].id);
  // Not reached yet: nothing to go to.
  assert.deepEqual(by('checks'), { name: 'checks', label: 'Checks', status: 'pending' });
  // Thorough has Break and Pick.
  const thorough = stripStages(stageRows([stage('t', 'plan', 'running', { effort: 'thorough' })]));
  assert.deepEqual(
    thorough.map((s) => s.name),
    STAGES
  );
});

test('the strip: a stage recorded running spins while a run goes, else it didn’t finish', () => {
  assert.equal(shownStatus('running', true), 'running');
  assert.equal(shownStatus('running', false), 'stopped');
  assert.equal(shownStatus('done', false), 'done');
  assert.equal(shownStatus('pending', false), 'pending');
  const [plan] = stripStages(stageRows([stage('t', 'plan', 'running')]));
  assert.equal(stageHover(plan, true), 'Plan: running');
  assert.equal(stageHover(plan, false), 'Plan: didn’t finish');
  const [again] = stripStages(
    stageRows([stage('t', 'plan', 'failed', { round: 3, summary: 'The run ended without a plan' })])
  );
  assert.equal(stageHover(again, false), 'Plan, round 3: failed. The run ended without a plan');
});

test('the strip: a stage that waits out the provider’s rate limit waits, nothing cut it', () => {
  assert.equal(shownStatus('running', false, true), 'waiting');
  assert.equal(shownStatus('running', true, true), 'running');
  assert.equal(shownStatus('done', false, true), 'done');
  const rateLimit = { until: 1_700_000_000_000, waits: 1, waitedMs: 60_000 };
  const summary = 'Rate limited by the provider: again in 1 min';
  const [plan] = stripStages(stageRows([stage('t', 'plan', 'running', { rateLimit, summary })]));
  assert.equal(plan.waiting, true);
  assert.equal(stageHover(plan, false), `Plan: waiting. ${summary}`);
  assert.equal(stageHover(plan, true), `Plan: running. ${summary}`);
  // Gone on: its run goes, or a crash cut it.
  const [gone] = stripStages(
    stageRows([stage('t', 'plan', 'running', { rateLimit: { waits: 1, waitedMs: 60_000 } })])
  );
  assert.equal(gone.waiting, undefined);
  assert.equal(stageHover(gone, false), 'Plan: didn’t finish');
  assert.equal(
    taskLine(stageRows([stage('t', 'plan', 'running', { rateLimit })]), false),
    'Autopilot: plan waits out the provider’s rate limit'
  );
  assert.equal(
    taskLine(stageRows([stage('t', 'plan', 'running', { rateLimit })]), true),
    'Autopilot: plan…'
  );
  // A waiting task can be stopped, though no run goes.
  assert.equal(taskWaits(stageRows([stage('t', 'plan', 'running', { rateLimit })])), true);
  assert.equal(
    taskWaits(stageRows([stage('t', 'plan', 'running', { rateLimit: { waits: 1, waitedMs: 1 } })])),
    false
  );
  assert.equal(taskWaits(null), false);
});

test('the strip’s line: the stage at work, or how the task ended', () => {
  const line = (running: boolean, ...messages: ChatMessage[]) =>
    taskLine(stageRows(messages), running);
  assert.equal(line(true, stage('t', 'plan', 'running')), 'Autopilot: plan…');
  assert.equal(line(false, stage('t', 'plan', 'running')), 'Autopilot: plan didn’t finish');
  assert.equal(line(false, stage('t', 'plan', 'done')), 'Autopilot: waiting to go on');
  assert.equal(
    line(false, stage('t', 'plan', 'done'), stage('t', 'deliver', 'done')),
    'Autopilot: delivered'
  );
  assert.equal(
    line(
      false,
      stage('t', 'plan', 'failed', { outcome: 'plan-failed' }),
      stage('t', 'deliver', 'failed', { outcome: 'plan-failed' })
    ),
    'Autopilot: plan failed, not delivered'
  );
  assert.equal(
    line(false, stage('t', 'plan', 'done'), stage('t', 'deliver', 'failed')),
    'Autopilot: not delivered'
  );
  assert.equal(
    line(false, stage('t', 'implement', 'failed', { outcome: 'stopped' })),
    'Autopilot: stopped during implement'
  );
  // A stage a Retry ran again is its later round's to say, as the backend's
  // task_outcome reads it.
  const retried = (...after: ChatMessage[]) =>
    line(
      false,
      stage('t', 'implement', 'failed', { outcome: 'stopped' }),
      stage('t', 'implement', 'done', { round: 2, outcome: 'verified' }),
      ...after
    );
  assert.equal(retried(stage('t', 'deliver', 'done')), 'Autopilot: delivered');
  assert.equal(
    retried(
      stage('t', 'review', 'failed', { outcome: 'review-open' }),
      stage('t', 'deliver', 'failed', { outcome: 'review-open' })
    ),
    'Autopilot: change review failed, not delivered'
  );
  // Delivered as partial (D7, S09): not landed, and said so.
  assert.equal(
    line(
      false,
      stage('t', 'acceptance', 'failed', { outcome: 'unmet' }),
      stage('t', 'deliver', 'done', { outcome: 'unmet' })
    ),
    'Autopilot: delivered as partial'
  );
});

test('the strip reads the stored records, with the loaded ones over them', () => {
  const plan = stage('t', 'plan', 'done');
  const implement = stage('t', 'implement', 'running');
  // The task's plan is older than the page held; its implementation is held, and newer.
  const stored = [rowOf(plan), rowOf(stage('t', 'implement', 'running'))];
  stored[1].id = implement.id;
  const done = { ...implement, meta: { stage: { ...implement.meta!.stage!, status: 'done' } } };
  const held = [done as ChatMessage, stage('t', 'checks', 'running')];
  const rows = taskRecords(stored, held)!;
  assert.deepEqual(
    rows.map((r) => [r.stage.name, r.stage.status]),
    [
      ['plan', 'done'],
      ['implement', 'done'],
      ['checks', 'running'],
    ]
  );
  // Nothing read yet: the loaded ones.
  assert.equal(taskRecords(null, held)?.length, 2);
  // A newer task than the one read: the loaded one, alone.
  const next = [user('Task B', { mode: 'autopilot' }), stage('b', 'plan', 'running')];
  assert.deepEqual(
    taskRecords(stored, next)?.map((r) => r.stage.task),
    ['b']
  );
  // No task anywhere.
  assert.equal(taskRecords(null, [user('Hello')]), null);
});

test('the strip: none of its records held, it’s current until a request of the user’s', () => {
  const stored = [rowOf(stage('t', 'plan', 'done')), rowOf(stage('t', 'implement', 'running'))];
  // A long implementation: the page held is its replies, and the IDE's messages.
  const replies: ChatMessage[] = [
    { id: 'a1', role: 'assistant', content: 'Working on it' },
    user('Checks failed', { harness: 'autopilot' }),
  ];
  assert.equal(taskRecords(stored, replies)?.length, 2);
  assert.equal(taskRecords(stored, [...replies, user('Something else')]), null);
  // A message the user's that wasn't delivered doesn't count.
  const failed = { ...user('Not sent'), failed: true };
  assert.equal(taskRecords(stored, [...replies, failed])?.length, 2);
});

test('the strip: opened around a search hit, the stored task is the chat’s, not the page’s', () => {
  const old = [user('Task A', { mode: 'autopilot' }), stage('a', 'plan', 'done')];
  const stored = [rowOf(stage('b', 'plan', 'running'))];
  assert.deepEqual(
    taskRecords(stored, old, false)?.map((r) => r.stage.task),
    ['b']
  );
  assert.equal(taskRecords(null, old, false), null);
  // Held messages that are the chat's newest end with its latest task: a read
  // of another one is passed over.
  assert.deepEqual(
    taskRecords(stored, old)?.map((r) => r.stage.task),
    ['a']
  );
});

test('the Tests record’s headline: at work, what it wrote, what passes already (S04)', () => {
  const files = ['test/a.test.js', 'test/b.test.js'];
  assert.equal(
    testsHeadline({ status: 'running' }, false),
    'Writing the tests before the code, from the plan’s criteria…'
  );
  assert.equal(
    testsHeadline({ status: 'running' }, true),
    'Writing the tests didn’t finish: the run ended meanwhile.'
  );
  assert.equal(
    testsHeadline({ status: 'done', files, passing: [] }, false),
    'Wrote 2 test files before the code: the change is made to pass them.'
  );
  assert.equal(
    testsHeadline({ status: 'done', files, passing: ['test/b.test.js'] }, false),
    'Wrote 2 test files before the code; 1 of them already passes, so it may not test the change.'
  );
  assert.equal(
    testsHeadline({ status: 'done', files: files.slice(0, 1), passing: files.slice(0, 1) }, false),
    'Wrote 1 test file before the code; it passes already, so it may not test the change.'
  );
  assert.equal(
    testsHeadline(
      { status: 'blocked', reason: 'The test writer was blocked: No interface to test' },
      false
    ),
    'The test writer was blocked: No interface to test. The task goes on without tests written first.'
  );
  // Blocked once it wrote some: they stay the task's.
  assert.equal(
    testsHeadline(
      { status: 'blocked', reason: 'The test writer was blocked: Out of ideas', files },
      false
    ),
    'The test writer was blocked: Out of ideas, after it wrote 2 test files: the change is made to pass them.'
  );
  assert.equal(
    testsHeadline({ status: 'stopped' }, false),
    'Stopped while the tests were written.'
  );
});

test('the Tests record after a change that came with none (S07)', () => {
  const after = { after: true, files: ['src/util.test.js'] };
  assert.equal(
    testsHeadline({ ...after, status: 'running' }, false),
    'The change came with no tests: writing them…'
  );
  assert.equal(
    testsHeadline({ ...after, status: 'done' }, false),
    'The change came with no tests: wrote 1 test file, and the checks run it next.'
  );
  assert.equal(
    testsHeadline(
      { after: true, status: 'blocked', reason: 'The test writer was blocked: Nothing to test' },
      false
    ),
    'The test writer was blocked: Nothing to test. The checks’ result stands.'
  );
});

test('the acceptance record’s headline: judging, met, sent back, unmet, skipped (S08)', () => {
  const base = { round: 1, maxRounds: 2, criteria: ['a', 'b'] };
  const verdicts = [
    { criterion: 1, satisfied: true, note: 'x' },
    { criterion: 2, satisfied: false, note: 'y' },
  ];
  assert.equal(
    acceptanceHeadline({ ...base, status: 'running' }, false),
    'Checking the change against the plan’s 2 criteria…'
  );
  assert.equal(
    acceptanceHeadline({ ...base, status: 'running' }, true),
    'The acceptance check didn’t finish: the run ended meanwhile.'
  );
  assert.equal(
    acceptanceHeadline({ ...base, status: 'satisfied' }, false),
    'The change meets all 2 of the plan’s criteria.'
  );
  assert.equal(
    acceptanceHeadline({ ...base, criteria: ['a'], status: 'satisfied' }, false),
    'The change meets the plan’s criterion.'
  );
  assert.equal(
    acceptanceHeadline({ ...base, status: 'unsatisfied', verdicts }, false),
    '1 of 2 criteria unmet: sent to the agent for one more round.'
  );
  assert.equal(
    acceptanceHeadline({ ...base, round: 2, status: 'unsatisfied', verdicts, unmet: true }, false),
    '1 of 2 criteria still unmet after the last check: the task ends unmet.'
  );
  assert.equal(
    acceptanceHeadline(
      { ...base, status: 'skipped', reason: 'its verdicts couldn’t be read' },
      false
    ),
    'Acceptance check skipped: its verdicts couldn’t be read. The checks decide.'
  );
  // R03: after a check that found criteria unmet, nothing confirmed their fix.
  const unconfirmed = { ...base, round: 2, status: 'skipped' as const, unmet: true };
  assert.equal(
    acceptanceHeadline(
      { ...unconfirmed, reason: 'its verdicts couldn’t be read', earlierUnmet: 1 },
      false
    ),
    'Acceptance check skipped: its verdicts couldn’t be read. The criterion the check before found unmet stands: the task ends unmet.'
  );
  assert.equal(
    acceptanceHeadline({ ...unconfirmed, earlierUnmet: 2 }, false),
    'Acceptance check skipped: it couldn’t judge. The 2 criteria the check before found unmet stand: the task ends unmet.'
  );
});

test('a planner record’s headline: at work, its plan, taken or flagged, none (T01)', () => {
  const base = { role: 'plan' as const, candidate: 'b' };
  assert.equal(
    plannerHeadline({ ...base, status: 'running' }, false),
    'Planner B is planning the task on its own…'
  );
  assert.equal(
    plannerHeadline({ ...base, status: 'running' }, true),
    'Planner B didn’t finish: the run ended meanwhile.'
  );
  const done = { ...base, status: 'done' as const, title: 'Greet', steps: 3 };
  assert.equal(plannerHeadline(done, false), 'Planner B planned “Greet” in 3 steps.');
  assert.equal(
    plannerHeadline({ ...done, chosen: true }, false),
    'Planner B planned “Greet” in 3 steps: the task takes this plan.'
  );
  assert.equal(
    plannerHeadline({ ...done, steps: 1, problems: ['Step 1 names a file there isn’t'] }, false),
    'Planner B planned “Greet” in 1 step, but the plan’s checks find problems in it.'
  );
  assert.equal(
    plannerHeadline({ ...base, status: 'skipped', reason: 'its report has no plan' }, false),
    'Planner B gave no plan: its report has no plan.'
  );
  assert.equal(plannerHeadline({ ...base, status: 'stopped' }, false), 'Planner B was stopped.');
});

test('a judge record’s headline: weighing, picked, revised, merged, no choice (T02)', () => {
  const base = { role: 'plans' as const, offered: ['a', 'b', 'c'] };
  assert.equal(
    judgeHeadline({ ...base, status: 'running' }, false),
    'The judge is weighing the 3 plans against the code…'
  );
  assert.equal(
    judgeHeadline({ ...base, status: 'running' }, true),
    'The judge didn’t finish: the run ended meanwhile.'
  );
  const done = { ...base, status: 'done' as const, title: 'Greet', steps: 2 };
  assert.equal(
    judgeHeadline({ ...done, choice: 'b' }, false),
    'The judge took planner B’s plan, “Greet”, 2 steps.'
  );
  assert.equal(
    judgeHeadline({ ...done, choice: 'b', revised: true, steps: 1 }, false),
    'The judge took planner B’s plan, revised: “Greet”, 1 step.'
  );
  assert.equal(
    judgeHeadline({ ...done, choice: 'merged', revised: true }, false),
    'The judge merged the plans into one: “Greet”, 2 steps.'
  );
  assert.equal(
    judgeHeadline(
      { ...base, status: 'skipped', reason: 'its report couldn’t be read', fallback: 'a' },
      false
    ),
    'The judge gave no choice: its report couldn’t be read. The task takes planner A’s plan.'
  );
  assert.equal(
    judgeHeadline({ ...base, status: 'skipped', reason: 'its model failed' }, false),
    'The judge gave no choice: its model failed. The agent plans the task itself.'
  );
  assert.equal(judgeHeadline({ ...base, status: 'stopped' }, false), 'The judge was stopped.');
});

test('criteria records’ headlines: a writer’s set, the judge’s merge (T03)', () => {
  const writer = { role: 'criteria' as const, candidate: 'a' };
  assert.equal(
    plannerHeadline({ ...writer, status: 'running' }, false),
    'Criteria writer A is writing what done means, on its own…'
  );
  assert.equal(
    plannerHeadline({ ...writer, status: 'done', criteria: ['x', 'y'] }, false),
    'Criteria writer A wrote 2 criteria.'
  );
  assert.equal(
    plannerHeadline({ ...writer, status: 'skipped', reason: 'its model failed' }, false),
    'Criteria writer A gave no criteria: its model failed.'
  );
  const merge = { role: 'criteria' as const, offered: ['a', 'b'] };
  assert.equal(
    judgeHeadline({ ...merge, status: 'running' }, false),
    'The judge is merging the 2 sets of criteria…'
  );
  assert.equal(
    judgeHeadline({ ...merge, status: 'done', criteria: ['x'] }, false),
    'The judge merged the 2 sets into 1 criterion.'
  );
  assert.equal(
    judgeHeadline(
      { ...merge, status: 'skipped', reason: 'its report couldn’t be read', fallback: 'a' },
      false
    ),
    'The judge couldn’t merge the criteria: its report couldn’t be read. The task takes writer A’s criteria.'
  );
});

test('the adversarial tester’s headline: at work, found a fault, passed, none (T04)', () => {
  assert.equal(
    breakerHeadline({ status: 'running' }, false),
    'The checks pass: an adversarial tester is trying to break the change…'
  );
  assert.equal(
    breakerHeadline({ status: 'running' }, true),
    'The adversarial tester didn’t finish: the run ended meanwhile.'
  );
  assert.equal(
    breakerHeadline({ status: 'done', files: ['a.test.js'] }, false),
    'The adversarial tester wrote 1 test file; the checks run them next.'
  );
  assert.equal(
    breakerHeadline({ status: 'done', files: ['a.test.js', 'b.test.js'], found: true }, false),
    'The adversarial tester’s 2 test files found a fault: the agent fixes the change to pass them.'
  );
  assert.equal(
    breakerHeadline({ status: 'done', files: ['a.test.js'], found: false }, false),
    'The change passes the adversarial tester’s 1 test file.'
  );
  assert.equal(
    breakerHeadline({ status: 'blocked', reason: 'it couldn’t test the change: no API' }, false),
    'The adversarial tester left no test: it couldn’t test the change: no API.'
  );
  // Blocked with tests written: they stay.
  assert.equal(
    breakerHeadline({ status: 'blocked', files: ['a.test.js'], found: false }, false),
    'The change passes the adversarial tester’s 1 test file.'
  );
});

test('the candidates’ headline and lines: one at work, both done, one implementer (T05)', () => {
  const both: CandidatesMeta = {
    task: 't',
    round: 1,
    status: 'running',
    candidates: [
      { letter: 'a', chat: 'c1', status: 'done', outcome: 'verified', summary: '1 check passed' },
      { letter: 'b', chat: 'c2', status: 'running' },
    ],
  };
  assert.equal(
    candidatesHeadline(both, false),
    'Two implementers, one after the other: candidate B is implementing the plan in its own copy…'
  );
  assert.equal(
    candidatesHeadline(both, true),
    'The candidates didn’t finish: the run ended meanwhile.'
  );
  const done: CandidatesMeta = {
    ...both,
    status: 'done',
    candidates: [both.candidates![0], { ...both.candidates![1], status: 'done' }],
  };
  assert.equal(
    candidatesHeadline(done, false),
    'Both candidates implemented the plan, each in its own copy: the pick takes the better.'
  );
  const one: CandidatesMeta = {
    ...done,
    candidates: [done.candidates![0], { ...done.candidates![1], status: 'failed' }],
  };
  assert.equal(
    candidatesHeadline(one, false),
    '1 of 2 candidates implemented the plan: the pick takes the better.'
  );
  assert.equal(
    candidatesHeadline({ ...both, status: 'stopped' }, false),
    'The candidates were stopped.'
  );
  assert.equal(
    candidatesHeadline(
      {
        task: 't',
        round: 1,
        status: 'single',
        reason: 'the chat works in the project folder itself',
      },
      false
    ),
    'One implementer: the chat works in the project folder itself.'
  );
  assert.equal(candidateLine(both.candidates![0]), 'A: done · 1 check passed');
  assert.equal(candidateLine(both.candidates![1]), 'B: implementing…');
});

test('the pick’s headline and score lines: by score, by the judge, failed (T06)', () => {
  const score = {
    changed: true,
    finished: true,
    testsPassed: 2,
    checksPassed: 1,
    openFindings: 0,
    diffLines: 12,
  };
  const pick: PickMeta = {
    task: 't',
    status: 'done',
    winner: 'a',
    judged: false,
    adopted: true,
    candidates: [
      { letter: 'a', chat: 'c1', score, files: ['ok.txt'] },
      {
        letter: 'b',
        chat: 'c2',
        score: { ...score, diffLines: 40 },
        files: ['ok.txt'],
        diff: '--- ok.txt',
      },
    ],
  };
  assert.equal(pickHeadline(pick, false), 'Candidate A’s change is the task’s: it scored better.');
  assert.equal(
    pickHeadline({ ...pick, winner: 'b', judged: true }, false),
    'Candidate B’s change is the task’s: the scores tied, and the judge picked it.'
  );
  assert.equal(
    pickHeadline(
      { ...pick, judged: true, why: 'the judge is off; the first candidate is taken' },
      false
    ),
    'Candidate A’s change is the task’s: the scores tied, and the judge is off; the first candidate is taken.'
  );
  assert.equal(
    pickHeadline(
      { ...pick, status: 'failed', reason: 'Candidate A’s change couldn’t be taken: busy' },
      false
    ),
    'The pick failed: Candidate A’s change couldn’t be taken: busy.'
  );
  assert.equal(
    pickHeadline({ ...pick, status: 'running' }, true),
    'The pick didn’t finish: the run ended meanwhile.'
  );
  assert.equal(
    scoreLine(score),
    '2 tests written first pass · 1 check passes · no findings open · 12 lines changed'
  );
  assert.equal(
    scoreLine({
      ...score,
      finished: false,
      testsPassed: 1,
      checksPassed: 0,
      openFindings: 1,
      diffLines: 1,
    }),
    'its run didn’t finish · 1 test written first passes · 0 checks pass · 1 finding open · 1 line changed'
  );
  assert.equal(scoreLine({ ...score, changed: false }), 'no change');
});

test('the judge’s headline on tied candidates (T06)', () => {
  const base = { role: 'candidates' as const, offered: ['a', 'b'] };
  assert.equal(
    judgeHeadline({ ...base, status: 'running' }, false),
    'The candidates’ scores tie: the judge is weighing their changes…'
  );
  assert.equal(
    judgeHeadline({ ...base, status: 'done', choice: 'b' }, false),
    'The judge picked candidate B’s change.'
  );
  assert.equal(
    judgeHeadline(
      { ...base, status: 'skipped', reason: 'its verdict names neither candidate' },
      false
    ),
    'The judge didn’t decide: its verdict names neither candidate. The first candidate is taken.'
  );
});

test('the report card’s lines: how it ended, what it spent (S09)', () => {
  const meta = {
    outcome: 'verified',
    ending: 'Its checks pass',
    delivered: true,
    partial: false,
    delivery: 'Delivered: the changes are in the project',
    assumptions: [],
    criteria: [],
    verdicts: [],
    tests: [],
    checks: [],
    spent: { ms: 125_000, cost: 0.4249, uncosted: 0 },
  };
  assert.equal(reportHeadline(meta), 'Task done: its checks pass.');
  assert.equal(
    reportHeadline({ ...meta, delivered: false, partial: true, ending: 'Over the task’s budget' }),
    'Task delivered as partial: over the task’s budget.'
  );
  assert.equal(
    reportHeadline({ ...meta, delivered: false, ending: 'The checks still fail' }),
    'Task ended: the checks still fail.'
  );
  assert.equal(reportSpent(meta.spent), '2 min, 0.42');
  assert.equal(reportSpent({ ms: 1000, cost: 0.1, uncosted: 2 }), '1 s, at least 0.10');
  assert.equal(reportSpent({ ms: 1000, cost: 0, uncosted: 2 }), '1 s; no cost reported');
  // The tokens, where the providers said.
  assert.equal(
    reportSpent({ ms: 1000, cost: 0, uncosted: 2, tokens: 2_150_000 }),
    '1 s; no cost reported; 2.15 million tokens'
  );
  assert.equal(
    reportSpent({ ms: 1000, cost: 0.1, uncosted: 0, tokens: 45_200 }),
    '1 s, 0.10; 45,200 tokens'
  );
  assert.equal(tokensText(330), '330');
  assert.equal(tokensText(1_000_000), '1 million');
  assert.equal(tokensText(12_500_000), '12.5 million');
  assert.equal(reportDuration(3_900_000), '1 h 5 min');
});
