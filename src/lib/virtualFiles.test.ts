// virtualFiles.ts tests (node:test): where a chat across folders can start
// (docs/design/workspaces-plan.md, 4.12). It works in a view of its
// folders, a virtual folder: here that takes the driver, and on Windows,
// where chats run in WSL, a Rovik there that works across folders. Each
// reason says where to get what's missing.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { acrossFoldersBlocked, type DriverStatus } from './virtualFiles';
import type { HostSummary } from './cloud';

const driver = (name: string, installed: boolean): DriverStatus => ({
  driver: name,
  installed,
  version: installed ? '3.16' : null,
  detail: null,
  canInstall: true,
});

test('a second folder needs the driver here, and says how to get it', () => {
  assert.equal(acrossFoldersBlocked(false, driver('FUSE', true), undefined), null);
  assert.equal(
    acrossFoldersBlocked(false, driver('FUSE', false), undefined),
    'Needs FUSE: Settings → Virtual files'
  );
  assert.equal(
    acrossFoldersBlocked(false, driver('macFUSE', false), undefined),
    'Needs macFUSE: Settings → Virtual files'
  );
  assert.equal(
    acrossFoldersBlocked(false, null, undefined),
    'Needs the virtual files driver: Settings → Virtual files',
    'not known yet'
  );
});

test('on Windows a second folder needs Rovik in WSL connected and working across folders', () => {
  const wsl = (state: HostSummary['state'], features?: string[]): HostSummary => ({
    id: 'wsl',
    name: 'WSL',
    deployed: true,
    wsl: true,
    state,
    features,
  });
  const windowsDriver = driver('WSL', false);
  assert.equal(acrossFoldersBlocked(true, windowsDriver, wsl('ready', ['folders'])), null);
  for (const host of [undefined, wsl('offline', ['folders']), wsl('connecting', ['folders'])]) {
    assert.equal(
      acrossFoldersBlocked(true, windowsDriver, host),
      'Needs WSL connected: Settings → This computer → WSL'
    );
  }
  assert.equal(
    acrossFoldersBlocked(true, windowsDriver, wsl('ready', ['mirror'])),
    'Rovik in WSL doesn’t work across folders yet: Settings → This computer → WSL'
  );
});
