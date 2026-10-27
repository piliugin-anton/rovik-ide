// rustAnalyzer.ts tests (node:test): a running rust-analyzer restarts for the
// folders that are there when they change, and only then. The app's commands
// are answered here: `ra_find` counts how often a server is looked for, which
// is how a (re)start is seen without a language server.
import '../../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ensureRustSupport, workspaceFoldersChanged } from './rustAnalyzer';

/** How often the binary was looked for: once per start or restart. */
let finds = 0;

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  unregisterCallback: () => {},
  invoke: async (cmd: string) => {
    if (cmd === 'ra_find') {
      finds++;
      return null;
    }
    return null;
  },
};

const settle = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

test('no server is started when the folders change and no Rust file was opened', async () => {
  workspaceFoldersChanged(['/work/api']);
  await settle();
  assert.equal(finds, 0);
});

test('a running server restarts for the folders that are there, and not for the same ones', async () => {
  ensureRustSupport(['/work/api']);
  await settle();
  assert.equal(finds, 1, 'started for the Rust file');
  workspaceFoldersChanged(['/work/api']);
  await settle();
  assert.equal(finds, 1, 'the same folders change nothing');
  workspaceFoldersChanged(['/work/api', '/work/web']);
  await settle();
  assert.equal(finds, 2, 'a folder came back: restarted for both');
});

test('with no folders left, the server stops and does not come back by itself', async () => {
  workspaceFoldersChanged([]);
  await settle();
  workspaceFoldersChanged(['/work/api']);
  await settle();
  assert.equal(finds, 2, 'nothing started again until a Rust file is opened');
});
