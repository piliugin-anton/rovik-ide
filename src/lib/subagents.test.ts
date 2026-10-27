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
  transcriptHold,
  isSubagentChat,
  seedRunningSubagent,
  runUsage,
  runUsageSource,
  usageLabel,
  kindLabel,
  liveLabel,
  modelLabel,
  liveSubagent,
  noteSubagentEnded,
  noteSubagentProgress,
  noteSubagentStarted,
  patchSubagentMessage,
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
  const live = { toolCalls: 3, currentTool: 'bash', model: 'm' };
  assert.equal(liveLabel(live), '3 tool calls · bash · m');
  assert.equal(liveLabel({ ...live, currentTool: null }), '3 tool calls · m');
  assert.equal(liveLabel(live, false), '3 tool calls · bash');
  assert.equal(modelLabel('m', ['a', 'b', 'm']), 'm (after 2 failed)');
  assert.equal(modelLabel('m', ['m']), 'm');
  assert.equal(modelLabel('m', undefined), 'm');
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
  // A message sent to the run while it worked doesn't start a request: what
  // came before it is still the request's.
  assert.deepEqual(runUsage([...messages, msg('user', { steer: { queuedAt: 1 } })]), {
    inputTokens: 305,
    outputTokens: 35,
    cost: 0.5,
  });
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
  // A change review's guide counts beside its reviewer.
  const guided = msg('assistant', {
    review: {
      kind: 'change',
      status: 'findings',
      round: 2,
      maxRounds: 3,
      usage: { inputTokens: 10, outputTokens: 1 },
      guide: { status: 'done', usage: { inputTokens: 30, outputTokens: 3, cost: 0.125 } },
    },
  });
  assert.deepEqual(runUsage([...messages, guided]), {
    inputTokens: 345,
    outputTokens: 39,
    cost: 0.625,
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

/** `chatApi.getMessages` answering with `messages` once `land()` is called. */
function deferredPage(messages: ChatMessage[]) {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  let land!: () => void;
  api.getMessages = () =>
    new Promise((resolve) => {
      land = () => resolve({ messages, hasOlder: false, hasNewer: false });
    });
  return { land: () => land(), restore: () => (api.getMessages = original) };
}

test('a_patch_for_a_page_row_during_load_is_kept (P5.4)', async () => {
  const page = deferredPage([
    { id: 'm1', role: 'assistant', content: 'partial', pending: true },
    { id: 'm0', role: 'assistant', content: 'other' },
  ]);
  try {
    const opening = openTranscript('kid-p', 'lead-p', 'c1');
    // The child's streaming reply finishes while its page is fetched.
    patchSubagentMessage('kid-p', 'm1', { content: 'full', finalized: true });
    page.land();
    await opening;
    const m1 = transcript('kid-p')!.messages.find((m) => m.id === 'm1')!;
    assert.equal(m1.content, 'full');
    assert.equal(m1.pending, false);
    assert.equal(transcript('kid-p')!.messages.length, 2);
  } finally {
    page.restore();
  }
});

test('buffered_row_then_patched: a row that arrived mid-load is patched where it waits', async () => {
  const page = deferredPage([{ id: 'b1', role: 'assistant', content: 'first' }]);
  try {
    const opening = openTranscript('kid-b', 'lead-b', 'c1');
    receiveSubagentMessages('kid-b', [{ ...row('b2', 'stre'), finalized: false, pending: true }]);
    patchSubagentMessage('kid-b', 'b2', { content: 'streamed', finalized: true });
    page.land();
    await opening;
    assert.deepEqual(
      transcript('kid-b')!.messages.map((m) => [m.id, m.content, !!m.pending]),
      [
        ['b1', 'first', false],
        ['b2', 'streamed', false],
      ]
    );
  } finally {
    page.restore();
  }
});

test('finished_child_after_reload: opened with no live state, then followed', async () => {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () =>
    Promise.resolve({
      messages: ['r1', 'r2', 'r3'].map((id) => ({ id, role: 'assistant' as const, content: id })),
      hasOlder: false,
      hasNewer: false,
    });
  try {
    assert.equal(isSubagentChat('kid-reloaded'), false);
    await openTranscript('kid-reloaded', 'lead-reloaded', 'c9');
    assert.equal(transcript('kid-reloaded')!.messages.length, 3);
    assert.ok(isSubagentChat('kid-reloaded'), 'its events now go to the transcript');
    assert.equal(childOf('lead-reloaded', 'c9'), 'kid-reloaded');
    receiveSubagentMessages('kid-reloaded', [row('r4', 'later'), row('r3', 'again')]);
    assert.deepEqual(
      transcript('kid-reloaded')!.messages.map((m) => m.id),
      ['r1', 'r2', 'r3', 'r4'],
      'later rows append, a known one isn’t added twice'
    );
  } finally {
    api.getMessages = original;
  }
});

test('error_then_retry_does_not_double_buffer', async () => {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  try {
    let fail!: () => void;
    api.getMessages = () => new Promise((_, reject) => (fail = () => reject(new Error('busy'))));
    const first = openTranscript('kid-r', 'lead-r2', 'c1');
    receiveSubagentMessages('kid-r', [row('x2', 'second')]);
    fail();
    await first;
    assert.match(transcript('kid-r')!.error ?? '', /busy/);

    const page = deferredPage([
      { id: 'x1', role: 'assistant', content: 'first' },
      { id: 'x2', role: 'assistant', content: 'second' },
    ]);
    const retry = openTranscript('kid-r', 'lead-r2', 'c1');
    receiveSubagentMessages('kid-r', [row('x3', 'third')]);
    page.land();
    await retry;
    assert.deepEqual(
      transcript('kid-r')!.messages.map((m) => m.id),
      ['x1', 'x2', 'x3'],
      'the failed load’s buffer is gone; the retry’s is merged once'
    );
  } finally {
    api.getMessages = original;
  }
});

test('forget_transcript_clears_the_loading_buffer', async () => {
  const page = deferredPage([{ id: 'f1', role: 'assistant', content: 'first' }]);
  try {
    const opening = openTranscript('kid-g', 'lead-g', 'c1');
    receiveSubagentMessages('kid-g', [row('f2', 'mid-load')]);
    forgetTranscript('kid-g');
    page.land();
    await opening;
    assert.equal(
      transcript('kid-g'),
      undefined,
      'a load that lands after it’s forgotten is dropped'
    );
  } finally {
    page.restore();
  }
  const again = deferredPage([{ id: 'f1', role: 'assistant', content: 'first' }]);
  try {
    const opening = openTranscript('kid-g', 'lead-g', 'c1');
    again.land();
    await opening;
    assert.deepEqual(
      transcript('kid-g')!.messages.map((m) => m.id),
      ['f1'],
      'nothing buffered before it was forgotten comes back'
    );
  } finally {
    again.restore();
  }
});

test('subagent_ended_before_status: an end for an unknown entry is a no-op', () => {
  noteSubagentEnded({
    dialogId: 'lead-u',
    toolCallId: 't1',
    childDialogId: 'kid-u',
    status: 'done',
  });
  assert.equal(liveSubagent('lead-u', 't1'), undefined);
  seedRunningSubagent({
    dialogId: 'kid-u',
    parentDialogId: 'lead-u',
    parentCallId: 't1',
    kind: 'scout',
    model: 'm',
  });
  assert.equal(liveSubagent('lead-u', 't1')?.status, undefined, 'seeded as running');
  forgetLive('lead-u', 't1', 'kid-u');
  assert.equal(liveSubagent('lead-u', 't1'), undefined);
  assert.equal(childOf('lead-u', 't1'), 'kid-u', 'the link stays');
});

test('run_usage_sums_subagent_review_and_triage', () => {
  const usage = (inputTokens: number, outputTokens: number, cost?: number) => ({
    inputTokens,
    outputTokens,
    ...(cost !== undefined ? { cost } : {}),
  });
  const subagent = (u: ReturnType<typeof usage>) => ({
    kind: 'scout',
    status: 'done' as const,
    childDialogId: null,
    model: null,
    modelsTried: [],
    toolCalls: 1,
    ms: 1,
    usage: u,
  });
  const messages: ChatMessage[] = [
    // Before the request: not counted.
    { id: 'old', role: 'tool', content: '', meta: { subagent: subagent(usage(1000, 1000, 9)) } },
    { id: 'u', role: 'user', content: 'go' },
    { id: 's', role: 'tool', content: '', meta: { subagent: subagent(usage(100, 10, 0.1)) } },
    {
      id: 'r',
      role: 'assistant',
      content: '',
      meta: {
        review: {
          kind: 'change',
          status: 'clean',
          round: 1,
          maxRounds: 3,
          usage: usage(50, 5, 0.05),
        },
      },
    },
    {
      id: 'v',
      role: 'assistant',
      content: '',
      meta: {
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
            usage: usage(40, 2, 0.1),
          },
        },
      },
    },
  ];
  const total = runUsage(messages)!;
  assert.equal(total.inputTokens, 190);
  assert.equal(total.outputTokens, 17);
  assert.ok(Math.abs(total.cost! - 0.25) < 1e-9, String(total.cost));
  // None reported a cost: the total has none.
  const free = runUsage([
    { id: 'u', role: 'user', content: 'go' },
    { id: 's', role: 'tool', content: '', meta: { subagent: subagent(usage(1, 2)) } },
  ]);
  assert.deepEqual(free, { inputTokens: 1, outputTokens: 2 });
  assert.equal(free!.cost, undefined);
});

test('a transcript is kept while any block holds it (P5.9)', async () => {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () =>
    Promise.resolve({
      messages: [{ id: 'h1', role: 'assistant', content: 'hi' }],
      hasOlder: false,
      hasNewer: false,
    });
  try {
    const a = transcriptHold();
    const b = transcriptHold();
    await a.open('kid-h', 'lead-h', 't1');
    await a.open('kid-h', 'lead-h', 't1'); // shown again by the same block: one hold
    await b.open('kid-h', 'lead-h', 't2');
    a.release();
    a.release();
    assert.equal(transcript('kid-h')?.messages.length, 1, 'b still holds it');
    b.release();
    assert.equal(transcript('kid-h'), undefined);
  } finally {
    api.getMessages = original;
  }
});

test('the usage row names who it counted: subagents, a connector, or both', () => {
  const msg = (role: ChatMessage['role'], meta?: ChatMessage['meta']): ChatMessage => ({
    id: Math.random().toString(),
    role,
    content: role === 'user' ? 'go' : '',
    meta,
  });
  const connector = msg('assistant', {
    connector: { kind: 'claude-code', usage: { prompt_tokens: 100, completion_tokens: 5 } },
  });
  const subagent = msg('tool', {
    subagent: {
      kind: 'scout',
      status: 'done',
      childDialogId: null,
      model: null,
      modelsTried: [],
      toolCalls: 1,
      ms: 1,
      usage: { inputTokens: 10, outputTokens: 1 },
    },
  });
  assert.equal(runUsageSource([msg('user'), connector]), 'Claude Code');
  assert.equal(runUsageSource([msg('user'), subagent]), 'Subagents');
  assert.equal(runUsageSource([msg('user'), subagent, connector]), 'Subagents and Claude Code');
  // An earlier request's connector isn't this one's.
  assert.equal(runUsageSource([msg('user'), connector, msg('user'), subagent]), 'Subagents');
});
