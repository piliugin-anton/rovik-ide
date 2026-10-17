// Autopilot's stage records as the chat holds them (lib/autopilot.ts).
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  currentTask,
  pipeline,
  stageRows,
  STAGES,
  taskOver,
  unfinishedTask,
  type StageMeta,
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
