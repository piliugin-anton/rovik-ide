// The editor's tabs (lib/editorTabs.ts): files and browser tabs in one
// order, the files changed as before while the browser tabs keep their
// places, and a page opened, navigated and loaded again. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  isBrowserTab,
  isFileTab,
  navigated,
  reloaded,
  tabHints,
  unsavedIn,
  withFiles,
  withPage,
  type BrowserTab,
  type EditorTab,
} from './editorTabs';
import type { OpenFile } from '../types';

const file = (path: string, id = path): OpenFile => ({
  id,
  path,
  name: path.split('/').pop() ?? path,
  content: '',
  originalContent: '',
  dirty: false,
  language: 'plaintext',
});

const page = (path: string, address: string, loads = 0): BrowserTab => ({
  kind: 'browser',
  path,
  address,
  loads,
});

const keys = (tabs: EditorTab[]) => tabs.map((t) => t.path);

test('the files and the browser tabs are told apart', () => {
  assert.equal(isBrowserTab(page('browser:1', 'http://localhost:5173/')), true);
  assert.equal(isFileTab(page('browser:1', 'http://localhost:5173/')), false);
  assert.equal(isFileTab(file('/p/a.ts')), true);
});

test('files changed among browser tabs keep their places, new ones come last and gone ones go', () => {
  const tabs: EditorTab[] = [
    file('/p/a.ts'),
    page('browser:1', 'http://localhost:5173/'),
    file('/p/b.ts'),
    file('/p/c.ts'),
  ];
  const files = tabs.filter(isFileTab);
  const written = { ...files[1], content: 'b' };
  const next = withFiles(tabs, [files[0], written, file('/p/d.ts')]);
  assert.deepEqual(keys(next), ['/p/a.ts', 'browser:1', '/p/b.ts', '/p/d.ts']);
  assert.equal(next[2], written, 'the changed file in its place');
  assert.equal(next[1], tabs[1], 'the browser tab as it was');
  assert.deepEqual(keys(withFiles(tabs, [])), ['browser:1']);
});

test('a renamed file keeps its place by its id, and a re-created one by its path', () => {
  const tabs: EditorTab[] = [
    file('/p/a.ts', 'id-a'),
    page('browser:1', 'http://localhost:5173/'),
    file('/p/b.ts', 'id-b'),
  ];
  const renamed = withFiles(tabs, [file('/p/z.ts', 'id-a'), file('/p/b.ts', 'id-b')]);
  assert.deepEqual(keys(renamed), ['/p/z.ts', 'browser:1', '/p/b.ts']);
  const recreated = withFiles(tabs, [file('/p/a.ts', 'id-a'), file('/p/b.ts', 'id-new')]);
  assert.deepEqual(keys(recreated), ['/p/a.ts', 'browser:1', '/p/b.ts']);
});

test('a page opens in a new browser tab last, or loads again in the tab that shows it', () => {
  let n = 0;
  const key = () => `browser:${++n}`;
  const first = withPage([file('/p/a.ts')], 'http://localhost:5173/', key);
  assert.equal(first.path, 'browser:1');
  assert.deepEqual(first.tabs[1], page('browser:1', 'http://localhost:5173/'));

  const second = withPage(first.tabs, 'http://localhost:3000/', key);
  assert.deepEqual(keys(second.tabs), ['/p/a.ts', 'browser:1', 'browser:2']);

  const again = withPage(second.tabs, 'http://localhost:5173/', key);
  assert.equal(again.path, 'browser:1', 'the tab that shows it');
  assert.deepEqual(again.tabs[1], page('browser:1', 'http://localhost:5173/', 1), 'loaded again');
  assert.equal(again.tabs.length, 3);
  assert.equal(n, 2, 'no new key for it');
});

test('a browser tab navigated or reloaded is a new tab object, and the others stay as they were', () => {
  const tabs: EditorTab[] = [
    page('browser:1', 'http://localhost:5173/'),
    file('/p/a.ts'),
    page('browser:2', 'http://localhost:3000/'),
  ];
  const moved = navigated(tabs, 'browser:1', 'http://localhost:5173/about');
  assert.deepEqual(moved[0], page('browser:1', 'http://localhost:5173/about', 1));
  assert.equal(moved[1], tabs[1]);
  assert.equal(moved[2], tabs[2]);

  const again = reloaded(moved, 'browser:2');
  assert.deepEqual(again[2], page('browser:2', 'http://localhost:3000/', 1));
  assert.notEqual(again[2], moved[2]);
  assert.equal(again[0], moved[0]);
  assert.deepEqual(reloaded(tabs, '/p/a.ts'), tabs, 'a file is no browser tab');
});

const hintFolder = (path: string, name: string) => ({ path, name });

test('two package.json show their folders', () => {
  const tabs: EditorTab[] = [file('/work/api/package.json'), file('/work/web/package.json')];
  const hints = tabHints(tabs, [hintFolder('/work/api', 'api'), hintFolder('/work/web', 'web')]);
  assert.deepEqual(
    hints.get('/work/api/package.json'),
    'api',
    "the folder's stored name, not the one it would be given now"
  );
  assert.deepEqual(hints.get('/work/web/package.json'), 'web');
});

test('two index.ts in one project show their parent folders', () => {
  const tabs: EditorTab[] = [file('/p/a/src/index.ts'), file('/p/b/src/index.ts')];
  const hints = tabHints(tabs, [hintFolder('/p', 'p')]);
  assert.deepEqual(
    [hints.get('/p/a/src/index.ts'), hints.get('/p/b/src/index.ts')],
    ['a/src', 'b/src']
  );

  // The shortest end that tells them apart: "src" would be both.
  const siblings: EditorTab[] = [file('/p/a/index.ts'), file('/p/b/index.ts')];
  const near = tabHints(siblings, [hintFolder('/p', 'p')]);
  assert.deepEqual([near.get('/p/a/index.ts'), near.get('/p/b/index.ts')], ['a', 'b']);
});

test('a unique name shows alone, and a tab outside the folders gets no hint', () => {
  const tabs: EditorTab[] = [
    file('/work/api/package.json'),
    file('/work/api/only.ts'),
    file('/elsewhere/package.json'),
    page('browser:1', 'http://localhost:5173/'),
    // A stashed version's tab: its key is no path in the folder.
    { ...file('disk:readme', 'disk:1'), name: 'package.json' },
  ];
  const hints = tabHints(tabs, [hintFolder('/work/api', 'api')]);
  assert.equal(hints.has('/work/api/only.ts'), false, 'one tab of its name');
  // A tab in no folder of the window neither gets a hint nor makes one.
  assert.equal(hints.has('/elsewhere/package.json'), false);
  assert.equal(hints.has('/work/api/package.json'), false);
});

test('closing a folder settles only the unsaved files in it, and a read-only one has nothing to save', () => {
  const dirty = (path: string, extra: Partial<OpenFile> = {}): OpenFile => ({
    ...file(path),
    dirty: true,
    ...extra,
  });
  const files = [
    dirty('/work/api/a.ts'),
    dirty('/work/web/b.ts'),
    dirty('/work/api/stash.ts', { readOnly: true }),
    file('/work/api/clean.ts'),
  ];
  assert.deepEqual(
    unsavedIn(files).map((f) => f.path),
    ['/work/api/a.ts', '/work/web/b.ts'],
    'every folder, for leaving the app'
  );
  assert.deepEqual(
    unsavedIn(files, '/work/api').map((f) => f.path),
    ['/work/api/a.ts'],
    'only the leaving folder'
  );
  assert.deepEqual(
    unsavedIn(files, '/work/web').map((f) => f.path),
    ['/work/web/b.ts']
  );
  assert.deepEqual(unsavedIn(files.slice(2), '/work/api'), [], 'a read-only file is not saved');
});
