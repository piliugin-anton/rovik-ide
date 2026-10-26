// openFolders.ts: what a window has open (a folder, or a workspace's folders),
// which folder a path is in, the folder being worked in, and the session key.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  activeFolder,
  folderOf,
  innermostFolder,
  openFolders,
  sessionKey,
  type OpenFolder,
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
