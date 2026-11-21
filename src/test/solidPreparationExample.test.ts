import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

test('the protected Solid oracle renders candidate TSX and detects a broken label', async () => {
  const project = await mkdtemp(join(tmpdir(), 'solid-check-'));
  const oracle = resolve('src-tauri/crates/rovik-core/src/harness/solid-component-example.mjs');
  const env = { ...process.env };
  delete env.NODE_TEST_CONTEXT;
  const run = () =>
    promisify(execFile)(process.execPath, ['--test', oracle], { cwd: project, env });
  try {
    await mkdir(join(project, 'src'));
    await symlink(resolve('node_modules'), join(project, 'node_modules'), 'junction');
    await writeFile(join(project, 'package.json'), '{"type":"module"}');
    const component = join(project, 'src', 'Badge.tsx');
    await writeFile(component, 'export default (props: {label: string}) => <b>{props.label}</b>');
    assert.match((await run()).stdout, /the badge renders its supplied label/);
    await writeFile(component, 'export default () => <b>Broken</b>');
    await assert.rejects(run(), (error: Error & { stdout?: string }) => {
      assert.match(error.stdout ?? '', /Broken/);
      assert.match(error.stdout ?? '', /Ready/);
      return true;
    });
  } finally {
    await rm(project, { recursive: true, force: true });
  }
});
