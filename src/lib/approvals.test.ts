import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  allow,
  clearApproval,
  decideRemaining,
  onRevealEdit,
  pendingApproval,
  pendingApprovalsOfTask,
  pendingEditFor,
  registerApproval,
  revealEdit,
  setHunkState,
  showRemoteEdit,
  type ApprovalRequest,
} from './approvals';
import { onCompareRequest, type CompareRequest } from './editorBridge';
import { childOf, noteSubagentStarted } from './subagents';

/*
 * A file edit reviewed hunk by hunk: what the run is told decides what's
 * written. The backend calls are caught instead of sent.
 */
const sent: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: unknown) => {
      sent.push({ cmd, args });
    },
  },
};

let n = 0;
function review(current: string, proposed: string, dialogId = 'chat'): string {
  const toolCallId = `call-${++n}`;
  const request: ApprovalRequest = {
    toolCallId,
    dialogId,
    toolName: 'write',
    summary: 'Edit a.txt',
    edit: { absPath: `/p/a${n}.txt`, relPath: `a${n}.txt`, current, proposed },
  };
  registerApproval(request);
  return toolCallId;
}

const decision = (toolCallId: string) =>
  sent.find((s) => s.cmd === 'agent_resolve_approval' && s.args.toolCallId === toolCallId)?.args;

test('accepting some changes writes the original with only those applied', () => {
  const id = review('one\ntwo\nthree\nfour\n', 'ONE\ntwo\nthree\nFOUR\n');
  assert.equal(pendingApproval('chat', id)?.edit?.hunks.length, 2);
  setHunkState('chat', id, 0, 'accepted');
  assert.equal(decision(id), undefined, 'not decided while a change is pending');
  setHunkState('chat', id, 1, 'rejected');
  const d = decision(id);
  assert.equal(d.dialogId, 'chat');
  assert.equal(d.decision.kind, 'partial');
  assert.equal(d.decision.content, 'ONE\ntwo\nthree\nfour\n');
  assert.equal(d.decision.total, 2);
  assert.deepEqual(d.decision.rejected, [{ origStart: 4, deleted: ['four'], added: ['FOUR'] }]);
  assert.equal(pendingApproval('chat', id), undefined, 'settled');
});

test('a CRLF file stays CRLF and a missing final newline stays missing', () => {
  const id = review('a\r\nb\r\nc', 'A\r\nb\r\nC');
  setHunkState('chat', id, 0, 'rejected');
  setHunkState('chat', id, 1, 'accepted');
  assert.equal(decision(id).decision.content, 'a\r\nb\r\nC');
});

test('all accepted allows, all rejected denies', () => {
  const yes = review('a\nb\nc\n', 'A\nb\nC\n');
  decideRemaining('chat', yes, 'accepted');
  assert.deepEqual(decision(yes).decision, { kind: 'allow' });

  const no = review('a\nb\nc\n', 'A\nb\nC\n');
  setHunkState('chat', no, 0, 'rejected');
  decideRemaining('chat', no, 'rejected');
  assert.deepEqual(decision(no).decision, { kind: 'deny' });
});

test('"accept the rest" keeps changes already rejected', () => {
  const id = review('1\n2\n3\n4\n5\n', 'X\n2\nY\n4\nZ\n');
  assert.equal(pendingApproval('chat', id)?.edit?.hunks.length, 3);
  setHunkState('chat', id, 1, 'rejected');
  decideRemaining('chat', id, 'accepted');
  assert.equal(decision(id).decision.content, 'X\n2\n3\n4\nZ\n');
});

test('an undecided change can be decided again', () => {
  const id = review('a\nb\nc\n', 'A\nb\nC\n');
  setHunkState('chat', id, 0, 'accepted');
  setHunkState('chat', id, 0, 'pending');
  setHunkState('chat', id, 1, 'rejected');
  assert.equal(decision(id), undefined, 'the first change is pending again');
  setHunkState('chat', id, 0, 'rejected');
  assert.deepEqual(decision(id).decision, { kind: 'deny' });
});

test('two chats waiting on the same call id are reviewed apart', () => {
  const request = (dialogId: string, proposed: string): ApprovalRequest => ({
    toolCallId: 'function-call-1',
    dialogId,
    toolName: 'write',
    summary: 'Edit',
    edit: { absPath: `/p/${dialogId}.txt`, relPath: `${dialogId}.txt`, current: 'a\n', proposed },
  });
  registerApproval(request('chat-a', 'A\n'));
  registerApproval(request('chat-b', 'B\n'));
  assert.deepEqual(pendingApproval('chat-a', 'function-call-1')?.edit?.hunks[0].added, ['A']);
  assert.deepEqual(pendingApproval('chat-b', 'function-call-1')?.edit?.hunks[0].added, ['B']);
  setHunkState('chat-a', 'function-call-1', 0, 'accepted');
  assert.ok(pendingApproval('chat-b', 'function-call-1'), 'the other chat still waits');
  const forA = sent.filter((s) => s.args.toolCallId === 'function-call-1');
  assert.equal(forA.length, 1);
  assert.equal(forA[0].args.dialogId, 'chat-a');
});

test("a subagent's approval is found by its lead's call, with nothing else known (P1.5)", () => {
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-x',
    toolName: 'bash',
    summary: 'Run `npm test`',
    parentDialogId: 'lead-x',
    parentCallId: 't1',
  });
  assert.equal(pendingApprovalsOfTask('lead-x', 't1', null).length, 1);
  assert.equal(pendingApprovalsOfTask('lead-x', 't1', 'kid-x').length, 1);
  assert.equal(pendingApprovalsOfTask('lead-x', 't2', null).length, 0);
});

test('approval_before_subagent_started: one record, counted once before and after', () => {
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-early',
    toolName: 'bash',
    summary: 'Run `ls`',
    parentDialogId: 'lead-early',
    parentCallId: 't1',
  });
  // Before subagentStarted: found by the call the request names.
  assert.equal(pendingApprovalsOfTask('lead-early', 't1', childOf('lead-early', 't1')).length, 1);
  noteSubagentStarted({
    dialogId: 'lead-early',
    toolCallId: 't1',
    childDialogId: 'kid-early',
    kind: 'scout',
    model: 'm',
    goal: 'g',
  });
  // After: found both by its chat and by the call, still one.
  assert.equal(childOf('lead-early', 't1'), 'kid-early');
  assert.equal(pendingApprovalsOfTask('lead-early', 't1', childOf('lead-early', 't1')).length, 1);
  clearApproval('kid-early', 'b1');
});

test('no_cross_chat_leak: two leads with the same call id see their own child’s approvals', () => {
  // Providers number calls per reply: both leads have a "t1".
  for (const [lead, kid] of [
    ['lead-a', 'kid-a'],
    ['lead-b', 'kid-b'],
  ])
    noteSubagentStarted({
      dialogId: lead,
      toolCallId: 't1',
      childDialogId: kid,
      kind: 'scout',
      model: 'm',
      goal: 'g',
    });
  registerApproval({
    toolCallId: 'b1',
    dialogId: 'kid-a',
    toolName: 'bash',
    summary: 'Run `ls`',
    parentDialogId: 'lead-a',
    parentCallId: 't1',
  });
  assert.equal(pendingApprovalsOfTask('lead-a', 't1', childOf('lead-a', 't1')).length, 1);
  assert.equal(pendingApprovalsOfTask('lead-b', 't1', childOf('lead-b', 't1')).length, 0);
  // Callers pass `childOf` in the same chat: another chat's child would match by its chat.
  assert.equal(pendingApprovalsOfTask('lead-b', 't1', 'kid-a').length, 1);
  clearApproval('kid-a', 'b1');
});

test('a remote edit approval doesn’t open the local file: it shows apart, allowed whole (K09)', () => {
  const revealed: string[] = [];
  const compared: CompareRequest[] = [];
  const stopReveal = onRevealEdit((path) => revealed.push(path));
  const stopCompare = onCompareRequest((request) => compared.push(request));
  const absPath = '/srv/rovik/work/chat/src/a.ts';
  registerApproval({
    toolCallId: 'remote-1',
    dialogId: 'remote-chat',
    toolName: 'write',
    summary: 'Edit src/a.ts',
    edit: { absPath, relPath: 'src/a.ts', current: 'one\n', proposed: 'two\n' },
    remote: true,
  });
  assert.deepEqual(revealed, [], 'the local file isn’t opened');
  assert.equal(pendingEditFor(absPath), undefined, 'nor reviewed on its tab');
  assert.equal(pendingApproval('remote-chat', 'remote-1')?.edit, undefined);
  assert.equal(compared.length, 1);
  const shown = compared[0];
  assert.deepEqual(
    { fileName: shown.fileName, before: shown.before, after: shown.after },
    { fileName: 'src/a.ts', before: 'one\n', after: 'two\n' }
  );
  // "Show the change" shows it again.
  showRemoteEdit('remote-chat', 'remote-1');
  assert.equal(compared.length, 2);
  assert.equal(compared[1].id, shown.id, 'in the same tab');
  stopReveal();
  stopCompare();
  allow('remote-chat', 'remote-1');
  assert.deepEqual(decision('remote-1').decision, { kind: 'allow' });
  assert.equal(pendingApproval('remote-chat', 'remote-1'), undefined);
});

test('"Review in editor" asks to open the file; the reveal as the edit arrives doesn’t insist', () => {
  // App opens the file for an explicit reveal whatever the editor settings
  // say; one as the approval arrives only when files open after agent edits.
  const revealed: [string, boolean][] = [];
  const stopReveal = onRevealEdit((path, explicit) => revealed.push([path, explicit]));
  const id = review('one\n', 'two\n', 'reveal-chat');
  const absPath = pendingApproval('reveal-chat', id)?.edit?.absPath;
  assert.ok(absPath);
  assert.deepEqual(revealed, [[absPath, false]], 'arriving: not explicit');
  revealEdit('reveal-chat', id, true);
  assert.deepEqual(revealed[1], [absPath, true], 'the button: explicit');
  stopReveal();
  clearApproval('reveal-chat', id);
});
