// Child processes of the evaluation (eval.mjs). Every one started here is
// tracked, so Ctrl+C or a kill of the evaluation takes its runs (and what
// they started: each run is its own process group) down with it (P6.1).
import { spawn } from 'node:child_process';
import { appendFileSync } from 'node:fs';

/** Children still running: detached ones by process group. */
const live = new Set();

function killAll() {
  for (const { child, group } of live) {
    try {
      if (group) process.kill(-child.pid, 'SIGKILL');
      else child.kill('SIGKILL');
    } catch {
      // gone already
    }
  }
  live.clear();
}

let installed = false;
function installHandlers() {
  if (installed) return;
  installed = true;
  process.on('exit', killAll);
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.once(signal, () => {
      killAll();
      // Re-raised with the default action: the evaluation ends by it.
      process.kill(process.pid, signal);
    });
  }
}

function track(child, group) {
  installHandlers();
  const entry = { child, group };
  live.add(entry);
  child.once('close', () => live.delete(entry));
  child.once('error', () => live.delete(entry));
}

/** Runs a command to its end without blocking the event loop (for long runs). */
export function run(cmd, args, { env = {}, cwd, timeoutMs, logFile } = {}) {
  return new Promise((done) => {
    const child = spawn(cmd, args, {
      cwd,
      env: { ...process.env, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: true,
    });
    track(child, true);
    let out = '';
    let finished = false;
    const finish = (r) => {
      if (finished) return;
      finished = true;
      if (timer) clearTimeout(timer);
      done(r);
    };
    const take = (c) => {
      out += c;
      if (logFile) appendFileSync(logFile, c);
    };
    child.stdout.on('data', take);
    child.stderr.on('data', take);
    let timedOut = false;
    const timer = timeoutMs
      ? setTimeout(() => {
          timedOut = true;
          try {
            process.kill(-child.pid, 'SIGKILL');
          } catch {
            // gone already
          }
        }, timeoutMs)
      : null;
    // A command that can't start (missing binary) ends the run, not the evaluation.
    child.on('error', (e) =>
      finish({ status: null, out: `${out}\ncould not start ${cmd}: ${e.message}`, timedOut })
    );
    child.on('close', (status) => finish({ status, out, timedOut }));
  });
}

/**
 * A helper server that prints its URL on its first line (fake-model.mjs).
 * Rejects if it fails to start or exits before printing.
 */
export function startServer(script, args) {
  return new Promise((done, fail) => {
    const child = spawn(process.execPath, [script, ...args], {
      stdio: ['ignore', 'pipe', 'inherit'],
    });
    track(child, false);
    let started = false;
    child.stdout.once('data', (c) => {
      started = true;
      done({ url: String(c).trim(), stop: () => child.kill() });
    });
    child.once('error', fail);
    child.once('exit', (code, signal) => {
      if (!started) fail(new Error(`${script} exited before it started (${code ?? signal})`));
    });
  });
}
