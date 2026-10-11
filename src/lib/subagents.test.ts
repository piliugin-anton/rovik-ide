import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ChatMessage } from '../types';
import { chatApi, type MessageRow } from './chatApi';
import {
  childOf,
  forgetLive,
  forgetTranscript,
  openTranscript,
  receiveSubagentMessages,
  transcript,
  isSubagentChat,
  seedRunningSubagent,
  runUsage,
  usageLabel,
  kindLabel,
  liveSubagent,
  noteSubagentEnded,
  noteSubagentProgress,
  noteSubagentStarted,
  reportBody,
  statusLabel,
  workLabel,
} from './subagents';

test('a subagent is followed from its events, under its lead chat and call', () => {
  noteSubagentStarted({
    dialogId: 'lead',
    toolCallId: 't1',
    childDialogId: 'kid',
    kind: 'scout',
    model: 'Test/m',
    goal: 'Where?',
  });
  assert.ok(isSubagentChat('kid'));
  assert.ok(!isSubagentChat('lead'));
  assert.equal(liveSubagent('lead', 't1')?.toolCalls, 0);

  noteSubagentProgress({
    dialogId: 'lead',
    toolCallId: 't1',
    toolCalls: 3,
    currentTool: 'read',
    model: 'Other/x',
  });
  const live = liveSubagent('lead', 't1')!;
  assert.equal(live.toolCalls, 3);
  assert.equal(live.currentTool, 'read');
  assert.equal(live.model, 'Other/x');

  noteSubagentEnded({ dialogId: 'lead', toolCallId: 't1', childDialogId: 'kid', status: 'done' });
  assert.equal(liveSubagent('lead', 't1')?.status, 'done');
  assert.equal(liveSubagent('lead', 't1')?.currentTool, null);
  // Progress for a call nobody started is ignored.
  noteSubagentProgress({
    dialogId: 'x',
    toolCallId: 'y',
    toolCalls: 1,
    currentTool: null,
    model: '',
  });
  assert.equal(liveSubagent('x', 'y'), undefined);
});

test('the block’s words', () => {
  assert.equal(kindLabel('scout'), 'Scout');
  assert.equal(kindLabel('reviewer'), 'Change reviewer');
  assert.equal(kindLabel('fixer'), 'Fixer');
  assert.equal(statusLabel('partial'), 'out of budget');
  assert.equal(workLabel(1), '1 tool call');
  assert.equal(workLabel(12, 41_000), '12 tool calls · 41 s');
  assert.equal(workLabel(2, 95_000), '2 tool calls · 1 min 35 s');
  assert.equal(
    reportBody('Report from a scout subagent. …\nStatus: done (1 tool call)\n\n## Answer\nHere.'),
    '## Answer\nHere.'
  );
  assert.equal(reportBody('Error: subagents are turned off.'), 'Error: subagents are turned off.');
});

test('tokens on a block, and the latest request’s total', () => {
  assert.equal(usageLabel(null), null);
  assert.equal(usageLabel({ inputTokens: 900, outputTokens: 50 }), '950 tokens');
  assert.equal(
    usageLabel({ inputTokens: 12_000, outputTokens: 300, cost: 0.0123 }),
    '12.3k tokens · $0.01'
  );
  assert.equal(usageLabel({ inputTokens: 1, outputTokens: 1, cost: 0.0004 }), '2 tokens · $0.0004');
  const msg = (role: ChatMessage['role'], meta?: ChatMessage['meta']): ChatMessage => ({
    id: Math.random().toString(),
    role,
    content: role === 'user' ? 'go' : '',
    meta,
  });
  const used = (inputTokens: number, outputTokens: number, cost?: number) => ({
    kind: 'scout',
    status: 'done' as const,
    childDialogId: null,
    model: null,
    modelsTried: [],
    toolCalls: 1,
    ms: 1,
    usage: { inputTokens, outputTokens, ...(cost !== undefined ? { cost } : {}) },
  });
  const messages = [
    msg('user'),
    msg('tool', { subagent: used(1000, 0) }),
    msg('user'),
    msg('tool', { subagent: used(100, 10, 0.5) }),
    msg('user', { harness: 'verification' }),
    msg('tool', { subagent: used(200, 20) }),
    msg('assistant', {
      review: {
        kind: 'change',
        status: 'clean',
        round: 1,
        maxRounds: 3,
        usage: { inputTokens: 5, outputTokens: 5 },
      },
    }),
  ];
  assert.deepEqual(runUsage(messages), { inputTokens: 305, outputTokens: 35, cost: 0.5 });
  assert.equal(runUsage([msg('user')]), null);
  // P5.3: a check round's triage counts.
  const triaged = msg('assistant', {
    verification: {
      status: 'failed',
      round: 1,
      maxRounds: 3,
      checks: [],
      changedFiles: [],
      triage: {
        status: 'done',
        check: 'test',
        reason: 'large output',
        usage: { inputTokens: 40, outputTokens: 2, cost: 0.25 },
      },
    },
  });
  assert.deepEqual(runUsage([...messages, triaged]), {
    inputTokens: 345,
    outputTokens: 37,
    cost: 0.75,
  });
});

test('a reloaded page picks up a running subagent with what it does (P1.5)', () => {
  seedRunningSubagent({
    dialogId: 'kid-r',
    parentDialogId: 'lead-r',
    parentCallId: 't7',
    kind: 'worker',
    goal: 'Step 2',
    model: 'Test/m',
    startedAt: 1,
    toolCalls: 4,
  });
  const live = liveSubagent('lead-r', 't7')!;
  assert.equal(live.childDialogId, 'kid-r');
  assert.equal(live.kind, 'worker');
  assert.equal(live.toolCalls, 4);
  assert.ok(isSubagentChat('kid-r'));
  // Known only as a chat (an older backend): still found by its call.
  seedRunningSubagent({ dialogId: 'kid-s', parentDialogId: 'lead-s', parentCallId: 't8' });
  assert.equal(liveSubagent('lead-s', 't8'), undefined);
  assert.equal(childOf('lead-s', 't8'), 'kid-s');
  assert.equal(childOf('lead-s', 'other'), null);
});

const row = (id: string, content: string): MessageRow => ({
  id,
  dialogId: 'kid-t',
  seq: 0,
  role: 'assistant',
  content,
  bodyExternal: false,
  bodyBytes: 0,
  toolCalls: null,
  toolCallId: null,
  meta: null,
  error: null,
  failed: false,
  finalized: true,
  attachments: [],
  createdAt: 0,
});

test('rows that arrive while a transcript loads are kept, and an error retries', async () => {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  try {
    let land!: () => void;
    api.getMessages = () =>
      new Promise((resolve) => {
        land = () =>
          resolve({
            messages: [{ id: 'm1', role: 'assistant', content: 'first' }],
            hasOlder: false,
            hasNewer: false,
          });
      });
    const opening = openTranscript('kid-t', 'lead-t', 'c1');
    assert.equal(transcript('kid-t')?.loading, true);
    receiveSubagentMessages('kid-t', [row('m2', 'second')]);
    land();
    await opening;
    assert.deepEqual(
      transcript('kid-t')?.messages.map((m) => m.content),
      ['first', 'second'],
      'the row that arrived mid-load is kept'
    );

    api.getMessages = () => Promise.reject(new Error('db busy'));
    await openTranscript('kid-e', 'lead-t', 'c2');
    assert.match(transcript('kid-e')?.error ?? '', /db busy/);
    api.getMessages = () =>
      Promise.resolve({
        messages: [{ id: 'e1', role: 'assistant', content: 'ok' }],
        hasOlder: false,
        hasNewer: false,
      });
    await openTranscript('kid-e', 'lead-t', 'c2');
    assert.equal(transcript('kid-e')?.error, undefined);
    assert.equal(transcript('kid-e')?.messages[0]?.content, 'ok', 'opened again: tried again');
  } finally {
    api.getMessages = original;
  }
});

test('finished subagents and unshown transcripts leave the store', async () => {
  noteSubagentStarted({
    dialogId: 'lead-f',
    toolCallId: 'call_0',
    childDialogId: 'kid-new',
    kind: 'scout',
    model: 'm',
    goal: 'g',
  });
  // An older call with the same id, finished: the newer run's entry stays.
  forgetLive('lead-f', 'call_0', 'kid-old');
  assert.equal(liveSubagent('lead-f', 'call_0')?.childDialogId, 'kid-new');
  forgetLive('lead-f', 'call_0', 'kid-new');
  assert.equal(liveSubagent('lead-f', 'call_0'), undefined);
  assert.equal(childOf('lead-f', 'call_0'), 'kid-new', 'the link stays');

  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () => Promise.resolve({ messages: [], hasOlder: false, hasNewer: false });
  try {
    await openTranscript('kid-new', 'lead-f', 'call_0');
    assert.ok(transcript('kid-new'));
    forgetTranscript('kid-new');
    assert.equal(transcript('kid-new'), undefined);
  } finally {
    api.getMessages = original;
  }
});
