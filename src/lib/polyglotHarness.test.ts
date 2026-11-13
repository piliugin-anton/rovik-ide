import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  displayedHarnessStatus,
  eligibleHarnessChat,
  harnessRequest,
  verificationInputProblem,
  verifyChanges,
  type HarnessInspection,
  type HarnessRun,
} from './polyglotHarness';

const calls: { command: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    return run;
  },
};
const run: HarnessRun = {
  runId: 'run-1',
  state: 'ACCEPTED',
  status: 'verified',
  snapshotDigest: 'sha256:reviewed',
  pendingApprovals: [],
  canResume: false,
  canRecover: false,
  canCancel: false,
};
const inspection: HarnessInspection = {
  projectId: 'project',
  snapshotDigest: 'sha256:reviewed',
  inventory: { units: [], unresolved: [] },
  untracked: [],
  issues: [],
  setups: [],
  runs: [],
  host: { supported: true, platform: 'linux', engines: [] },
  configuration: {
    digest: 'sha256:config',
    setupId: 'setup',
    label: 'API contract',
    engine: { kind: 'docker', executable: '/usr/bin/docker' },
  },
};

test('verification refuses unsaved or unknown editor inputs before sending any request', async () => {
  calls.length = 0;
  for (const files of [undefined, ['src/api.ts']]) {
    await assert.rejects(
      verifyChanges({ root: '/project' }, inspection, [], files),
      /Editor state|unsaved changes/
    );
  }
  assert.equal(calls.length, 0);
  assert.equal(verificationInputProblem([]), null);
});

test('verification binds the inspected snapshot and explicit new files to the selected host', async () => {
  calls.length = 0;
  await verifyChanges(
    { root: '/project', dialogId: 'remote-chat' },
    inspection,
    ['src/new.ts'],
    []
  );
  assert.deepEqual(calls, [
    {
      command: 'polyglot_harness',
      args: {
        root: '/project',
        dialogId: 'remote-chat',
        executionHost: 'chat',
        request: {
          action: 'verify',
          untracked: ['src/new.ts'],
          expectedSnapshotDigest: 'sha256:reviewed',
          savedBuffers: true,
        },
      },
    },
  ]);
  await harnessRequest({ root: '/project' }, { action: 'inspect', untracked: [] });
  assert.equal(calls[1].args.executionHost, 'local');
  assert.equal(calls[1].args.dialogId, null);
});

test('unsupported hosts and missing reviewed configuration never start verification', async () => {
  calls.length = 0;
  await assert.rejects(
    verifyChanges({ root: '/project' }, { ...inspection, configuration: null }, [], []),
    /review the project setup/
  );
  await assert.rejects(
    verifyChanges(
      { root: '/project' },
      { ...inspection, host: { ...inspection.host, supported: false } },
      [],
      []
    ),
    /supported execution host/
  );
  assert.equal(calls.length, 0);
});

test('a verified snapshot becomes stale for edited inputs while failed results stay failed', () => {
  assert.equal(displayedHarnessStatus(run, [], 'sha256:reviewed'), 'verified');
  assert.equal(displayedHarnessStatus(run, ['src/api.ts'], 'sha256:reviewed'), 'stale');
  assert.equal(displayedHarnessStatus(run, [], 'sha256:changed'), 'stale');
  assert.equal(displayedHarnessStatus(run, undefined), 'stale');
  assert.equal(
    displayedHarnessStatus({ ...run, status: 'failed' }, [], 'sha256:reviewed'),
    'failed'
  );
});

test('only a known host chat belonging to the open project can be offered for verification', () => {
  const chat = {
    id: 'remote-chat',
    title: 'API repair',
    createdAt: 1,
    updatedAt: 1,
    messageCount: 0,
    place: { kind: 'host' as const, host: 'host-1' },
  };
  const hosts = [{ id: 'host-1', name: 'Linux builder' }];
  assert.deepEqual(eligibleHarnessChat('project-1', 'project-1', chat, hosts), {
    dialogId: 'remote-chat',
    projectId: 'project-1',
    hostId: 'host-1',
    label: 'Linux builder — API repair',
  });
  assert.equal(eligibleHarnessChat('project-2', 'project-1', chat, hosts), undefined);
  assert.equal(eligibleHarnessChat(undefined, 'project-1', chat, hosts), undefined);
  assert.equal(
    eligibleHarnessChat(
      'project-1',
      'project-1',
      { ...chat, place: { kind: 'moving', host: 'host-1' } },
      hosts
    ),
    undefined
  );
  assert.equal(eligibleHarnessChat('project-1', 'project-1', chat, []), undefined);
});
