// openFolders.ts: what a window has open (a folder, or a workspace's folders),
// which folder a path is in, the folder being worked in, and the session key.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  activeFolder,
  chatFolderFor,
  chatFolderNames,
  chatOwners,
  chatPathOf,
  chatPathToAbsolute,
  chatRootOf,
  folderOf,
  innermostFolder,
  listedBy,
  openFolders,
  pathsByFolder,
  sameOwners,
  runPlace,
  sessionKey,
  withoutFolder,
  workspaceAsTarget,
  workspaceIdOf,
  type ChatRoot,
  type OpenFolder,
  type WindowTarget,
} from './openFolders';

const api: OpenFolder = { path: '/work/api', name: 'api', missing: false };
const web: OpenFolder = { path: '/work/web', name: 'web', missing: false };

test('a project is a list of one folder, named after its folder', () => {
  assert.deepEqual(openFolders({ kind: 'folder', path: '/work/api' }), [
    { path: '/work/api', name: 'api', missing: false },
  ]);
  assert.deepEqual(openFolders(null), []);
});

test('a workspace opens its folders under the names it keeps for them', () => {
  const folders = openFolders({
    kind: 'workspace',
    id: 'w1',
    name: 'Shop',
    folders: [
      { path: '/work/api', name: 'api' },
      { path: '/work/web', name: 'storefront' },
    ],
  });
  assert.deepEqual(folders, [
    { path: '/work/api', name: 'api', missing: false },
    { path: '/work/web', name: 'storefront', missing: false },
  ]);
});

test("a path is in the folder that holds it, and in none outside the window's", () => {
  assert.equal(folderOf([api, web], '/work/web/src/a.ts'), web);
  assert.equal(folderOf([api, web], '/work/api/a.ts'), api);
  assert.equal(folderOf([api, web], '/work/api'), api);
  assert.equal(folderOf([api, web], '/work/apiary/a.ts'), null);
  assert.equal(folderOf([api, web], '/elsewhere/a.ts'), null);
  assert.equal(folderOf([], '/work/api/a.ts'), null);
});

test("the innermost folder that holds a path is the one it's worked in", () => {
  assert.equal(innermostFolder(['/work/api', '/work/web'], '/work/web/a.ts'), '/work/web');
  assert.equal(innermostFolder(['/work/api', '/work/web'], '/elsewhere/a.ts'), null);
  // Not wanted (D5), but a window's state comes from the database: the
  // folder inside another decides for its own files.
  assert.equal(
    innermostFolder(['/work', '/work/packages/web'], '/work/packages/web/a.ts'),
    '/work/packages/web'
  );
  assert.equal(innermostFolder([], '/work/a.ts'), null);
});

test("the active folder is the active tab's, else the selected row's, else the first", () => {
  assert.equal(activeFolder([api, web], '/work/web/a.ts', '/work/api/b.ts'), web);
  assert.equal(activeFolder([api, web], null, '/work/web/b.ts'), web);
  // A tab in no folder of the window leaves it to the selection.
  assert.equal(activeFolder([api, web], '/elsewhere/a.ts', '/work/web/b.ts'), web);
  // Nothing to go by: the first folder.
  assert.equal(activeFolder([api, web], null, null), api);
  assert.equal(activeFolder([], null, null), null);
});

test('a session is kept under its folder, or under the workspace', () => {
  assert.equal(sessionKey({ kind: 'folder', path: '/work/api' }), '/work/api');
  assert.equal(
    sessionKey({ kind: 'workspace', id: 'w1', name: 'Shop', folders: [] }),
    'workspace:w1'
  );
  assert.equal(sessionKey(null), null);
});

test('a session key names its workspace, and a folder’s names none', () => {
  const key = sessionKey({ kind: 'workspace', id: 'w1', name: 'Shop', folders: [] });
  assert.equal(workspaceIdOf(key), 'w1');
  assert.equal(workspaceIdOf(sessionKey({ kind: 'folder', path: '/work/api' })), null);
  assert.equal(workspaceIdOf(null), null);
});

test("a session's open tree folders are registered on the folder each is in", () => {
  // The tree's roots are the window's folders, whatever key the window's
  // session is kept under: a workspace's is `workspace:<id>`, which is no
  // root's path, so what it saved is split folder by folder (B04).
  assert.deepEqual(pathsByFolder(['/work/api/src', '/work/web/src'], ['/work/api', '/work/web']), [
    { folder: '/work/api', paths: ['/work/api/src'] },
    { folder: '/work/web', paths: ['/work/web/src'] },
  ]);
  // A folder with nothing open in it, and a path in none of them, which
  // isn't restored.
  assert.deepEqual(pathsByFolder(['/elsewhere/src'], ['/work/api', '/work/web']), [
    { folder: '/work/api', paths: [] },
    { folder: '/work/web', paths: [] },
  ]);
  // A folder inside another decides for its own paths.
  assert.deepEqual(pathsByFolder(['/work/packages/web/src'], ['/work', '/work/packages/web']), [
    { folder: '/work', paths: [] },
    { folder: '/work/packages/web', paths: ['/work/packages/web/src'] },
  ]);
  assert.deepEqual(pathsByFolder([], []), []);
});

test('withoutFolder leaves the other folders in their order', () => {
  const target = {
    kind: 'workspace' as const,
    id: 'w1',
    name: 'Shop',
    folders: [
      { name: 'api', path: '/work/api' },
      { name: 'web', path: '/work/web' },
      { name: 'docs', path: '/work/docs' },
    ],
  };
  /** The folders that stay, by name: a project's none, it has no folder row. */
  const left = (name: string) => {
    const next = withoutFolder(target, name);
    return next.kind === 'workspace' ? next.folders.map((folder) => folder.name) : [];
  };
  assert.deepEqual(left('web'), ['api', 'docs']);
  // A name no folder has leaves them as they are.
  assert.deepEqual(left('gone'), ['api', 'web', 'docs']);
  assert.deepEqual(withoutFolder({ kind: 'folder', path: '/work/api' }, 'api'), {
    kind: 'folder',
    path: '/work/api',
  });
});

test("workspaceAsTarget keeps each folder's stored name", () => {
  assert.deepEqual(
    workspaceAsTarget({
      id: 'w1',
      name: 'Shop',
      folders: [
        { name: 'api', path: '/work/api', missing: false },
        { name: 'storefront', path: '/work/web', missing: true },
      ],
    }),
    {
      kind: 'workspace',
      id: 'w1',
      name: 'Shop',
      folders: [
        { name: 'api', path: '/work/api' },
        { name: 'storefront', path: '/work/web' },
      ],
    }
  );
});

/** `folder` resolved as the project `id`. */
const resolved = (folder: OpenFolder, id: string): OpenFolder => ({
  ...folder,
  project: { id, name: folder.name, path: folder.path, idFile: true },
});

const shop: WindowTarget = {
  kind: 'workspace',
  id: 'shop',
  name: 'Shop',
  folders: [
    { path: '/work/api', name: 'api' },
    { path: '/work/web', name: 'web' },
  ],
};

test("a workspace's owners are its own and each folder's, once each folder is resolved", () => {
  // The panel waits while a folder resolves, as it waits for a project's.
  assert.equal(chatOwners(shop, [resolved(api, 'p-api'), web], false), undefined);
  const owners = chatOwners(shop, [resolved(api, 'p-api'), resolved(web, 'p-web')], false);
  assert.deepEqual(owners, { target: 'workspace:shop', projects: ['shop', 'p-api', 'p-web'] });
  // A missing folder isn't resolved: nothing waits for it, and its chats aren't listed.
  assert.deepEqual(chatOwners(shop, [resolved(api, 'p-api'), { ...web, missing: true }], false), {
    target: 'workspace:shop',
    projects: ['shop', 'p-api'],
  });
});

test('a project lists its own chats, and a window with nothing open the chats with no project', () => {
  const project: WindowTarget = { kind: 'folder', path: '/work/api' };
  assert.deepEqual(chatOwners(project, [resolved(api, 'p-api')], false), {
    target: '/work/api',
    projects: ['p-api'],
  });
  assert.equal(chatOwners(null, [], false), undefined, 'not claimed for the window yet');
  assert.deepEqual(chatOwners(null, [], true), { target: null, projects: null });
});

test('owners list their own projects’ chats, and the chats with no project only with none open', () => {
  const owners = { target: 'workspace:shop', projects: ['shop', 'p-api'] };
  assert.equal(listedBy(owners, 'p-api'), true);
  assert.equal(listedBy(owners, 'p-web'), false);
  assert.equal(listedBy(owners, null), false);
  assert.equal(listedBy({ target: null, projects: null }, null), true);
  assert.equal(sameOwners(owners, { target: 'workspace:shop', projects: ['shop', 'p-api'] }), true);
  assert.equal(sameOwners(owners, { target: 'workspace:shop', projects: ['shop'] }), false);
  assert.equal(sameOwners(owners, { target: '/work/api', projects: ['shop', 'p-api'] }), false);
});

test("a folder chat works in the window's folder whose project it is", () => {
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web')];
  assert.deepEqual(chatRootOf({ projectId: 'p-web' }, folders, 'shop'), {
    kind: 'folder',
    folders: [folders[1]],
  });
  // Its folder left the window, or is missing: nowhere to work here.
  assert.equal(chatRootOf({ projectId: 'p-gone' }, folders, 'shop'), null);
  assert.equal(
    chatRootOf({ projectId: 'p-web' }, [folders[0], { ...folders[1], missing: true }], 'shop'),
    null
  );
  // With no folder open a chat has no project, and no folder.
  assert.equal(chatRootOf({ projectId: null }, folders, null), null);
});

test('a workspace chat works in the folders checked for it, or in each of the workspace’s for All folders', () => {
  const docs: OpenFolder = { path: '/work/docs', name: 'docs', missing: true };
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web'), docs];
  const root = (folders_: string | null) =>
    chatRootOf({ projectId: 'shop', folders: folders_ }, folders, 'shop');
  // In the workspace's order, whatever order they were checked in.
  assert.deepEqual(root('["web","api"]'), { kind: 'workspace', folders: folders.slice(0, 2) });
  // All folders: a missing one among them, which its run asks for (D20).
  assert.deepEqual(root('all'), { kind: 'workspace', folders });
  assert.deepEqual(root(null), { kind: 'workspace', folders }, 'one that names none follows it');
  // A name the workspace no longer has is no folder of the chat's.
  assert.deepEqual(root('["api","infra"]'), { kind: 'workspace', folders: [folders[0]] });
  assert.deepEqual(root('{"not":"names"}'), { kind: 'workspace', folders: [] });
  // Another workspace's chat doesn't work in this window's folders.
  assert.equal(chatRootOf({ projectId: 'other', folders: '["api"]' }, folders, 'shop'), null);
  assert.equal(chatRootOf({ projectId: 'shop', folders: '["api"]' }, folders, null), null);
});

test("a file of a chat the list doesn't have, a subagent's, is in the open chat's folder", () => {
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web')];
  const open = { projectId: 'p-web' };
  assert.deepEqual(chatFolderFor(undefined, open, folders, 'shop')?.folders, [folders[1]]);
  // A listed chat's own folder, whichever chat is open.
  assert.deepEqual(chatFolderFor({ projectId: 'p-api' }, open, folders, 'shop')?.folders, [
    folders[0],
  ]);
  // A workspace chat's subagent works across its folders, as it does.
  const across = { projectId: 'shop', folders: 'all' };
  assert.deepEqual(chatFolderFor(undefined, across, folders, 'shop'), {
    kind: 'workspace',
    folders,
  });
  // Neither works here: the window's first folder; no folder open: none.
  assert.deepEqual(chatFolderFor(undefined, null, folders, 'shop'), {
    kind: 'folder',
    folders: [folders[0]],
  });
  assert.equal(chatFolderFor(undefined, null, [], null), null);
});

/** A folder chat's root: `folder`. */
const inFolder = (folder: OpenFolder): ChatRoot => ({ kind: 'folder', folders: [folder] });
/** A workspace chat's root: `folders`. */
const across = (...folders: OpenFolder[]): ChatRoot => ({ kind: 'workspace', folders });

test("a folder chat's path is joined to its folder", () => {
  const root = inFolder(resolved(web, 'p-web'));
  assert.equal(chatPathToAbsolute(root, 'src/cart.ts'), '/work/web/src/cart.ts');
  assert.equal(chatPathToAbsolute(root, './src/cart.ts'), '/work/web/src/cart.ts');
  assert.equal(chatPathToAbsolute(root, '/work/web/src/cart.ts'), '/work/web/src/cart.ts');
  assert.equal(chatPathToAbsolute(null, 'src/cart.ts'), null, 'no folder here: no file');
});

test("a workspace chat's path is found in the folder its first part names, and an unknown name names no file", () => {
  const root = across(resolved(api, 'p-api'), resolved(web, 'p-web'));
  assert.equal(chatPathToAbsolute(root, 'web/src/cart.ts'), '/work/web/src/cart.ts');
  assert.equal(chatPathToAbsolute(root, './api/routes.ts'), '/work/api/routes.ts');
  assert.equal(chatPathToAbsolute(root, '/work/web/src/cart.ts'), '/work/web/src/cart.ts');
  assert.equal(chatPathToAbsolute(root, 'docs/readme.md'), null, 'no folder of the chat’s');
  assert.equal(chatPathToAbsolute(root, 'src/cart.ts'), null, 'a folder chat’s path');
  assert.equal(chatPathToAbsolute(across(), 'web/src/cart.ts'), null, 'no folder left');
});

test("a file is named from its folder, and one outside the chat's folder has no name", () => {
  const root = inFolder(resolved(web, 'p-web'));
  assert.equal(chatPathOf(root, '/work/web/src/cart.ts'), 'src/cart.ts');
  assert.equal(chatPathOf(root, '/work/api/src/routes.ts'), null, "another folder's");
  assert.equal(chatPathOf(root, '/work/web2/x.ts'), null, 'a folder beside it with a longer name');
  assert.equal(chatPathOf(null, '/work/web/src/cart.ts'), null);
});

test("a workspace chat names a file under its folder's name, and one of a folder it doesn't work in not at all", () => {
  const root = across(resolved(api, 'p-api'), resolved(web, 'p-web'));
  assert.equal(chatPathOf(root, '/work/web/src/cart.ts'), 'web/src/cart.ts');
  assert.equal(chatPathOf(root, '/work/api/routes.ts'), 'api/routes.ts');
  assert.equal(chatPathOf(root, '/work/api'), 'api', 'the folder itself');
  assert.equal(chatPathOf(across(resolved(api, 'p-api')), '/work/web/src/cart.ts'), null);
  assert.equal(chatPathOf(root, '/work/docs/readme.md'), null, 'in none of the window’s');
});

test('a Windows file is named with / and found again', () => {
  const folder = resolved({ path: 'C:\\work\\web', name: 'web', missing: false }, 'p-web');
  const file = 'C:\\work\\web\\src\\cart.ts';
  const named = chatPathOf(inFolder(folder), file);
  assert.equal(named, 'src/cart.ts');
  assert.equal(chatPathToAbsolute(inFolder(folder), named!), file);
  // A workspace chat's too, under its folder's name.
  const workspaceNamed = chatPathOf(across(folder), file);
  assert.equal(workspaceNamed, 'web/src/cart.ts');
  assert.equal(chatPathToAbsolute(across(folder), workspaceNamed!), file);
  // A drive root holds its files too.
  const drive = resolved({ path: 'D:\\', name: 'D', missing: false }, 'p-d');
  assert.equal(chatPathOf(inFolder(drive), 'D:\\notes\\todo.md'), 'notes/todo.md');
});

test('on Windows a file whose folder is spelled in another case is named from it', () => {
  // The file system ignores case there: an editor's path with `c:\Work\WEB`
  // is a file of the folder opened as `C:\work\web`.
  const saved = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  Object.defineProperty(globalThis, 'navigator', {
    value: { platform: 'Win32' },
    configurable: true,
  });
  try {
    const root = inFolder(
      resolved({ path: 'C:\\work\\web', name: 'web', missing: false }, 'p-web')
    );
    assert.equal(chatPathOf(root, 'c:\\Work\\WEB\\src\\Cart.ts'), 'src/Cart.ts');
  } finally {
    if (saved) Object.defineProperty(globalThis, 'navigator', saved);
    else delete (globalThis as { navigator?: unknown }).navigator;
  }
});

test("a chat's row says its folder, a workspace chat's the folders checked or all of them", () => {
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web')];
  const names = (dialog: { projectId: string; folders?: string }) =>
    chatFolderNames(dialog, folders, 'shop');
  assert.deepEqual(names({ projectId: 'p-web' }), ['web']);
  assert.deepEqual(names({ projectId: 'shop', folders: '["web","api"]' }), ['api', 'web']);
  assert.equal(names({ projectId: 'shop', folders: 'all' }), 'all');
  assert.equal(names({ projectId: 'p-gone' }), null);
  assert.deepEqual(names({ projectId: 'shop', folders: '["api","infra"]' }), ['api']);
  assert.deepEqual(names({ projectId: 'shop', folders: '{"not":"names"}' }), []);
});

test("a folder chat's run is sent its folder, and a chat with none open here doesn't run", () => {
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web')];
  const place = (dialog: { projectId: string | null; folders?: string }) =>
    runPlace(dialog, chatRootOf(dialog, folders, 'shop'));
  assert.deepEqual(place({ projectId: 'p-web' }), { root: '/work/web' });
  // With no folder open, a chat works in none.
  assert.deepEqual(place({ projectId: null }), { root: null });
  assert.match(
    (place({ projectId: 'p-gone' }) as { why: string }).why,
    /^This chat’s folder isn’t open here/
  );
  assert.match(
    (place({ projectId: 'other', folders: 'all' }) as { why: string }).why,
    /^This chat’s workspace isn’t open here/
  );
});

test("a workspace chat's run is sent no root and no folders, and a missing folder stops it, naming it", () => {
  const docs: OpenFolder = { path: '/work/docs', name: 'docs', missing: true };
  const infra: OpenFolder = { path: '/work/infra', name: 'infra', missing: true };
  const folders = [resolved(api, 'p-api'), resolved(web, 'p-web'), docs, infra];
  const place = (checked: string) => {
    const dialog = { projectId: 'shop', folders: checked };
    return runPlace(dialog, chatRootOf(dialog, folders, 'shop'));
  };
  // The backend reads its folders itself (4.1).
  assert.deepEqual(place('["api","web"]'), { root: null });
  assert.deepEqual(place('["api","docs"]'), {
    why: 'docs isn’t there: locate it, or remove it from the workspace, to go on with this chat.',
  });
  assert.deepEqual(place('all'), {
    why: 'docs and infra aren’t there: locate them, or remove them from the workspace, to go on with this chat.',
  });
});
