// proc.mjs tests. Run with: npm run test:eval
import { it } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { run, startServer } from './proc.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};
const until = async (ok, ms = 5000) => {
  const end = Date.now() + ms;
  while (!ok() && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
  return ok();
};

it('a killed evaluation takes its runs, and what they started, with it', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-proc-'));
  const pidFile = join(dir, 'pid');
  const driver = join(dir, 'driver.mjs');
  writeFileSync(
    driver,
    `import { run } from ${JSON.stringify(join(HERE, 'proc.mjs'))};
     await run('sh', ['-c', 'sleep 60 & echo $! > ${pidFile}; wait']);`
  );
  const child = spawn(process.execPath, [driver], { stdio: 'ignore' });
  assert.ok(await until(() => existsSync(pidFile) && readFileSync(pidFile, 'utf8').trim()));
  const grandchild = Number(readFileSync(pidFile, 'utf8').trim());
  assert.ok(alive(grandchild));
  const ended = new Promise((r) => child.once('exit', (code, signal) => r({ code, signal })));
  child.kill('SIGTERM');
  assert.deepEqual(await ended, { code: null, signal: 'SIGTERM' }, 're-raised');
  assert.ok(await until(() => !alive(grandchild)), 'its run was killed');
});

it('a command that cannot start ends its run, not the evaluation', async () => {
  const r = await run('/no/such/command', []);
  assert.equal(r.status, null);
  assert.match(r.out, /could not start/);
});

it('a helper that exits before printing its URL is an error', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-proc-'));
  const script = join(dir, 'dies.mjs');
  writeFileSync(script, 'process.exit(3);');
  await assert.rejects(startServer(script, []), /exited before it started \(3\)/);
});
