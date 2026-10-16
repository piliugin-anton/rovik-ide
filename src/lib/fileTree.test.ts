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
    if (cmd === 'read_dir_entries') return entries(args.path);
    throw new Error(`unexpected ${cmd}`);
  },
};

const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

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
