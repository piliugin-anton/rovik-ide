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
