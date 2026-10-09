// Runs the frontend unit tests (src/**/*.test.ts) with node:test.
//
// Each test file is bundled with esbuild first: the sources use
// extensionless imports, which node can't resolve on its own, and the
// browser build of solid-js is used (the `browser` condition), so stores
// behave as they do in the app rather than as the server build does.
import { build } from 'esbuild';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';

function testFiles(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return testFiles(path);
    return name.endsWith('.test.ts') ? [path] : [];
  });
}

const files = testFiles('src');
const out = mkdtempSync(join(tmpdir(), 'rovik-tests-'));
try {
  const bundles = [];
  for (const file of files) {
    const outfile = join(
      out,
      relative('src', file).replace(/[\\/]/g, '__').replace(/\.ts$/, '.mjs')
    );
    await build({
      entryPoints: [file],
      outfile,
      bundle: true,
      platform: 'node',
      format: 'esm',
      conditions: ['browser'],
      logLevel: 'error',
    });
    bundles.push(outfile);
  }
  const run = spawnSync(process.execPath, ['--test', ...bundles], { stdio: 'inherit' });
  process.exitCode = run.status ?? 1;
} finally {
  rmSync(out, { recursive: true, force: true });
}
