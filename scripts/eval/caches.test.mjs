// caches.mjs tests. Run with: npm run test:eval
import { it } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  chownSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  rmSync,
  statSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { changedCaches, copyTree, fingerprint, protect } from './caches.mjs';

const ROOT = process.getuid?.() === 0;
/** As root, the cache's owner in these tests: root's writes aren't refused. */
const NOBODY = 65534;

const chownTree = (p, id) => {
  chownSync(p, id, id);
  if (lstatSync(p).isDirectory()) for (const n of readdirSync(p)) chownTree(join(p, n), id);
};

/**
 * Runs `code` (with `dir` set to `dir`) as `dir`'s owner, who isn't root:
 * as root, the tree is given to `nobody` and the code runs as `nobody`.
 */
const asOwner = (dir, code) => {
  if (ROOT) chownTree(dir, NOBODY);
  const r = spawnSync(
    process.execPath,
    ['-e', `const fs = require('node:fs'); const dir = process.argv[1]; ${code}`, dir],
    { encoding: 'utf8', ...(ROOT ? { uid: NOBODY, gid: NOBODY } : {}) }
  );
  if (r.error) throw r.error;
  return r;
};

/** A small venv-like cache: a folder, a nested folder, files. */
const venv = () => {
  const base = mkdtempSync(join(tmpdir(), 'eval-cache-'));
  const dir = join(base, 'venv');
  mkdirSync(join(dir, 'lib', 'site-packages', 'pytest'), { recursive: true });
  writeFileSync(join(dir, 'lib', 'site-packages', 'pytest', '__init__.py'), '');
  mkdirSync(join(dir, 'bin'));
  writeFileSync(join(dir, 'bin', 'pytest'), '#!/bin/sh\n', { mode: 0o755 });
  if (ROOT) chownTree(base, NOBODY);
  return { base, dir };
};
const writeBits = (p) => statSync(p).mode & 0o222;

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

it('a copy keeps symlinks as symlinks and executables executable', () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-cache-'));
  const cache = join(dir, 'node_modules');
  mkdirSync(join(cache, 'pkg'), { recursive: true });
  mkdirSync(join(cache, '.bin'));
  writeFileSync(join(cache, 'pkg', 'cli.js'), '#!/usr/bin/env node\n', { mode: 0o755 });
  writeFileSync(join(cache, 'pkg', 'index.js'), '', { mode: 0o644 });
  symlinkSync('../pkg/cli.js', join(cache, '.bin', 'cli'));
  const copy = join(dir, 'ws', 'node_modules');
  mkdirSync(join(dir, 'ws'));
  copyTree(cache, copy);
  const link = join(copy, '.bin', 'cli');
  assert.ok(lstatSync(link).isSymbolicLink(), 'still a symlink');
  assert.equal(readlinkSync(link), '../pkg/cli.js', 'same relative target');
  assert.equal(statSync(join(copy, 'pkg', 'cli.js')).mode & 0o777, 0o755);
  assert.equal(statSync(join(copy, 'pkg', 'index.js')).mode & 0o777, 0o644);
  assert.ok(!existsSync(join(copy, 'node_modules')), 'not copied into itself');
});

it('protect makes files read-only and leaves folders writable', () => {
  const { dir } = venv();
  // A folder made read-only by an earlier `chmod -R a-w` gets its write bit back.
  chmodSync(join(dir, 'lib'), 0o555);
  protect(dir);
  for (const f of ['bin/pytest', 'lib/site-packages/pytest/__init__.py'])
    assert.equal(writeBits(join(dir, f)), 0, `${f} is read-only`);
  assert.equal(statSync(join(dir, 'bin', 'pytest')).mode & 0o111, 0o111, 'still executable');
  for (const d of ['', 'bin', 'lib', 'lib/site-packages'])
    assert.ok(statSync(join(dir, d)).mode & 0o200, `${d || 'the cache'} keeps its write bit`);
});

it('a protected cache refuses writes to its files', () => {
  const { dir } = venv();
  protect(dir);
  const r = asOwner(
    dir,
    `try { fs.writeFileSync(dir + '/bin/pytest', 'changed'); } catch (e) { console.log(e.code); }`
  );
  assert.equal(r.stdout.trim(), 'EACCES', r.stderr);
  assert.equal(readFileSync(join(dir, 'bin', 'pytest'), 'utf8'), '#!/bin/sh\n');
});

it('protect_then_rmSync_as_non_root: its owner can delete a protected cache', () => {
  const { dir } = venv();
  protect(dir);
  const r = asOwner(dir, `fs.rmSync(dir, { recursive: true, force: true });`);
  assert.equal(r.status, 0, r.stderr);
  assert.ok(!existsSync(dir));
});

it('a fingerprint ignores access times and the order files were made in', () => {
  const make = (order) => {
    const dir = mkdtempSync(join(tmpdir(), 'eval-cache-'));
    const cache = join(dir, 'c');
    mkdirSync(cache);
    for (const name of order) writeFileSync(join(cache, name), name);
    for (const name of order) utimesSync(join(cache, name), 1000, 2000);
    return cache;
  };
  const cache = make(['a', 'b', 'c']);
  const print = fingerprint(cache);
  // Read, and its access time changed: the same.
  readFileSync(join(cache, 'a'));
  utimesSync(join(cache, 'b'), 5000, 2000);
  assert.equal(fingerprint(cache), print);
  // Made again in another order, same contents and times: the same.
  for (const name of ['a', 'b', 'c']) rmSync(join(cache, name));
  for (const name of ['c', 'a', 'b']) writeFileSync(join(cache, name), name);
  for (const name of ['c', 'a', 'b']) utimesSync(join(cache, name), 1000, 2000);
  assert.equal(fingerprint(cache), print);
  // A modification time is not ignored.
  utimesSync(join(cache, 'c'), 1000, 3000);
  assert.notEqual(fingerprint(cache), print);
});

it('changedCaches names only the cache that changed', () => {
  const base = mkdtempSync(join(tmpdir(), 'eval-cache-'));
  const caches = ['node_modules', 'venv'].map((name) => {
    const dir = join(base, name);
    mkdirSync(join(dir, 'pkg'), { recursive: true });
    writeFileSync(join(dir, 'pkg', 'x'), '1');
    return { dir, print: fingerprint(dir) };
  });
  assert.deepEqual(changedCaches(caches), []);
  writeFileSync(join(base, 'venv', 'pkg', 'x'), '22');
  assert.deepEqual(changedCaches(caches), [join(base, 'venv')]);
});
