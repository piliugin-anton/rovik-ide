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
const PROC = JSON.stringify(join(HERE, 'proc.mjs'));

/**
 * Whether a process still runs. A killed process whose parent is gone
 * stays a zombie until init reaps it, which can take seconds under load
 * (and `kill -0` succeeds on a zombie): on Linux a zombie counts as ended.
 */
const alive = (pid) => {
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, 'utf8');
    // "pid (comm) S ...": the state follows the last ')'.
    return !/^[ZX]$/.test(stat.slice(stat.lastIndexOf(')') + 2).split(' ')[0]);
  } catch {
    return process.platform !== 'linux';
  }
};
const until = async (ok, ms = 15000) => {
  const end = Date.now() + ms;
  while (!ok() && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
  return ok();
};
const readPid = async (file) => {
  assert.ok(
    await until(() => existsSync(file) && readFileSync(file, 'utf8').trim()),
    `${file} written`
  );
  return Number(readFileSync(file, 'utf8').trim());
};

/** A node script in a fresh folder; `$DIR` in it is the folder. */
const scratch = (name, body) => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-proc-'));
  const file = join(dir, name);
  writeFileSync(file, body.replaceAll('$DIR', dir));
  return { dir, file };
};

/** Starts a driver (an evaluation stand-in); its exit and what it printed. */
const drive = (file) => {
  const child = spawn(process.execPath, [file], { stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  child.stdout.on('data', (c) => (out += c));
  child.stderr.on('data', (c) => (out += c));
  const ended = new Promise((r) => child.once('close', (code, signal) => r({ code, signal, out })));
  return { child, ended };
};

/** A run whose shell starts a background `sleep` (same process group) and waits. */
const SLEEPER = (pidFile) => `['-c', 'sleep 60 & echo $! > ${pidFile}; wait']`;

for (const signal of ['SIGTERM', 'SIGINT']) {
  it(`${signal} during a run kills its process group and the evaluation ends by ${signal}`, async () => {
    const { dir, file } = scratch(
      'driver.mjs',
      `import { run } from ${PROC};
       await run('sh', ${SLEEPER('$DIR/pid')});`
    );
    const { child, ended } = drive(file);
    const grandchild = await readPid(join(dir, 'pid'));
    assert.ok(alive(grandchild));
    child.kill(signal);
    const end = await ended;
    assert.deepEqual([end.code, end.signal], [null, signal], `re-raised: ${end.out}`);
    assert.ok(await until(() => !alive(grandchild)), 'its run was killed');
  });
}

it('SIGTERM between runs re-raises, with nothing to kill and nothing printed', async () => {
  const { dir, file } = scratch(
    'driver.mjs',
    `import { writeFileSync } from 'node:fs';
     import { run } from ${PROC};
     const r = await run('sh', ['-c', 'echo done']);
     if (r.status !== 0) process.exit(9);
     writeFileSync('$DIR/between', String(process.pid));
     setTimeout(() => {}, 60000);`
  );
  const { child, ended } = drive(file);
  await readPid(join(dir, 'between'));
  child.kill('SIGTERM');
  const end = await ended;
  assert.deepEqual([end.code, end.signal], [null, 'SIGTERM']);
  assert.equal(end.out, '', 'no ESRCH or other noise');
});

it('SIGINT while a helper is starting kills the helper', async () => {
  const helper = scratch(
    'slow.mjs',
    `import { writeFileSync } from 'node:fs';
     writeFileSync('$DIR/pid', String(process.pid));
     setTimeout(() => {}, 60000); // never prints its URL`
  );
  const { file } = scratch(
    'driver.mjs',
    `import { startServer } from ${PROC};
     await startServer(${JSON.stringify(helper.file)}, []);
     process.exit(7); // never reached`
  );
  const { child, ended } = drive(file);
  const pid = await readPid(join(helper.dir, 'pid'));
  assert.ok(alive(pid));
  child.kill('SIGINT');
  const end = await ended;
  assert.deepEqual([end.code, end.signal], [null, 'SIGINT']);
  assert.ok(await until(() => !alive(pid)), 'the helper was killed');
});

it('a normal exit of the evaluation kills a run still going', async () => {
  const { dir, file } = scratch(
    'driver.mjs',
    `import { existsSync, readFileSync } from 'node:fs';
     import { run } from ${PROC};
     run('sh', ${SLEEPER('$DIR/pid')});
     const written = () => existsSync('$DIR/pid') && readFileSync('$DIR/pid', 'utf8').trim();
     while (!written()) await new Promise((r) => setTimeout(r, 25));
     process.exit(0);`
  );
  const { ended } = drive(file);
  const grandchild = await readPid(join(dir, 'pid'));
  const end = await ended;
  assert.deepEqual([end.code, end.signal], [0, null], end.out);
  assert.ok(await until(() => !alive(grandchild)), 'the exit handler killed the run');
});

it('a run that ends by a signal has no status and did not time out', async () => {
  const r = await run('sh', ['-c', 'kill -TERM $$']);
  assert.equal(r.status, null);
  assert.equal(r.timedOut, false);
});

it('a timeout kills the run’s whole process group', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'eval-proc-'));
  const pidFile = join(dir, 'pid');
  const started = Date.now();
  // The background sleep keeps the output pipe open: the run ends only
  // when it is killed too.
  const r = await run('sh', ['-c', `sleep 60 & echo $! > ${pidFile}; wait`], { timeoutMs: 500 });
  assert.equal(r.timedOut, true);
  assert.equal(r.status, null);
  assert.ok(Date.now() - started < 30000);
  const grandchild = Number(readFileSync(pidFile, 'utf8').trim());
  assert.ok(await until(() => !alive(grandchild)), 'the background process was killed');
});

it('a command that cannot start ends its run, not the evaluation', async () => {
  const r = await run('/no/such/command', []);
  assert.equal(r.status, null);
  assert.match(r.out, /could not start/);
});

it('a helper that exits before printing its URL is an error', async () => {
  const { file } = scratch('dies.mjs', 'process.exit(3);');
  await assert.rejects(startServer(file, []), /exited before it started \(3\)/);
});

it('a helper’s URL is its first whole line, and stop() ends it', async () => {
  // The URL comes in two chunks, then more lines follow in the same write.
  const { dir, file } = scratch(
    'server.mjs',
    `import { writeFileSync } from 'node:fs';
     writeFileSync('$DIR/pid', String(process.pid));
     process.stdout.write('http://127.0.0.1:');
     setTimeout(() => process.stdout.write('4321/v1\\nlistening\\nmore\\n'), 100);
     setTimeout(() => {}, 60000);`
  );
  const server = await startServer(file, []);
  const pid = await readPid(join(dir, 'pid'));
  try {
    assert.equal(server.url, 'http://127.0.0.1:4321/v1');
    assert.ok(alive(pid));
  } finally {
    server.stop();
  }
  assert.ok(await until(() => !alive(pid)), 'stopped');
});
