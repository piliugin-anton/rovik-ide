import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./ci-install-packages.sh', import.meta.url));

for (const { failUpdate, fallback } of [
  { failUpdate: false, fallback: false },
  { failUpdate: true, fallback: false },
  { failUpdate: false, fallback: true },
]) {
  test(`Ubuntu packages ${fallback ? 'fall back to a signed mirror' : 'use official sources'} and ${failUpdate ? 'stop on an index failure' : 'retain those sources for installation'}`, () => {
    const dir = mkdtempSync(join(tmpdir(), 'rovik-apt-test-'));
    const log = join(dir, 'calls.jsonl');
    try {
      writeFileSync(
        join(dir, 'apt-get'),
        `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const source = args.find(arg => arg.startsWith('Dir::Etc::sourcelist=')).split('=')[1];
const parts = args.find(arg => arg.startsWith('Dir::Etc::sourceparts=')).split('=')[1];
fs.appendFileSync(process.env.APT_TEST_LOG, JSON.stringify({args, source, parts: fs.readdirSync(parts), text: fs.readFileSync(source, 'utf8')}) + '\\n');
if (args.includes('update') && process.env.APT_TEST_FAIL === '1') process.exit(100);
`,
        { mode: 0o755 }
      );
      writeFileSync(
        join(dir, 'curl'),
        `#!/usr/bin/env node
process.exit(process.env.APT_TEST_FALLBACK === '1' && process.argv.at(-1).includes('.ubuntu.com/') ? 6 : 0);
`,
        { mode: 0o755 }
      );
      const result = spawnSync('bash', [script, 'noble', 'bubblewrap', 'build-essential'], {
        encoding: 'utf8',
        env: {
          ...process.env,
          PATH: `${dir}:${process.env.PATH}`,
          APT_TEST_LOG: log,
          APT_TEST_FAIL: failUpdate ? '1' : '0',
          APT_TEST_FALLBACK: fallback ? '1' : '0',
        },
      });
      assert.equal(result.status, failUpdate ? 100 : 0, result.stderr);
      const calls = readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
      assert.equal(calls.length, failUpdate ? 1 : 2);
      for (const call of calls) {
        if (fallback) {
          assert.match(call.text, /https:\/\/mirrors\.edge\.kernel\.org\/ubuntu noble-updates/);
          assert.match(call.text, /https:\/\/mirrors\.edge\.kernel\.org\/ubuntu noble-security/);
        } else {
          assert.match(call.text, /https:\/\/archive\.ubuntu\.com\/ubuntu noble-updates/);
          assert.match(call.text, /https:\/\/security\.ubuntu\.com\/ubuntu noble-security/);
        }
        assert.match(call.text, /signed-by=\/usr\/share\/keyrings\/ubuntu-archive-keyring.gpg/);
        assert.doesNotMatch(call.text, /azure|mirror\+|trusted=yes/);
        assert.deepEqual(call.parts, [], 'runner-provided third-party sources are excluded');
        assert.ok(call.args.includes('Acquire::Retries=3'));
        assert.equal(existsSync(call.source), false, 'temporary sources are cleaned up');
      }
      assert.ok(calls[0].args.includes('APT::Update::Error-Mode=any'));
      if (!failUpdate) {
        assert.deepEqual(calls[1].args.slice(-4), [
          'install',
          '-y',
          'bubblewrap',
          'build-essential',
        ]);
        assert.equal(calls[0].source, calls[1].source);
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
}
