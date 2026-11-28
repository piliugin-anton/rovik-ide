import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const result = spawnSync(
  'cargo',
  [
    'run',
    '--quiet',
    '--manifest-path',
    'src-tauri/Cargo.toml',
    '-p',
    'rovik-core',
    '--example',
    'generate_preparation_types',
  ],
  { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }
);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
const directory = new URL('../src/lib/generated/', import.meta.url);
mkdirSync(directory, { recursive: true });
writeFileSync(new URL('preparation.ts', directory), result.stdout);
