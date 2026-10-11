// The evaluation's shared caches (eval.mjs), kept out of the agent's reach
// (P6.5): a run gets its own copy of node_modules, the Python environment
// is read-only, and a cache that changed during a run is reported.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstatSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Copies a folder: a copy-on-write clone where the file system can
 * (Linux's reflink, macOS's clonefile), else a plain copy.
 */
export function copyTree(from, to) {
  const tries =
    process.platform === 'darwin'
      ? [
          ['-cR', from, to],
          ['-R', from, to],
        ]
      : [
          ['-a', '--reflink=auto', from, to],
          ['-R', from, to],
        ];
  for (const args of tries) {
    if (spawnSync('cp', args, { stdio: 'ignore' }).status === 0) return;
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

/** Makes a cache read-only for the runs (its owner can still delete it). */
export function protect(dir) {
  spawnSync('chmod', ['-R', 'a-w', dir], { stdio: 'ignore' });
}
