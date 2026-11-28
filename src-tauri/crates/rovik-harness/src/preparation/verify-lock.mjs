// Cargo removes workspace-only features when resolving this standalone helper.
// Accept that normalization, while retaining every shipped version and checksum.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function verifyLock(lock, trusted) {
  const expected = new Map(trusted.map(([name, version, source, checksum]) => [
    JSON.stringify([name, version, source]), checksum,
  ]));
  const packages = lock.split(/^\[\[package\]\]\r?$/m).slice(1);
  if (!packages.length) throw new Error('The verifier lock contains no packages.');
  const seen = new Set();
  for (const block of packages) {
    const field = (name, required) => {
      const found = [...block.matchAll(new RegExp(`^${name} = ("(?:[^"\\\\]|\\\\.)*")\\s*$`, 'gm'))];
      if (found.length > 1 || (required && !found.length))
        throw new Error(`Invalid verifier lock field: ${name}`);
      return found.length ? JSON.parse(found[0][1]) : null;
    };
    const name = field('name', true);
    const version = field('version', true);
    const key = JSON.stringify([name, version, field('source', false)]);
    if (seen.has(key) || !expected.has(key) || expected.get(key) !== field('checksum', false))
      throw new Error(`The verifier dependency differs from the shipped lock: ${name} ${version}`);
    seen.add(key);
  }
  if (![...seen].some((key) => JSON.parse(key)[0] === 'rovik-harness'))
    throw new Error('The verifier package is missing from its lock.');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  verifyLock(readFileSync('Cargo.lock', 'utf8'), JSON.parse(readFileSync('trusted-lock.json', 'utf8')));
}
