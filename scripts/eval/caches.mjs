// The evaluation's shared caches (eval.mjs), kept out of the agent's reach
// (P6.5): a run gets its own copy of node_modules, the Python environment
// is read-only, and a cache that changed during a run is reported.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, lstatSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';

let warnedFullCopy = false;

/**
 * Copies a folder: a copy-on-write clone where the file system can
 * (Linux's reflink, macOS's clonefile), else a plain copy, which says so
 * once (every run then copies the whole folder).
 */
export function copyTree(from, to) {
  const tries =
    process.platform === 'darwin'
      ? [
          { args: ['-cR', from, to], clone: true },
          { args: ['-R', from, to], clone: false },
        ]
      : [
          { args: ['-a', '--reflink=always', from, to], clone: true },
          { args: ['-a', from, to], clone: false },
          { args: ['-R', from, to], clone: false },
        ];
  for (const { args, clone } of tries) {
    // A failed try may leave part of a copy: `cp` would then copy into it.
    rmSync(to, { recursive: true, force: true });
    if (spawnSync('cp', args, { stdio: 'ignore' }).status !== 0) continue;
    if (!clone && !warnedFullCopy) {
      warnedFullCopy = true;
      console.warn(
        `note: this file system can't clone ${from}: each run copies it in full (slower, more disk).`
      );
    }
    return;
  }
  throw new Error(`couldn't copy ${from} to ${to}`);
}

/** Every file's path, size and modification time, hashed: changes if any change. */
export function fingerprint(dir) {
  const hash = createHash('sha256');
  const walk = (d) => {
    let names;
    try {
      names = readdirSync(d).sort();
    } catch {
      return;
    }
    for (const name of names) {
      const p = join(d, name);
      const s = lstatSync(p);
      hash.update(`${p}\0${s.size}\0${s.mtimeMs}\0${s.mode}\n`);
      if (s.isDirectory()) walk(p);
    }
  };
  walk(dir);
  return hash.digest('hex');
}

/**
 * The caches (`{dir, print}`, `print` its fingerprint before the run)
 * whose contents changed since: later runs would see the changes.
 */
export function changedCaches(caches) {
  return caches.filter((c) => fingerprint(c.dir) !== c.print).map((c) => c.dir);
}

/**
 * Makes a cache's files read-only for the runs. Only files: its folders
 * keep (or get back) their owner's write bit, so the owner can still
 * delete the cache (`rm -rf`) without a chmod first.
 */
export function protect(dir) {
  const walk = (p) => {
    let s;
    try {
      s = lstatSync(p);
    } catch {
      return;
    }
    if (s.isSymbolicLink()) return; // chmod would follow it
    if (s.isDirectory()) {
      if (!(s.mode & 0o200)) chmodSync(p, (s.mode & 0o7777) | 0o200);
      for (const name of readdirSync(p)) walk(join(p, name));
    } else if (s.mode & 0o222) {
      chmodSync(p, s.mode & 0o7777 & ~0o222);
    }
  };
  walk(dir);
}
