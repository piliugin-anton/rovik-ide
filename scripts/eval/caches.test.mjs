// caches.mjs tests. Run with: npm run test:eval
import { it } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { copyTree, fingerprint, protect } from './caches.mjs';

it('a run gets its own copy, and a changed cache is noticed', () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-cache-'));
  const cache = join(dir, 'node_modules');
  mkdirSync(join(cache, 'pkg'), { recursive: true });
  writeFileSync(join(cache, 'pkg', 'index.js'), 'module.exports = 1;\n');
  const before = fingerprint(cache);

  const ws = join(dir, 'ws');
  mkdirSync(ws);
  copyTree(cache, join(ws, 'node_modules'));
  // The agent edits its copy: the cache stays as it was.
  writeFileSync(join(ws, 'node_modules', 'pkg', 'index.js'), 'module.exports = 2;\n');
  assert.equal(readFileSync(join(cache, 'pkg', 'index.js'), 'utf8'), 'module.exports = 1;\n');
  assert.equal(fingerprint(cache), before);

  writeFileSync(join(cache, 'pkg', 'extra.js'), '');
  assert.notEqual(fingerprint(cache), before, 'a change shows');
});

it('a protected cache refuses writes', { skip: process.getuid?.() === 0 }, () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-cache-'));
  mkdirSync(join(dir, 'venv'));
  protect(join(dir, 'venv'));
  assert.throws(() => writeFileSync(join(dir, 'venv', 'x'), ''));
  assert.ok(!existsSync(join(dir, 'venv', 'x')));
});
