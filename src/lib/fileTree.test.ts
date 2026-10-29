// The file tree's controller (fileTree.ts): a renamed or moved folder
// stays expanded at its new place, with what was open inside it.
// Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRoot, createSignal } from 'solid-js';
import { createFileTreeController } from './fileTree';
import type { DirEntryInfo } from '../types';

/** Folders of a fake disk, by path: their entries' names (a trailing / for a folder). */
let disk: Record<string, string[]> = {};
let read = (path: string): DirEntryInfo[] | Promise<DirEntryInfo[]> => entries(path);

function entries(path: string): DirEntryInfo[] {
  const names = disk[path];
  if (!names) throw new Error(`No such directory: ${path}`);
  return names.map((name) => {
    const isDir = name.endsWith('/');
    const clean = name.replace(/\/$/, '');
    const full = `${path}/${clean}`;
    return { id: full, name: clean, path: full, is_dir: isDir, ignored: false };
  });
}

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: { path: string }) => {
    if (cmd === 'read_dir_entries') return read(args.path);
    throw new Error(`unexpected ${cmd}`);
  },
};

const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test('a renamed folder stays expanded, and the folders open inside it', async () => {
  disk = {
    '/p': ['src/', 'README.md'],
    '/p/src': ['lib/', 'main.ts'],
    '/p/src/lib': ['math.ts'],
  };
  await createRoot(async (dispose) => {
    const [root] = createSignal<string | null>('/p');
    const tree = createFileTreeController(root);
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

test('the file tree stays loading until the root and restored folders have finished reading', async () => {
  const rootRead = deferred<DirEntryInfo[]>();
  const folderRead = deferred<DirEntryInfo[]>();
  read = (path) => (path === '/p' ? rootRead.promise : folderRead.promise);
  let dispose = () => {};
  try {
    const tree = createRoot((stop) => {
      dispose = stop;
      const [root, setRoot] = createSignal<string | null>(null);
      const tree = createFileTreeController(root);
      tree.presetExpanded('/p', ['/p/src']);
      setRoot('/p');
      return tree;
    });
    assert.equal(tree.loading(), true);
    rootRead.resolve([{ id: 'src', path: '/p/src', name: 'src', is_dir: true, ignored: false }]);
    await settle();
    assert.ok(tree.rows().some((row) => row.path === '/p/src'));
    assert.equal(tree.loading(), true);
    folderRead.resolve([]);
    await settle();
    assert.equal(tree.loading(), false);
  } finally {
    dispose();
    read = (path) => entries(path);
  }
});

test('a failed root read finishes loading and can be retried by refreshing', async () => {
  const pending = deferred<DirEntryInfo[]>();
  let reads = 0;
  read = () => {
    reads++;
    // A mistaken retry stays pending, so it fails the assertion instead
    // of starving the test's event loop with repeated rejections.
    return reads === 1 ? pending.promise : new Promise(() => {});
  };
  let dispose = () => {};
  const quiet = console.warn;
  console.warn = () => {};
  try {
    const tree = createRoot((stop) => {
      dispose = stop;
      return createFileTreeController(() => '/p');
    });
    assert.equal(tree.loading(), true);
    pending.reject(new Error('permission denied'));
    await settle();
    assert.equal(tree.loading(), false);
    assert.equal(reads, 1, 'a failed root read is not retried forever');
    assert.deepEqual(tree.rows(), []);

    disk = { '/p': ['README.md'] };
    read = (path) => entries(path);
    tree.refresh();
    await settle();
    assert.deepEqual(
      tree.rows().map((row) => row.name),
      ['README.md']
    );
  } finally {
    dispose();
    read = (path) => entries(path);
    console.warn = quiet;
  }
});
