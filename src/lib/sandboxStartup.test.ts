// sandboxStartup.ts tests (node:test): what the app does about the command
// sandbox at start.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startupStep } from './sandboxStartup';
import type { SandboxStatus } from './sandbox';

const blocked: SandboxStatus = {
  required: true,
  program: 'bubblewrap',
  working: false,
  detail: 'bubblewrap is installed, but AppArmor stops it from making a sandbox here.',
  canInstall: true,
  fallback: null,
};

/** bubblewrap blocked, the commands confined by Landlock meanwhile. */
const lighter: SandboxStatus = { ...blocked, working: true, fallback: 'Landlock' };

test('a sandbox the app can fix is set up by itself, with nothing to click first', () => {
  assert.equal(startupStep(blocked, {}), 'setUp');
});

test('one it can’t fix is only told of; one that works, or isn’t needed, is left alone', () => {
  assert.equal(startupStep({ ...blocked, canInstall: false }, {}), 'tell');
  assert.equal(startupStep({ ...blocked, working: true }, {}), 'none');
  assert.equal(startupStep({ ...blocked, required: false }, {}), 'none');
});

test('“Don’t ask again” stops it, setting up included', () => {
  assert.equal(startupStep(blocked, { quiet: true }), 'none');
  assert.equal(startupStep({ ...blocked, canInstall: false }, { quiet: true }), 'none');
});

test('under the fallback the full sandbox is set up by itself once, then left alone', () => {
  assert.equal(startupStep(lighter, {}), 'setUp');
  assert.equal(startupStep(lighter, { triedFull: true }), 'none');
  assert.equal(startupStep({ ...lighter, canInstall: false }, {}), 'none');
  assert.equal(startupStep(lighter, { quiet: true }), 'none');
});
