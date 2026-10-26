// switchTarget.ts tests (node:test): opening a target in this window — a
// folder, or a workspace's folders — in the order docs/design/workspaces-
// plan.md (3.3) gives, and what a switch that doesn't happen leaves as it
// was.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { OpenFolder } from './openFolders';

const { claimTargetOf, openAtStart, presentFolders, resolvable, switchTarget } =
  await import('./switchTarget');
const { openFolders } = await import('./openFolders');

/** The calls a switch makes, each recorded: the order is what's tested. */
function calls(over: Record<string, unknown> = {}) {
  const order: string[] = [];
  const made = {
    check: async () => (order.push('check'), true),
    settle: async () => (order.push('settle'), true),
    keepTabs: () => void order.push('keepTabs'),
    claim: async () => (order.push('claim'), true),
    show: async () => void order.push('show'),
    resolve: (folders: { path: string }[]) =>
      void order.push(`resolve:${folders.map((f) => f.path).join(',')}`),
    exists: async () => true,
    ...over,
  };
  return { order, made };
}

const folder = (path: string) => ({ kind: 'folder' as const, path });
const workspace = (id: string, folders: string[]) => ({
  kind: 'workspace' as const,
  id,
  name: 'Shop',
  folders: folders.map((path) => ({ name: path.split('/').pop() ?? path, path })),
});

test('a folder opens in these steps, in this order', async () => {
  const { order, made } = calls();
  assert.equal(await switchTarget(folder('/work/app'), made), true);
  assert.deepEqual(order, ['check', 'settle', 'keepTabs', 'claim', 'show', 'resolve:/work/app']);
});

test('a target another window has changes nothing here', async () => {
  const { order, made } = calls({ check: async () => (order.push('check'), false) });
  assert.equal(await switchTarget(folder('/work/app'), made), false);
  assert.deepEqual(order, ['check'], 'tabs, claims and the window are as they were');
});

test('a target another window took since the check changes nothing either', async () => {
  const { order, made } = calls({ claim: async () => (order.push('claim'), false) });
  assert.equal(await switchTarget(folder('/work/app'), made), false);
  assert.deepEqual(order, ['check', 'settle', 'keepTabs', 'claim'], 'shown: no');
});

test('a cancelled unsaved tab stops the switch before anything is claimed', async () => {
  const { order, made } = calls({ settle: async () => (order.push('settle'), false) });
  assert.equal(await switchTarget(folder('/work/app'), made), false);
  assert.deepEqual(order, ['check', 'settle']);
});

test('the tabs shown are kept before the target is claimed', async () => {
  const { order, made } = calls();
  await switchTarget(folder('/work/app'), made);
  assert.ok(order.indexOf('keepTabs') < order.indexOf('claim'));
});

test('a workspace resolves its folders; a missing one is left out until it is back', async () => {
  // The second folder's drive isn't mounted.
  let missing = true;
  const order: string[] = [];
  const made = {
    check: async () => true,
    settle: async () => true,
    keepTabs: () => undefined,
    claim: async () => true,
    show: async () => undefined,
    exists: async (path: string) => !(missing && path === '/work/web'),
    // What the window does with them: the folders that are there are
    // resolved (`resolvable`), and the one that isn't is shown as missing
    // (D20).
    resolve: (folders: OpenFolder[]) => {
      const there = resolvable(folders).map((f) => f.path);
      order.push(`resolve:${there.join(',')}`);
    },
  };
  const target = workspace('w1', ['/work/api', '/work/web']);
  assert.equal(await switchTarget(target, made), true);
  assert.deepEqual(order, ['resolve:/work/api'], 'the missing one is left out');

  // Mounted again, as the window comes to the front: both are resolved.
  missing = false;
  order.length = 0;
  await switchTarget(target, made);
  assert.deepEqual(order, ['resolve:/work/api,/work/web']);
});

test('no folder open resolves nothing: the chats without a project are claimed', async () => {
  const { order, made } = calls();
  assert.equal(await switchTarget(null, made), true);
  assert.deepEqual(order, ['check', 'settle', 'keepTabs', 'claim', 'show', 'resolve:']);
});

test('a window that opens with nothing resolves — the chats without a project', async () => {
  // A first start, a fresh "New window", or a last project another window
  // has: nothing opens, and the window still has its chats, the ones without
  // a project, which resolving none claims (D6).
  const order: string[] = [];
  await openAtStart(null, {
    show: (target) => void order.push(`show:${target ? 'target' : 'nothing'}`),
    resolve: (open) => void order.push(`resolve:${open.map((f) => f.path).join(',')}`),
    exists: async () => true,
  });
  assert.deepEqual(order, ['show:nothing', 'resolve:'], 'nothing open is still resolved');

  // A folder asked for at the start: shown, then its one folder resolved.
  order.length = 0;
  await openAtStart(folder('/work/app'), {
    show: (target) => void order.push(`show:${target ? 'target' : 'nothing'}`),
    resolve: (open) => void order.push(`resolve:${open.map((f) => f.path).join(',')}`),
    exists: async () => true,
  });
  assert.deepEqual(order, ['show:target', 'resolve:/work/app']);
});

test('a folder that is not there is not one to resolve', async () => {
  const open = await presentFolders(
    workspace('w1', ['/work/api', '/work/web']),
    // The second one's drive isn't mounted.
    async (path) => path !== '/work/web'
  );
  assert.deepEqual(
    resolvable(open).map((f) => f.path),
    ['/work/api'],
    'resolving a missing folder would be a project for a path that is not there'
  );
  assert.deepEqual(resolvable([]), []);
});

test('the folders are marked when they are not there', async () => {
  const open = await presentFolders(workspace('w1', ['/work/api', '/work/web']), async (path) => {
    if (path === '/work/web') throw new Error('a drive that is not mounted');
    return true;
  });
  assert.deepEqual(
    open.map((f) => [f.name, f.missing]),
    [
      ['api', false],
      ['web', true],
    ]
  );
  assert.deepEqual(
    openFolders(workspace('w1', ['/work/api'])).map((f) => f.path),
    ['/work/api']
  );
});

test('what is claimed for a target: a folder by its path, a workspace by its ID', () => {
  assert.deepEqual(claimTargetOf(folder('/work/app')), folder('/work/app'));
  assert.deepEqual(claimTargetOf(workspace('w1', ['/work/api'])), {
    kind: 'workspace',
    id: 'w1',
  });
  assert.equal(claimTargetOf(null), null);
});
