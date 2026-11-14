import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

test('a failed component test releases its timers and reports the remaining tests', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'rovik-test-exit-'));
  try {
    // Load the project's compiler from the project, not this temporary test bundle.
    const { build } = createRequire(resolve('package.json'))('esbuild') as typeof import('esbuild');
    const fixture = join(directory, 'fixture.mjs');
    await build({
      stdin: {
        contents: `
          import './src/test/dom.ts';
          import { test } from 'node:test';
          import assert from 'node:assert/strict';
          test('failure leaves a component timer', () => {
            window.setInterval(() => {}, 1);
            assert.fail('intentional fixture failure');
          });
          test('the final result is retained', () => assert.ok(true));
        `,
        resolveDir: process.cwd(),
      },
      outfile: fixture,
      bundle: true,
      platform: 'node',
      format: 'esm',
      conditions: ['browser'],
      logLevel: 'error',
    });
    const env = { ...process.env };
    delete env.NODE_TEST_CONTEXT;
    const run = spawnSync(process.execPath, ['--test', fixture], {
      env,
      encoding: 'utf8',
      timeout: 15_000,
    });
    assert.equal(run.error, undefined, `the fixture did not finish: ${run.error}`);
    assert.equal(run.status, 1, run.stdout + run.stderr);
    assert.match(run.stdout, /intentional fixture failure/);
    assert.match(run.stdout, /the final result is retained/);
    assert.match(run.stdout, /tests\s+2/);
    assert.match(run.stdout, /pass\s+1/);
    assert.match(run.stdout, /fail\s+1/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
