import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  decideRemaining,
  pendingApproval,
  registerApproval,
  setHunkState,
  type ApprovalRequest,
} from './approvals';

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
