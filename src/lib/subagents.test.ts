import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  isSubagentChat,
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
  assert.equal(kindLabel('reviewer'), 'Reviewer');
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
