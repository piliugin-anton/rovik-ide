// plans.ts tests (node:test): a remote chat's plan is read from its host
// and opens read-only; a local chat's opens in the editor (K09). A
// workspace chat's plan names its folders, which choose the chat that
// carries it out (D28).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chatForPlan, implementationMessage, openPlan, planFolders, readPlan } from './plans';
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
  const remote = chatStore.createDialog('Fix the build', null);
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
  const local = chatStore.createDialog('Local', null);
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
  const message = implementationMessage('/p/.rovik/plans/a.md', '.rovik/plans/a.md');
  assert.deepEqual(message, {
    content: 'Implement the plan in `.rovik/plans/a.md`.',
    meta: { mode: 'agent', implementsPlan: true, planPath: '/p/.rovik/plans/a.md' },
  });
  assert.equal(reads().length, 0, 'not read here');
});

test('a plan’s Folders line, right under its title, names the folders its workspace chat had', () => {
  assert.deepEqual(planFolders('# Plan: cart\n\nFolders: api, web\n\nSummary.\n'), ['api', 'web']);
  assert.deepEqual(planFolders('# Plan: cart\r\n\r\nFolders: api\r\n'), ['api']);
  // A folder chat's plan, and lines that aren't the plan's own.
  for (const text of [
    '# Plan: cart\n\nSummary.\n',
    '# Plan: cart\n\nSummary.\n\nFolders: api\n',
    'Folders: api\n',
    '# Plan: cart\n\nFolders: \n',
  ]) {
    assert.equal(planFolders(text), null, text);
  }
});

test('a plan with a Folders line goes to a workspace chat with each folder: the open one if it has them, else a new one, even for one folder', () => {
  const workspace = { id: 'w', names: ['api', 'web', 'docs'] };
  const both = ['api', 'web'];
  // The open chat works in each folder named.
  assert.deepEqual(chatForPlan(both, workspace, { projectId: 'w', names: 'all' }), {
    kind: 'open',
  });
  assert.deepEqual(chatForPlan(both, workspace, { projectId: 'w', names: ['web', 'api'] }), {
    kind: 'open',
  });
  // It lacks one, it's another workspace's, or there's none: a new one across them.
  for (const open of [
    { projectId: 'w', names: ['api'] },
    { projectId: 'other', names: 'all' as const },
    null,
  ]) {
    assert.deepEqual(chatForPlan(both, workspace, open), { kind: 'new', folders: both });
  }
  // `Folders: api` gives a workspace chat in api, not api's folder chat:
  // that one would read `api/src/x.ts` as `api/api/src/x.ts`.
  assert.deepEqual(chatForPlan(['api'], workspace, { projectId: 'api-project', names: ['api'] }), {
    kind: 'new',
    folders: ['api'],
  });
});

test('a plan naming a folder this workspace lacks starts nothing, and says which', () => {
  const workspace = { id: 'w', names: ['api', 'web'] };
  const lacking = chatForPlan(['api', 'mobile'], workspace, { projectId: 'w', names: 'all' });
  assert.equal(lacking.kind, 'none');
  assert.match(lacking.kind === 'none' ? lacking.why : '', /^The plan works in mobile, which/);
  // A folder's window has no workspace to carry it out.
  const noWorkspace = chatForPlan(['api'], null, { projectId: 'api-project', names: ['api'] });
  assert.equal(noWorkspace.kind, 'none');
  assert.match(noWorkspace.kind === 'none' ? noWorkspace.why : '', /open the workspace/);
});
