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
  keptStage,
  runningCandidate,
  withLiveStages,
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
  // A message sent to a run while it worked is part of its request, not a new one.
  const steered = [...messages, user('Use the HTTP client', { steer: { queuedAt: 1 } })];
  assert.deepEqual(
    currentTask(steered)?.map((r) => r.stage.task),
    ['b']
  );
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
  // T05: a candidate's records end at a failed stage; its task's chat goes on.
  const theirs = (status: 'done' | 'failed') =>
    over(
      stage('t', 'implement', status, { candidate: 'a', outcome: 'error' }),
      stage('t', 'review', 'skipped', { candidate: 'a' })
    );
  assert.equal(theirs('done'), false);
  assert.equal(
    over(stage('t', 'implement', 'failed', { candidate: 'a', outcome: 'error' })),
    true,
    'not waiting to go on'
  );
});

test('the strip: a rate limit the implementation waits out shows on the stage it got to (H07)', () => {
  const before = [stage('t', 'plan', 'done'), stage('t', 'planReview', 'done')];
  const until = Date.now() + 60_000;
  const implement = stage('t', 'implement', 'running', {
    rateLimit: { until, waits: 1, waitedMs: 60_000 },
  });
  const rows = withLiveStages(stageRows([...before, implement]), [
    ...before,
    implement,
    review('findings'),
  ]);
  assert.equal(taskWaits(rows), true);
  assert.equal(
    taskLine(rows, false),
    'Autopilot: change review waits out the provider’s rate limit'
  );
  const shown = stripStages(rows).find((s) => s.name === 'review')!;
  assert.equal(shown.waiting, true);
  assert.equal(shownStatus(shown.status, false, shown.waiting), 'waiting');
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

test('the strip: a done stage some of whose steps failed shows a warning, not a tick', () => {
  assert.equal(shownStatus('done', false, false, 3), 'warning');
  assert.equal(shownStatus('done', true, false, 1), 'warning');
  assert.equal(shownStatus('done', false, false, 0), 'done');
  // Only a done stage: the others say how they are.
  assert.equal(shownStatus('running', true, false, 1), 'running');
  assert.equal(shownStatus('skipped', false, false, 1), 'skipped');
  const summary = '3 of 3 installs failed (`npm ci`, `cargo fetch` in api): npm wasn’t found';
  const rows = stageRows([
    stage('t', 'plan', 'done'),
    stage('t', 'planReview', 'done'),
    stage('t', 'prepare', 'done', { failures: 3, summary }),
  ]);
  const prepare = stripStages(rows).find((s) => s.name === 'prepare')!;
  assert.equal(prepare.failures, 3);
  assert.equal(stageHover(prepare, true), `Prepare: done, with failures. ${summary}`);
  // Installed: a tick, and no count.
  const [, , installed] = stripStages(
    stageRows([
      stage('t', 'plan', 'done'),
      stage('t', 'planReview', 'done'),
      stage('t', 'prepare', 'done', { summary: 'Installed: `npm ci`' }),
    ])
  );
  assert.equal(installed.failures, undefined);
  assert.equal(stageHover(installed, true), 'Prepare: done. Installed: `npm ci`');
  // It doesn't end the task: the strip's line goes on.
  assert.equal(taskLine(rows, true), 'Autopilot');
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

test('the strip: a patch of a record the chat doesn’t hold is laid over the stored one', () => {
  const plan = stage('t', 'plan', 'done');
  const implement = stage('t', 'implement', 'running');
  // A remote chat's mirror: the implementation's record as it began.
  const stored = [rowOf(plan), rowOf(implement)];
  // The page held is the run's replies; the record ended on the host since.
  const held: ChatMessage[] = [{ id: 'a1', role: 'assistant', content: 'Done' }];
  const ended = { ...implement.meta!.stage!, status: 'done' as const, outcome: 'verified' };
  const rows = taskRecords(stored, held, true, { [implement.id]: ended })!;
  assert.deepEqual(
    rows.map((r) => [r.stage.name, r.stage.status]),
    [
      ['plan', 'done'],
      ['implement', 'done'],
    ]
  );
  // A pull that read the host before the record ended doesn't take it back.
  const doneStored = [rowOf(plan), { id: implement.id, stage: ended }];
  const stale = implement.meta!.stage!;
  assert.equal(
    taskRecords(doneStored, held, true, { [implement.id]: stale })![1].stage.status,
    'done'
  );
});

test('a record that ended never goes back to running', () => {
  const running = stage('t', 'implement', 'running').meta!.stage!;
  const done = { ...running, status: 'done' as const };
  assert.equal(keptStage(running, done), done);
  assert.equal(keptStage(done, running), done);
  assert.equal(keptStage(undefined, running), running);
  const failed = { ...running, status: 'failed' as const };
  assert.equal(keptStage(done, failed), failed);
});

/** A record of the implementation's run: a change review, a check round, an acceptance check. */
const inRun = (meta: ChatMessage['meta']): ChatMessage => ({
  id: `r${++n}`,
  role: 'assistant',
  content: '',
  meta,
});
const review = (status: string, kind = 'change') =>
  inRun({ review: { kind, status, round: 1, findings: [] } as never });
const checks = (status: string, round = 1) =>
  inRun({ verification: { status, round, maxRounds: 3, checks: [], changedFiles: [] } as never });
const acceptance = (status: string) =>
  inRun({ acceptance: { status, round: 1, maxRounds: 2, criteria: [] } as never });

test('the strip: the stages done inside the implementation show as its run gets to them', () => {
  const before = [
    stage('t', 'plan', 'done'),
    stage('t', 'planReview', 'done'),
    stage('t', 'prepare', 'skipped'),
    stage('t', 'tests', 'skipped'),
  ];
  const implement = stage('t', 'implement', 'running');
  const records = stageRows([...before, implement]);
  const shown = (...run: ChatMessage[]) => {
    const rows = withLiveStages(records, [...before, implement, ...run]);
    const statuses = Object.fromEntries(stripStages(rows).map((s) => [s.name, s.status]));
    return { line: taskLine(rows, true), statuses, rows };
  };
  const reply: ChatMessage = { id: 'a1', role: 'assistant', content: 'Changing it' };
  // Writing the change: as recorded.
  assert.equal(shown(reply).line, 'Autopilot: implement…');
  assert.deepEqual(shown(reply).rows, records);
  // The change review: its stage at work, the implementation done meanwhile.
  let now = shown(reply, review('running'));
  assert.equal(now.line, 'Autopilot: change review…');
  assert.equal(now.statuses.implement, 'done');
  assert.equal(now.statuses.review, 'running');
  assert.equal(now.statuses.checks, 'pending');
  // It found something, which the agent fixes: still the review's.
  now = shown(reply, review('findings'), reply);
  assert.equal(now.line, 'Autopilot: change review…');
  assert.equal(now.statuses.implement, 'done');
  assert.equal(now.statuses.review, 'running');
  // The checks: the review is done.
  now = shown(review('findings'), checks('running'));
  assert.equal(now.line, 'Autopilot: checks…');
  assert.equal(now.statuses.review, 'done');
  assert.equal(now.statuses.checks, 'running');
  // A round that failed, which the agent fixes: still the checks'.
  now = shown(review('findings'), checks('failed'), reply);
  assert.equal(now.line, 'Autopilot: checks…');
  assert.equal(now.statuses.checks, 'running');
  // Green after a fix, the reviewer reads the change again (R08): still the checks'.
  now = shown(review('findings'), checks('failed'), checks('passed', 2), review('running'));
  assert.equal(now.line, 'Autopilot: checks…');
  assert.equal(now.statuses.review, 'done');
  // The acceptance check at work: the checks done, in the stage's first
  // round in the task (their second round of checks is their record's).
  now = shown(review('clean'), checks('failed'), checks('passed', 2), acceptance('running'));
  assert.equal(now.line, 'Autopilot: acceptance…');
  assert.deepEqual(
    stripStages(now.rows)
      .filter((s) => s.name === 'checks')
      .map((s) => [s.status, s.round]),
    [['done', undefined]]
  );
  // A click goes to the record at work.
  const acceptanceRow = now.rows[now.rows.length - 1];
  assert.equal(
    stripStages(now.rows).find((s) => s.name === 'acceptance')!.recordId,
    acceptanceRow.id
  );
  // A reviewer that didn't run: skipped, not ticked.
  assert.equal(shown(review('skipped'), checks('running')).statuses.review, 'skipped');
});

test('the strip: a Thorough task’s stages inside the implementation, from its running candidate (T05)', () => {
  const before = [stage('t', 'plan', 'done'), stage('t', 'planReview', 'done')];
  const implement = stage('t', 'implement', 'running');
  const records = stageRows([...before, implement]);
  /** The task chat's record of its candidates, a and b. */
  const candidates = (status: string, a: string, b: string): ChatMessage => ({
    id: 'k1',
    role: 'assistant',
    content: '',
    meta: {
      candidates: {
        task: 't',
        round: 1,
        status,
        candidates: [
          { letter: 'a', chat: 'cand-a', status: a },
          { letter: 'b', chat: 'cand-b', status: b },
        ],
      } as never,
    },
  });
  // Candidate a at work: the task's chat has its record, a's chat the rest.
  const messages = [...before, implement, candidates('running', 'running', 'waiting')];
  assert.deepEqual(runningCandidate(messages), { recordId: 'k1', letter: 'a', chat: 'cand-a' });
  const theirs = { recordId: 'k1', messages: [review('clean'), checks('running')] };
  const rows = withLiveStages(records, messages, theirs);
  const statuses = Object.fromEntries(stripStages(rows).map((s) => [s.name, s.status]));
  assert.equal(taskLine(rows, true), 'Autopilot: checks…');
  assert.deepEqual(
    [statuses.implement, statuses.review, statuses.checks],
    ['done', 'done', 'running']
  );
  // A click shows the candidates' record, which is in the task's chat.
  assert.equal(stripStages(rows).find((s) => s.name === 'checks')!.recordId, 'k1');
  // Without its messages (not loaded yet): as recorded.
  assert.deepEqual(withLiveStages(records, messages), records);
  // b's turn, then none at work: the one to follow, then none.
  const at = (record: ChatMessage) => runningCandidate([...before, implement, record]);
  assert.equal(at(candidates('running', 'done', 'running'))?.chat, 'cand-b');
  assert.equal(at(candidates('done', 'done', 'done')), null);
  assert.equal(runningCandidate(before), null);
  // Only the implementation's: the plan's review is the plan's run's.
  const planning = stageRows([stage('t', 'plan', 'running')]);
  const reviewing = { recordId: 'k1', messages: [review('running', 'plan')] };
  assert.deepEqual(withLiveStages(planning, messages, reviewing), planning);
});

test('the strip: the implementation’s stages stay as they showed while the driver records them', () => {
  const before = [stage('t', 'plan', 'done'), stage('t', 'planReview', 'done')];
  const implement = stage('t', 'implement', 'running');
  const run = [review('clean'), checks('passed'), acceptance('satisfied')];
  // The run ended: its record, then the driver's of the stages inside it,
  // one after another.
  const ended = { ...implement, meta: { stage: { ...implement.meta!.stage!, status: 'done' } } };
  const messages = [...before, ended as ChatMessage, ...run];
  const reviewed = stageRows([...before, ended as ChatMessage, stage('t', 'review', 'done')]);
  const rows = withLiveStages(reviewed, messages);
  assert.deepEqual(
    stripStages(rows)
      .filter((s) => ['implement', 'review', 'checks', 'acceptance'].includes(s.name))
      .map((s) => [s.name, s.status]),
    [
      ['implement', 'done'],
      ['review', 'done'],
      ['checks', 'done'],
      ['acceptance', 'running'],
    ]
  );
  // The task went on: as recorded.
  const delivering = stageRows([
    ...before,
    ended as ChatMessage,
    stage('t', 'review', 'done'),
    stage('t', 'checks', 'done'),
    stage('t', 'acceptance', 'done'),
    stage('t', 'deliver', 'running'),
  ]);
  assert.equal(withLiveStages(delivering, messages), delivering);
});

test('the strip: a stage only goes forward as the implementation’s run goes on', () => {
  const before = [stage('t', 'plan', 'done'), stage('t', 'planReview', 'done')];
  const implement = stage('t', 'implement', 'running');
  const ended = { ...implement, meta: { stage: { ...implement.meta!.stage!, status: 'done' } } };
  const reply = (): ChatMessage => ({ id: `a${++n}`, role: 'assistant', content: 'Fixing' });
  const r1 = review('running');
  const c1 = checks('running');
  const c2 = checks('running', 2);
  const r2 = review('running');
  const a1 = acceptance('running');
  const as = (m: ChatMessage, status: string) =>
    ({
      ...m,
      meta: Object.fromEntries(
        Object.entries(m.meta!).map(([k, v]) => [k, { ...(v as object), status }])
      ),
    }) as ChatMessage;
  const records = (...more: ChatMessage[]) => stageRows([...before, ...more]);
  // Each moment of the task: its records, and the messages the chat holds.
  const states: [StageRow[], ChatMessage[]][] = [
    [records(implement), [reply()]],
    [records(implement), [r1]],
    [records(implement), [as(r1, 'findings'), reply()]],
    [records(implement), [as(r1, 'findings'), c1]],
    [records(implement), [as(r1, 'findings'), as(c1, 'failed'), reply()]],
    [records(implement), [as(r1, 'findings'), as(c1, 'failed'), c2]],
    [records(implement), [as(r1, 'findings'), as(c1, 'failed'), as(c2, 'passed'), r2]],
    [
      records(implement),
      [as(r1, 'findings'), as(c1, 'failed'), as(c2, 'passed'), as(r2, 'clean'), a1],
    ],
    [records(ended as ChatMessage), [as(r1, 'findings'), as(c2, 'passed'), as(a1, 'satisfied')]],
    [
      records(ended as ChatMessage, stage('t', 'review', 'done')),
      [as(r1, 'findings'), as(c2, 'passed'), as(a1, 'satisfied')],
    ],
    [
      records(ended as ChatMessage, stage('t', 'review', 'done'), stage('t', 'checks', 'done')),
      [as(r1, 'findings'), as(c2, 'passed'), as(a1, 'satisfied')],
    ],
    [
      records(
        ended as ChatMessage,
        stage('t', 'review', 'done'),
        stage('t', 'checks', 'done'),
        stage('t', 'acceptance', 'done'),
        stage('t', 'deliver', 'running')
      ),
      [as(r1, 'findings'), as(c2, 'passed'), as(a1, 'satisfied')],
    ],
  ];
  const rank = (status: string) => (status === 'pending' ? 0 : status === 'running' ? 1 : 2);
  let was: Record<string, string> = {};
  for (const [i, [rows, held]] of states.entries()) {
    const shownRows = withLiveStages(rows, [
      ...rows.map(
        (r) =>
          ({ id: r.id, role: 'assistant', content: '', meta: { stage: r.stage } }) as ChatMessage
      ),
      ...held,
    ]);
    const now = Object.fromEntries(
      stripStages(shownRows).map((s) => [
        s.name,
        shownStatus(s.status, true, s.waiting, s.failures),
      ])
    );
    for (const [name, status] of Object.entries(now))
      assert.ok(
        rank(status) >= rank(was[name] ?? 'pending'),
        `state ${i}: ${name} went from ${was[name]} to ${status}`
      );
    was = now;
  }
  assert.equal(was.deliver, 'running');
});

test('the strip: the plan’s review shows as the plan’s run gets to it', () => {
  const plan = stage('t', 'plan', 'running');
  const rows = withLiveStages(stageRows([plan]), [plan, review('running', 'plan')]);
  assert.equal(taskLine(rows, true), 'Autopilot: plan review…');
  // A change review isn't the plan's.
  assert.equal(
    taskLine(withLiveStages(stageRows([plan]), [plan, review('running')]), true),
    'Autopilot: plan…'
  );
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
  assert.equal(
    scoreLine({ ...score, clean: false }),
    'its run ended with a caveat · 2 tests written first pass · 1 check passes · no findings open · 12 lines changed'
  );
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
