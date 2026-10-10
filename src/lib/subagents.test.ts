import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ChatMessage } from '../types';
import {
  isSubagentChat,
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
  assert.equal(kindLabel('triage'), 'Triage');
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
});
