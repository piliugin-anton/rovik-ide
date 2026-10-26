// The file tree's controller (fileTree.ts): a renamed or moved folder
// stays expanded at its new place, with what was open inside it; and a
// window with several folders shows each as a row, read on its own.
// Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRoot, createSignal } from 'solid-js';
import { createFileTreeController, type TreeRoot } from './fileTree';
import type { DirEntryInfo } from '../types';

/** Folders of a fake disk, by path: their entries' names (a trailing / for a folder). */
let disk: Record<string, string[]> = {};

function entries(path: string): DirEntryInfo[] {
  const names = disk[path];
  if (!names) throw new Error(`No such directory: ${path}`);
  // The separator the folder itself is spelled with, as the backend lists it.
  const sep = path.includes('\\') && !path.includes('/') ? '\\' : '/';
  const base = path.replace(/[\\/]$/, '');
  return names.map((name) => {
    const isDir = name.endsWith('/') || name.endsWith('\\');
    const clean = name.replace(/[\\/]$/, '');
    const full = `${base}${sep}${clean}`;
    return { id: full, name: clean, path: full, is_dir: isDir, ignored: false };
  });
}

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: { path: string }) => {
    if (cmd === 'read_dir_entries') return entries(args.path);
    throw new Error(`unexpected ${cmd}`);
  },
};

const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** A project's folder: the tree's only root, with no row of its own. */
function project(path: string): TreeRoot[] {
  return [{ path, name: path, row: false }];
}

test('a renamed folder stays expanded, and the folders open inside it', async () => {
  disk = {
    '/p': ['src/', 'README.md'],
    '/p/src': ['lib/', 'main.ts'],
    '/p/src/lib': ['math.ts'],
  };
  await createRoot(async (dispose) => {
    const [roots] = createSignal<TreeRoot[]>(project('/p'));
    const tree = createFileTreeController(roots);
    await settle();
    tree.toggle({ path: '/p/src', isDir: true });
    await settle();
    tree.toggle({ path: '/p/src/lib', isDir: true });
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      ['/p/src', '/p/src/lib', '/p/src/lib/math.ts', '/p/src/main.ts', '/p/README.md']
    );

    // Renamed on disk, then the tree is told and its listing reloads.
    disk = {
      '/p': ['code/', 'README.md'],
      '/p/code': ['lib/', 'main.ts'],
      '/p/code/lib': ['math.ts'],
    };
    tree.moved('/p/src', '/p/code');
    tree.refresh('/p/src');
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      ['/p/code', '/p/code/lib', '/p/code/lib/math.ts', '/p/code/main.ts', '/p/README.md']
    );
    assert.equal(
      tree.files().some((f) => f.path.startsWith('/p/src/')),
      false,
      'files under the old name are gone'
    );
    dispose();
  });
});

test('a change at the top of a drive refreshes the drive root', async () => {
  disk = { 'D:\\': ['p\\'], 'D:\\p': ['a.ts'] };
  await createRoot(async (dispose) => {
    const [roots] = createSignal<TreeRoot[]>(project('D:\\'));
    const tree = createFileTreeController(roots);
    await settle();
    tree.toggle({ path: 'D:\\p', isDir: true });
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      ['D:\\p', 'D:\\p\\a.ts']
    );

    // A folder created in D:\ itself: the listing to reload is the root's.
    disk = { 'D:\\': ['p\\', 'q\\'], 'D:\\p': ['a.ts'], 'D:\\q': [] };
    tree.refresh('D:\\q');
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      ['D:\\p', 'D:\\p\\a.ts', 'D:\\q']
    );
    dispose();
  });
});

test('a workspace lists each folder as a row, each read from its own folder', async () => {
  disk = {
    '/work/api': ['src/', 'a.ts'],
    '/work/api/src': ['main.ts'],
    '/work/web': ['src/', 'b.ts'],
    '/work/web/src': ['app.ts'],
  };
  await createRoot(async (dispose) => {
    const [roots] = createSignal<TreeRoot[]>([
      { path: '/work/api', name: 'api', row: true },
      { path: '/work/web', name: 'web', row: true },
    ]);
    const tree = createFileTreeController(roots);
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => `${r.depth}:${r.path}`),
      [
        '0:/work/api',
        '1:/work/api/src',
        '1:/work/api/a.ts',
        '0:/work/web',
        '1:/work/web/src',
        '1:/work/web/b.ts',
      ]
    );
    assert.deepEqual(
      tree.rows().map((r) => r.name),
      ['api', 'src', 'a.ts', 'web', 'src', 'b.ts']
    );

    // Expanding a folder of the second lists only its own files.
    tree.toggle({ path: '/work/web/src', isDir: true });
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      [
        '/work/api',
        '/work/api/src',
        '/work/api/a.ts',
        '/work/web',
        '/work/web/src',
        '/work/web/src/app.ts',
        '/work/web/b.ts',
      ]
    );
    assert.equal(
      tree.expanded().has('/work/api/src'),
      false,
      "the other folder's folder stays closed"
    );
    dispose();
  });
});

test("a folder's move stays in it, and a change refreshes the folder it's in", async () => {
  disk = {
    '/work/api': ['src/', 'a.ts'],
    '/work/api/src': ['main.ts'],
    '/work/web': ['b.ts'],
  };
  await createRoot(async (dispose) => {
    const [roots] = createSignal<TreeRoot[]>([
      { path: '/work/api', name: 'api', row: true },
      { path: '/work/web', name: 'web', row: true },
    ]);
    const tree = createFileTreeController(roots);
    await settle();
    tree.toggle({ path: '/work/api/src', isDir: true });
    await settle();
    assert.equal(tree.expanded().has('/work/api/src'), true);

    // Renamed within the first folder: stays expanded there.
    disk = {
      '/work/api': ['lib/', 'a.ts'],
      '/work/api/lib': ['main.ts'],
      '/work/web': ['b.ts'],
    };
    tree.moved('/work/api/src', '/work/api/lib');
    tree.refresh('/work/api/src');
    await settle();
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      [
        '/work/api',
        '/work/api/lib',
        '/work/api/lib/main.ts',
        '/work/api/a.ts',
        '/work/web',
        '/work/web/b.ts',
      ]
    );
    assert.equal(tree.expanded().has('/work/api/lib'), true, 'still open at its new name');

    // A new file in the second folder: the second's listing reloads.
    disk = { ...disk, '/work/web': ['b.ts', 'c.ts'] };
    tree.refresh('/work/web/c.ts');
    await settle();
    assert.deepEqual(
      tree
        .rows()
        .slice(-3)
        .map((r) => r.path),
      ['/work/web', '/work/web/b.ts', '/work/web/c.ts']
    );
    dispose();
  });
});

test('a link in one folder to another lists rows of its own', async () => {
  disk = {
    '/work/api': ['web/'],
    '/work/api/web': ['b.ts'],
    '/work/web': ['b.ts'],
  };
  await createRoot(async (dispose) => {
    const [roots] = createSignal<TreeRoot[]>([
      { path: '/work/api', name: 'api', row: true },
      { path: '/work/web', name: 'web', row: true },
    ]);
    const tree = createFileTreeController(roots);
    await settle();
    tree.toggle({ path: '/work/api/web', isDir: true });
    await settle();
    // The same file, seen through the link and as the other folder's: two
    // rows, each under the path the window knows it by.
    assert.deepEqual(
      tree.rows().map((r) => r.path),
      ['/work/api', '/work/api/web', '/work/api/web/b.ts', '/work/web', '/work/web/b.ts']
    );
    assert.deepEqual(
      tree
        .files()
        .map((f) => f.path)
        .sort(),
      ['/work/api/web/b.ts', '/work/web/b.ts']
    );
    dispose();
  });
});
