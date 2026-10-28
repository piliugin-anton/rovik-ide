// switchTarget.ts tests (node:test): opening a target in this window — a
// folder, or a workspace's folders — in the order docs/design/workspaces-
// plan.md (3.3) gives, and what a switch that doesn't happen leaves as it
// was.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { OpenFolder } from './openFolders';

import type { WindowTarget } from './openFolders';

const {
  addFolders,
  claimTargetOf,
  growsInto,
  growTarget,
  locateFolder,
  openAtStart,
  presentFolders,
  removeFolder,
  resolvable,
  switchTarget,
} = await import('./switchTarget');
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

/** What Remove and Locate… make of a target, for the recorded order. */
const pathsOf = (target: WindowTarget) =>
  target.kind === 'workspace' ? target.folders.map((f) => f.path).join(',') : target.path;

/** The calls a folder's removal or relocation makes, each recorded. */
function changes(over: Record<string, unknown> = {}) {
  const order: string[] = [];
  const made = {
    settle: async (folder: OpenFolder) => (order.push(`settle:${folder.path}`), true),
    remove: async (id: string, name: string) => void order.push(`remove:${id}:${name}`),
    check: async (path: string) => (order.push(`check:${path}`), true),
    relocate: async (id: string, name: string, to: string) => {
      order.push(`relocate:${id}:${name}:${to}`);
      return {
        id,
        name: 'Shop',
        folders: [
          { name: 'api', path: '/work/api', missing: false },
          { name, path: to, missing: false },
        ],
      };
    },
    follow: (from: string, to: string) => void order.push(`follow:${from}:${to}`),
    claim: async (target: WindowTarget) => (order.push(`claim:${pathsOf(target)}`), true),
    show: async (target: WindowTarget) => void order.push(`show:${pathsOf(target)}`),
    resolve: (path: string) => void order.push(`resolve:${path}`),
    ...over,
  };
  return { order, made };
}

const webOf = (target: WindowTarget) => openFolders(target).find((f) => f.name === 'web')!;

test('removing a folder settles its tabs, removes it, then claims the workspace without it', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes();
  assert.equal(await removeFolder(target, webOf(target), made), true);
  assert.deepEqual(order, [
    'settle:/work/web',
    'remove:w1:web',
    'claim:/work/api',
    'show:/work/api',
  ]);
});

test('a cancelled settle removes nothing', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes({
    settle: async (folder: OpenFolder) => (order.push(`settle:${folder.path}`), false),
  });
  assert.equal(await removeFolder(target, webOf(target), made), false);
  assert.deepEqual(order, ['settle:/work/web'], 'the workspace still has both folders');
});

test('a refused removal claims nothing', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes({
    remove: async (id: string, name: string) => {
      order.push(`remove:${id}:${name}`);
      throw new Error('/work/web: a workspace keeps at least one folder');
    },
  });
  await assert.rejects(removeFolder(target, webOf(target), made), /at least one folder/);
  assert.deepEqual(order, ['settle:/work/web', 'remove:w1:web'], 'nothing else happened');
});

test('Locate… checks the new place first, then points the folder’s tabs at it', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes();
  assert.equal(await locateFolder(target, webOf(target), '/mnt/web', made), true);
  assert.deepEqual(order, [
    'check:/mnt/web',
    'relocate:w1:web:/mnt/web',
    'follow:/work/web:/mnt/web',
    'claim:/work/api,/mnt/web',
    'show:/work/api,/mnt/web',
    'resolve:/mnt/web',
  ]);
});

test('a new place another window has is refused and nothing moves', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes({
    check: async (path: string) => (order.push(`check:${path}`), false),
  });
  assert.equal(await locateFolder(target, webOf(target), '/mnt/web', made), false);
  assert.deepEqual(order, ['check:/mnt/web'], 'relocated: no');
});

test('a claim refused after the removal still shows the workspace the backend keeps', async () => {
  // Another window took the workspace between this window's last claim and
  // this one: the folder is gone from the database all the same, so the tree
  // must not go on showing it.
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes({
    claim: async (next: WindowTarget) => (order.push(`claim:${pathsOf(next)}`), false),
  });
  assert.equal(await removeFolder(target, webOf(target), made), true, 'the workspace changed');
  assert.deepEqual(order, [
    'settle:/work/web',
    'remove:w1:web',
    'claim:/work/api',
    'show:/work/api',
  ]);
});

test('a claim refused after Locate… still shows the folder at its new place', async () => {
  const target = workspace('w1', ['/work/api', '/work/web']);
  const { order, made } = changes({
    claim: async (next: WindowTarget) => (order.push(`claim:${pathsOf(next)}`), false),
  });
  assert.equal(await locateFolder(target, webOf(target), '/mnt/web', made), true, 'it moved');
  assert.deepEqual(order, [
    'check:/mnt/web',
    'relocate:w1:web:/mnt/web',
    'follow:/work/web:/mnt/web',
    'claim:/work/api,/mnt/web',
    'show:/work/api,/mnt/web',
    'resolve:/mnt/web',
  ]);
});

/** The calls growing into a target, or adding folders, make, each recorded. */
function grows(over: Record<string, unknown> = {}) {
  const order: string[] = [];
  const made = {
    check: async (target: WindowTarget) => (order.push(`check:${pathsOf(target)}`), true),
    keepTabs: () => void order.push('keepTabs'),
    claim: async (target: WindowTarget) => (order.push(`claim:${pathsOf(target)}`), true),
    show: async (target: WindowTarget) => void order.push(`show:${pathsOf(target)}`),
    resolve: (added: OpenFolder[]) =>
      void order.push(
        `resolve:${added.map((f) => `${f.path}${f.missing ? ' (missing)' : ''}`).join(',')}`
      ),
    exists: async (path: string) => path !== '/mnt/gone',
    problem: async (_id: string, path: string, others: string[]) => {
      order.push(`problem:${path}:${others.join(',')}`);
      return path === '/work/api/src' ? 'It is inside api.' : null;
    },
    free: async (path: string) => (order.push(`free:${path}`), path !== '/work/blog'),
    refused: (path: string, why: string) => void order.push(`refused:${path}:${why}`),
    add: async (id: string, paths: string[]) => {
      order.push(`add:${id}:${paths.join(',')}`);
      return {
        id,
        name: 'Shop',
        folders: ['/work/api', ...paths].map((path) => ({
          name: path.split('/').pop()!,
          path,
          missing: false,
        })),
      };
    },
    ...over,
  };
  return { order, made };
}

test('a workspace made from the open folder grows the window: nothing settles, and only its new folders resolve', async () => {
  const { order, made } = grows();
  const next = workspace('w1', ['/work/api', '/work/web', '/mnt/gone']);
  assert.equal(await growTarget(folder('/work/api'), next, made), true);
  assert.deepEqual(order, [
    'check:/work/api,/work/web,/mnt/gone',
    'keepTabs',
    'claim:/work/api,/work/web,/mnt/gone',
    'show:/work/api,/work/web,/mnt/gone',
    'resolve:/work/web,/mnt/gone (missing)',
  ]);
});

test('growing into a target another window has a folder of changes nothing here', async () => {
  const { order, made } = grows({
    check: async (target: WindowTarget) => (order.push(`check:${pathsOf(target)}`), false),
  });
  const next = workspace('w1', ['/work/api', '/work/web']);
  assert.equal(await growTarget(folder('/work/api'), next, made), false);
  assert.deepEqual(order, ['check:/work/api,/work/web']);
});

test('the window grows into a workspace made from the folder it has open, and switches to any other', () => {
  const shop = workspace('w1', ['/work/api', '/work/web']);
  assert.equal(growsInto(folder('/work/api'), shop), true);
  assert.equal(growsInto(folder('/work/blog'), shop), false, 'made from other folders');
  assert.equal(growsInto(null, shop), false, 'nothing open');
  assert.equal(growsInto(workspace('w0', ['/work/api']), shop), false, 'from a workspace');
  assert.equal(growsInto(folder('/work/api'), folder('/work/api')), false);
});

test('folders added are checked against the workspace and those picked before them; those that can join join', async () => {
  const { order, made } = grows();
  const shop = workspace('w1', ['/work/api']);
  const picked = ['/work/web', '/work/api/src', '/work/blog', '/work/docs'];
  assert.equal(await addFolders(shop, picked, made), true);
  assert.deepEqual(order, [
    'problem:/work/web:',
    'free:/work/web',
    'problem:/work/api/src:/work/web',
    'refused:/work/api/src:It is inside api.',
    'problem:/work/blog:/work/web',
    'free:/work/blog',
    'problem:/work/docs:/work/web',
    'free:/work/docs',
    'add:w1:/work/web,/work/docs',
    'check:/work/api,/work/web,/work/docs',
    'keepTabs',
    'claim:/work/api,/work/web,/work/docs',
    'show:/work/api,/work/web,/work/docs',
    'resolve:/work/web,/work/docs',
  ]);
});

test('no folder that can join: nothing is added, claimed or shown', async () => {
  const { order, made } = grows();
  assert.equal(await addFolders(workspace('w1', ['/work/api']), ['/work/blog'], made), false);
  assert.deepEqual(order, ['problem:/work/blog:', 'free:/work/blog']);
});

test('a project has no folders added to it', async () => {
  const { order, made } = grows();
  assert.equal(await addFolders(folder('/work/api'), ['/work/web'], made), false);
  assert.deepEqual(order, []);
});
