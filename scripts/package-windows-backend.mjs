#!/usr/bin/env node
// Build the same workspace version as the UI and publish its installer resource.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, renameSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({
  options: {
    profile: { type: 'string', default: 'release' },
    'if-windows': { type: 'boolean', default: false },
  },
});
if (process.platform !== 'win32') {
  if (values['if-windows']) process.exit(0);
  throw new Error('Package the Windows backend on Windows.');
}
if (!['dev', 'release'].includes(values.profile)) throw new Error('Use --profile dev or release.');
const root = fileURLToPath(new URL('../', import.meta.url));
const built = spawnSync(
  'cargo',
  [
    'build',
    '--locked',
    '--manifest-path',
    join(root, 'src-tauri/Cargo.toml'),
    '-p',
    'rovik-client',
    '--bin',
    'rovik-desktopd',
    '--profile',
    values.profile,
    '--message-format=json-render-diagnostics',
  ],
  {
    cwd: root,
    encoding: 'utf8',
    stdio: ['inherit', 'pipe', 'inherit'],
    maxBuffer: 64 * 1024 * 1024,
  }
);
if (built.error) throw built.error;
if (built.status !== 0) process.exit(built.status ?? 1);
const artifacts = built.stdout
  .split('\n')
  .filter(Boolean)
  .map((line) => JSON.parse(line))
  .filter(
    (message) =>
      message.reason === 'compiler-artifact' &&
      message.target.name === 'rovik-desktopd' &&
      message.executable
  );
if (artifacts.length !== 1)
  throw new Error('Cargo did not identify exactly one backend executable.');
const destination = join(root, 'src-tauri/binaries/rovik-desktopd.exe');
mkdirSync(dirname(destination), { recursive: true });
const temporary = `${destination}.${process.pid}.tmp`;
try {
  copyFileSync(artifacts[0].executable, temporary);
  renameSync(temporary, destination);
} finally {
  rmSync(temporary, { force: true });
}
console.log(`Packaged backend: ${destination}`);
