import '../test/dom';
import { flush, installTauri } from '../test/chatPanel';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  needsInstall,
  progressNote,
  startDocker,
  statusNote,
  type DockerStatus,
} from './dockerStartup';
import { notifications } from './notifications';

const missing: DockerStatus = {
  installed: false,
  ready: false,
  path: null,
  canInstall: true,
  message: 'Docker is not installed.',
};

test('only a missing supported local Docker installation starts an install', () => {
  assert.equal(needsInstall(missing), true);
  assert.equal(needsInstall({ ...missing, installed: true }), false);
  assert.equal(needsInstall({ ...missing, canInstall: false }), false);
  assert.equal(needsInstall({ ...missing, ready: true }), false);
});

test('an installed but stopped daemon reports setup instead of success', () => {
  const note = statusNote({ ...missing, installed: true, message: 'Sign out and back in.' });
  assert.equal(note.kind, 'warning');
  assert.equal(note.title, 'Docker needs setup');
  assert.equal(note.message, 'Sign out and back in.');
  assert.equal(note.timeout, undefined);
  assert.equal(
    statusNote({ ...missing, installed: true, ready: true, message: null }).kind,
    'success'
  );
});

test('download bytes and native installation steps have distinct progress', () => {
  assert.deepEqual(
    progressNote({ phase: 'downloading', downloaded: 1024, total: 2048, message: 'Download' }),
    { progress: 0.5, message: '1 KB of 2 KB' }
  );
  assert.deepEqual(
    progressNote({
      phase: 'installing',
      downloaded: 2048,
      total: 2048,
      message: 'Complete system approval.',
    }),
    { progress: 'busy', message: 'Complete system approval.' }
  );
  assert.equal(
    progressNote({ phase: 'downloading', downloaded: 0, total: null, message: 'Waiting' }).progress,
    'busy'
  );
});

test('startup shares an attempt, retries failure, and checks an installed daemon without reinstalling', async () => {
  installTauri([]);
  let installs = 0;
  let installed = false;
  let ready = false;
  const note = () => notifications().find((entry) => entry.id === 'docker-install')!;
  (window as any).__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
    if (command === 'docker_status') return { ...missing, installed, ready };
    assert.equal(command, 'docker_install');
    installs++;
    args.onProgress.onmessage({
      phase: 'downloading',
      downloaded: 1,
      total: 2,
      message: 'Download',
    });
    assert.equal(note().progress, 0.5);
    if (installs === 1) throw new Error('System approval declined');
    installed = true;
    return { ...missing, installed, ready, message: 'Finish Docker setup.' };
  };
  await Promise.all([startDocker(), startDocker()]);
  assert.equal(installs, 1);
  assert.equal(note().kind, 'error');
  assert.match(note().message!, /System approval declined/);
  note().actions![0].run();
  await flush();
  assert.equal(installs, 2);
  assert.equal(note().kind, 'warning');
  assert.equal(note().title, 'Docker needs setup');
  ready = true;
  note().actions![0].run();
  await flush();
  assert.equal(installs, 2);
  assert.equal(note().kind, 'success');
});
