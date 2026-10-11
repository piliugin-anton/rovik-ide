// Component tests: a subagent's `task` block, its transcript and its line
// in the chat (ToolCallGroup), with what the backend's events and a
// reloaded page's status leave in the stores. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ToolCallBlock, { SubagentTranscript } from './ToolCallBlock';
import ToolCallGroup from './ToolCallGroup';
import { chatApi } from '../lib/chatApi';
import { clearApproval, registerApproval } from '../lib/approvals';
import {
  noteSubagentStarted,
  openTranscript,
  receiveSubagentMessages,
  seedRunningSubagent,
  transcript,
} from '../lib/subagents';
import type { ChatMessage, ChatToolCall } from '../types';

const task = (id: string, args: Record<string, unknown> = { kind: 'scout', goal: 'Look' }) => ({
  id,
  name: 'task',
  arguments: JSON.stringify(args),
});

/** Lets loads that already resolved land, and effects run. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const button = (root: Element, text: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === text);

/** `chatApi.getMessages` answering with `messages` for the test's duration. */
async function withMessages(messages: ChatMessage[], run: () => Promise<void>) {
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () => Promise.resolve({ messages, hasOlder: false, hasNewer: false });
  try {
    await run();
  } finally {
    api.getMessages = original;
  }
}

test('two_blocks_sharing_a_child_transcript (P5.9)', async () => {
  // A follow-up `task` continues the same child under a new call.
  for (const toolCallId of ['t1', 't2'])
    noteSubagentStarted({
      dialogId: 'lead-share',
      toolCallId,
      childDialogId: 'kid-share',
      kind: 'scout',
      model: 'Test/m',
      goal: 'Look',
    });
  const reply: ChatMessage = { id: 'k1', role: 'assistant', content: 'Found it in config.ts.' };
  await withMessages([reply], async () => {
    const a = container();
    const disposeA = render(
      () => <ToolCallBlock toolCall={task('t1')} dialogId="lead-share" running initiallyExpanded />,
      a
    );
    const b = container();
    const disposeB = render(
      () => <ToolCallBlock toolCall={task('t2')} dialogId="lead-share" running initiallyExpanded />,
      b
    );
    button(a, 'Show its work')!.click();
    button(b, 'Show its work')!.click();
    await flush();
    assert.equal(b.querySelector('.subagent-reply')?.textContent, 'Found it in config.ts.');

    // Collapsing A's row unmounts it: B still shows the transcript, and follows it.
    disposeA();
    assert.ok(transcript('kid-share'), 'kept while B shows it');
    assert.equal(b.querySelector('.subagent-reply')?.textContent, 'Found it in config.ts.');
    receiveSubagentMessages('kid-share', [
      {
        id: 'k2',
        dialogId: 'kid-share',
        seq: 2,
        role: 'assistant',
        content: 'And in main.ts.',
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
      },
    ]);
    assert.deepEqual(
      [...b.querySelectorAll('.subagent-reply')].map((e) => e.textContent),
      ['Found it in config.ts.', 'And in main.ts.']
    );

    // The last block showing it goes: so does the transcript.
    disposeB();
    assert.equal(transcript('kid-share'), undefined);
  });
});

test('subagent_transcript_with_reused_ids (P5.8)', async () => {
  // Gemini numbers calls per reply: every reply's first call is function-call-1.
  const call = (path: string): ChatToolCall => ({
    id: 'function-call-1',
    name: 'read',
    arguments: JSON.stringify({ path }),
  });
  const messages: ChatMessage[] = [
    { id: 'a1', role: 'assistant', content: '', toolCalls: [call('a.ts')] },
    { id: 'r1', role: 'tool', tool_call_id: 'function-call-1', content: 'export const a = 1;' },
    { id: 'a2', role: 'assistant', content: '', toolCalls: [call('b.ts')] },
    { id: 'r2', role: 'tool', tool_call_id: 'function-call-1', content: 'Error: no such file' },
    { id: 'a3', role: 'assistant', content: '', toolCalls: [call('c.ts')] },
  ];
  await withMessages(messages, () => openTranscript('kid-ids', 'lead-ids', 'c1'));
  const root = container();
  render(() => <SubagentTranscript childDialogId="kid-ids" running={true} />, root);
  const blocks = [...root.querySelectorAll('.tool-call')];
  assert.equal(blocks.length, 3);
  assert.ok(!blocks[0].classList.contains('error'), 'the first read has the first result');
  assert.ok(blocks[1].classList.contains('error'), 'the second read has its own (failed) result');
  assert.ok(blocks[2].classList.contains('running'), 'the third has none yet');
});

test('approval_shown_after_reload_with_nothing_else_known (P1.5)', () => {
  // After a reload: only the approval, naming its lead's call.
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-reload',
    toolName: 'bash',
    summary: 'Run `npm test`',
    command: 'npm test',
    parentDialogId: 'lead-reload',
    parentCallId: 't1',
  });
  const root = container();
  render(() => <ToolCallBlock toolCall={task('t1')} dialogId="lead-reload" running />, root);
  const block = root.querySelector('.subagent-block')!;
  assert.ok(block.classList.contains('awaiting'), block.className);
  assert.equal(root.querySelectorAll('.tool-approval').length, 1);
  assert.equal(root.querySelector('.tool-call-meta')?.textContent, 'starting…');

  seedRunningSubagent({
    dialogId: 'kid-reload',
    parentDialogId: 'lead-reload',
    parentCallId: 't1',
    kind: 'scout',
    toolCalls: 2,
    model: 'm',
  });
  assert.equal(root.querySelector('.tool-call-meta')?.textContent, '2 tool calls · m');
  assert.equal(root.querySelectorAll('.tool-approval').length, 1, 'still once');
  clearApproval('kid-reload', 'b1');
});

function taskLine(id: string, dialogId: string, result?: ChatMessage) {
  const root = container();
  render(
    () => (
      <ToolCallGroup
        name="task"
        calls={[task(id)]}
        dialogId={dialogId}
        result={() => result}
        running={true}
        renderCall={(call, expanded) => (
          <ToolCallBlock
            toolCall={call}
            dialogId={dialogId}
            result={result}
            running
            initiallyExpanded={expanded}
          />
        )}
      />
    ),
    root
  );
  return root;
}

test('collapsed_row_opens_for_a_child_approval (P1.6)', () => {
  noteSubagentStarted({
    dialogId: 'lead-row',
    toolCallId: 't1',
    childDialogId: 'kid-row',
    kind: 'scout',
    model: 'm',
    goal: 'Look',
  });
  const root = taskLine('t1', 'lead-row');
  assert.equal(root.querySelector('.tool-group-calls'), null, 'collapsed');
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-row',
    toolName: 'bash',
    summary: 'Run `ls`',
    parentDialogId: 'lead-row',
    parentCallId: 't1',
  });
  assert.ok(root.querySelector('.tool-group-calls'), 'opened for the child’s approval');
  assert.equal(root.querySelectorAll('.tool-approval').length, 1);
  clearApproval('kid-row', 'b1');
  assert.equal(root.querySelector('.tool-group-calls'), null, 'closed again once answered');
});

test('awaiting_ignores_a_task_with_a_result', () => {
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-res',
    toolName: 'bash',
    summary: 'Run `ls`',
    parentDialogId: 'lead-res',
    parentCallId: 't1',
  });
  const result: ChatMessage = {
    id: 'r1',
    role: 'tool',
    tool_call_id: 't1',
    content: 'Report from a scout\n\n## Answer\nHere.',
    meta: {
      subagent: {
        kind: 'scout',
        status: 'done',
        childDialogId: 'kid-res',
        model: 'm',
        modelsTried: ['m'],
        toolCalls: 1,
        ms: 10,
      },
    },
  };
  const root = taskLine('t1', 'lead-res', result);
  assert.equal(root.querySelector('.tool-group-calls'), null, 'its result is in: not held open');
  clearApproval('kid-res', 'b1');
});
