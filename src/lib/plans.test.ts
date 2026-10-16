// plans.ts tests (node:test): a remote chat's plan is read from its host
// and opens read-only; a local chat's opens in the editor (K09).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { openPlan, readPlan } from './plans';
import { chatStore } from './chatStore';
import {
  onCompareRequest,
  onOpenFileRequest,
  type CompareRequest,
  type OpenFileRequest,
} from './editorBridge';

const PLAN = '# Fix the build\n\n1. Pin the toolchain\n';
const calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      return cmd === 'chat_read_plan' ? PLAN : null;
    },
  },
  setTimeout: () => 0,
  clearTimeout: () => undefined,
};

const reads = () => calls.filter((c) => c.cmd === 'chat_read_plan').map((c) => c.args);

test('a remote plan opens from the owner, read-only', async () => {
  const opened: OpenFileRequest[] = [];
  const compared: CompareRequest[] = [];
  const stopOpen = onOpenFileRequest((request) => opened.push(request));
  const stopCompare = onCompareRequest((request) => compared.push(request));
  const remote = chatStore.createDialog('Fix the build');
  chatStore.setPlace(remote.id, { kind: 'host', host: 'h1' });
  const path = '/srv/rovik/work/chat/.rovik/plans/fix-the-build.md';
  await openPlan(remote.id, path);
  assert.deepEqual(reads(), [{ dialogId: remote.id, path }]);
  assert.deepEqual(opened, [], 'not as a file of this computer’s');
  assert.equal(compared.length, 1);
  assert.equal(compared[0].after, PLAN);
  assert.equal(compared[0].before, PLAN, 'nothing marked: the plan as it is');
  assert.match(compared[0].title, /^fix-the-build\.md \(on h1\)$/);

  // A local chat's opens in the editor, where it can be edited.
  const local = chatStore.createDialog('Local');
  await openPlan(local.id, '/home/me/app/.rovik/plans/local.md');
  assert.deepEqual(opened, [{ path: '/home/me/app/.rovik/plans/local.md' }]);
  assert.equal(reads().length, 1, 'read as a file there');
  stopOpen();
  stopCompare();
});

test('a plan to implement is read through chat_read_plan, for a chat of either kind', async () => {
  calls.length = 0;
  assert.equal(await readPlan('chat-7', '/p/.rovik/plans/a.md'), PLAN);
  assert.deepEqual(reads(), [{ dialogId: 'chat-7', path: '/p/.rovik/plans/a.md' }]);
});
