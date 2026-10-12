// Runs TypeScript test files of a workspace with node:test, as the app's
// own `npm test` does (scripts/test.mjs): each file bundled with esbuild
// (the sources' extensionless imports; solid-js's browser build), then run.
// Prints ts-reporter.mjs's JSON lines; a file that fails to bundle counts
// as one failed test named after it.
//
//   node scripts/eval/ts-test.mjs <workspace> <file.test.ts>...
import { build } from 'esbuild';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const [root, ...files] = process.argv.slice(2);
if (!root || !files.length) {
  console.error('usage: ts-test.mjs <workspace> <file.test.ts>...');
  process.exit(2);
}
const here = dirname(fileURLToPath(import.meta.url));
const out = mkdtempSync(join(tmpdir(), 'rovik-eval-ts-'));
try {
  const sources = {};
  for (const [i, file] of files.entries()) {
    const outfile = join(out, `t${i}.mjs`);
    try {
      await build({
        entryPoints: [resolve(root, file)],
        absWorkingDir: resolve(root),
        outfile,
        bundle: true,
        platform: 'node',
        format: 'esm',
        conditions: ['browser'],
        logLevel: 'silent',
      });
      sources[outfile] = file;
    } catch (e) {
      console.log(JSON.stringify({ test: `${file} (bundle)`, status: 'fail' }));
      console.error(String(e.message ?? e));
    }
  }
  const bundles = Object.keys(sources);
  if (bundles.length) {
    const run = spawnSync(
      process.execPath,
      ['--test', `--test-reporter=${join(here, 'ts-reporter.mjs')}`, ...bundles],
      {
        stdio: ['ignore', 'inherit', 'inherit'],
        env: { ...process.env, ROVIK_EVAL_TS_FILES: JSON.stringify(sources) },
        timeout: 10 * 60 * 1000,
      }
    );
    process.exitCode = run.status ?? 1;
  }
} finally {
  rmSync(out, { recursive: true, force: true });
}
