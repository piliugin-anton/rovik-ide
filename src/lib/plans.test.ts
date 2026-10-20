// plans.ts tests (node:test): a remote chat's plan is read from its host
// and opens read-only; a local chat's opens in the editor (K09).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { implementationMessage, openPlan, readPlan } from './plans';
import { chatStore } from './chatStore';
import { setEditorOpener, type Compare, type OpenInEditor } from './openInEditor';

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
  const opened: OpenInEditor[] = [];
  setEditorOpener(async (request) => {
    opened.push(request);
  });
  const remote = chatStore.createDialog('Fix the build');
  chatStore.setPlace(remote.id, { kind: 'host', host: 'h1' });
  const path = '/srv/rovik/work/chat/.rovik/plans/fix-the-build.md';
  await openPlan(remote.id, path);
  assert.deepEqual(reads(), [{ dialogId: remote.id, path }]);
  assert.equal(opened.length, 1);
  assert.ok(typeof opened[0].marks === 'object', 'not as a file of this computer’s');
  const compare = (opened[0].marks as { compare: Compare }).compare;
  assert.equal(compare.after, PLAN);
  assert.equal(compare.before, PLAN, 'nothing marked: the plan as it is');
  assert.match(compare.title, /^fix-the-build\.md \(on h1\)$/);

  // A local chat's opens in the editor, where it can be edited.
  const local = chatStore.createDialog('Local');
  await openPlan(local.id, '/home/me/app/.rovik/plans/local.md');
  assert.deepEqual(opened[1], { path: '/home/me/app/.rovik/plans/local.md', focus: true });
  assert.equal(reads().length, 1, 'read as a file there');
  setEditorOpener(null);
});

test('a plan is read through chat_read_plan, for a chat of either kind', async () => {
  calls.length = 0;
  assert.equal(await readPlan('chat-7', '/p/.rovik/plans/a.md'), PLAN);
  assert.deepEqual(reads(), [{ dialogId: 'chat-7', path: '/p/.rovik/plans/a.md' }]);
});

test('implementing a plan sends its path; the backend writes what the model gets (A04)', () => {
  calls.length = 0;
  const message = implementationMessage('/p/.rovik/plans/a.md', '/p');
  assert.deepEqual(message, {
    content: 'Implement the plan in `.rovik/plans/a.md`.',
    meta: { mode: 'agent', implementsPlan: true, planPath: '/p/.rovik/plans/a.md' },
  });
  assert.equal(reads().length, 0, 'not read here');
  assert.equal(
    implementationMessage('/srv/w/.rovik/plans/a.md', '/p').content,
    'Implement the plan in `/srv/w/.rovik/plans/a.md`.',
    'a path outside the project, as it is'
  );
});
