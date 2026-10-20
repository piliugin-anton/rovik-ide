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
};

test('a sandbox the app can fix is set up by itself, with nothing to click first', () => {
  assert.equal(startupStep(blocked, false), 'setUp');
});

test('one it can’t fix is only told of; one that works, or isn’t needed, is left alone', () => {
  assert.equal(startupStep({ ...blocked, canInstall: false }, false), 'tell');
  assert.equal(startupStep({ ...blocked, working: true }, false), 'none');
  assert.equal(startupStep({ ...blocked, required: false }, false), 'none');
});

test('“Don’t ask again” stops it, setting up included', () => {
  assert.equal(startupStep(blocked, true), 'none');
  assert.equal(startupStep({ ...blocked, canInstall: false }, true), 'none');
});
