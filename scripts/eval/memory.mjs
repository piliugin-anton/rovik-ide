// Runs the real SQLite retriever, emitting a machine-readable baseline to stdout.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { memoryGate } from './memory-gates.mjs';

const run = spawnSync(
  'cargo',
  ['run', '--locked', '-p', 'verified-code-memory', '--example', 'evaluate_memory'],
  {
    cwd: fileURLToPath(new URL('../../src-tauri', import.meta.url)),
    encoding: 'utf8',
    stdio: ['inherit', 'pipe', 'inherit'],
  }
);
if (run.error) console.error(run.error.message);
if (run.stdout) process.stdout.write(run.stdout);
process.exitCode = run.status ?? 1;
if (run.status === 0 && process.argv.includes('--gate')) {
  const at = process.argv.indexOf('--agent-results');
  const tasks = at < 0 ? undefined : JSON.parse(readFileSync(process.argv[at + 1], 'utf8'));
  const gate = memoryGate(JSON.parse(run.stdout), tasks);
  console.error(JSON.stringify(gate));
  if (!gate.passed) process.exitCode = 2;
}
