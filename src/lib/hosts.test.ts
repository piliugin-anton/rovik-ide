// hosts.ts tests (node:test): what Settings → Cloud hosts says of a host
// and of its connection test, and what a host's form saves of its secrets.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  addressOf,
  asDeployFailure,
  emptyHost,
  hostOf,
  hostStatus,
  secretsToSave,
  setupVerdict,
  testVerdict,
  type HostRow,
  type Step,
} from './hosts';
import { formatBytes } from './textUtils';

const deployed = { state: 'deployed', version: '0.2.0' } as const;

test('a host’s status: deployed or not, then its connection', () => {
  assert.deepEqual(hostStatus({ deploy: { state: 'notDeployed' }, state: 'ready' }), {
    label: 'Not deployed',
    tone: 'muted',
  });
  assert.equal(hostStatus({ deploy: deployed, state: 'ready' }).label, 'Connected');
  assert.equal(hostStatus({ deploy: deployed, state: 'offline' }).tone, 'error');
  assert.equal(
    hostStatus({ deploy: deployed, state: 'version-mismatch' }).label,
    'Needs a newer app'
  );
  assert.deepEqual(hostStatus({ deploy: deployed, state: 'refused' }), {
    label: 'Refused',
    tone: 'error',
  });
  assert.equal(hostStatus({ deploy: deployed }).label, 'Connecting…');
});

const step = (check: Step['check'], outcome: Step['outcome']): Step => ({
  check,
  outcome,
  message: `${check} ${outcome}`,
});

test('the test’s verdict: running, nothing yet, a key to check, the first failure, warnings', () => {
  assert.equal(testVerdict([], true)?.label, 'Testing…');
  assert.equal(testVerdict([], false), null);
  const key = [step('address', 'passed'), step('host-key', 'confirm'), step('log-in', 'skipped')];
  assert.equal(testVerdict(key, false)?.label, 'Check the server’s key');
  const build = [step('install', 'passed'), step('build', 'confirm')];
  assert.equal(testVerdict(build, false)?.label, 'Check Rovik’s build there');
  const failed = [step('address', 'passed'), step('log-in', 'failed'), step('disk', 'failed')];
  assert.deepEqual(testVerdict(failed, false), { label: 'Failed: Log-in', tone: 'error' });
  const warned = [step('address', 'passed'), step('chrome', 'warning')];
  assert.equal(testVerdict(warned, false)?.label, 'Works, with warnings');
  assert.deepEqual(testVerdict([step('address', 'passed')], false), {
    label: 'Everything works',
    tone: 'ok',
  });
});

test('a rejection is a deploy failure: the object Rust sends, or its text', () => {
  const needsRoot = { kind: 'needsRoot', command: 'sudo sh install', message: 'Needs root' };
  assert.deepEqual(asDeployFailure(needsRoot), needsRoot);
  assert.deepEqual(asDeployFailure('No such host'), { kind: 'failed', message: 'No such host' });
  assert.deepEqual(asDeployFailure(new Error('gone')), { kind: 'failed', message: 'Error: gone' });
});

test('sizes and addresses for people', () => {
  // A server's folders run to gigabytes.
  assert.equal(formatBytes(512), '512 bytes');
  assert.equal(formatBytes(200 * 1024 ** 2), '200 MB');
  assert.equal(formatBytes(3 * 1024 ** 3), '3 GB');
  assert.equal(formatBytes(3.4 * 1024 ** 3), '3.4 GB');
  assert.equal(formatBytes(2048 * 1024 ** 3), '2 TB');
  assert.equal(
    addressOf({ user: 'dev', host: 'build.example.com', port: 22 }),
    'dev@build.example.com'
  );
  assert.equal(addressOf({ user: 'dev', host: '203.0.113.7', port: 2222 }), 'dev@203.0.113.7:2222');
});

test('a row’s host leaves out what the list adds', () => {
  const row: HostRow = {
    ...emptyHost(),
    id: 'h1',
    name: 'Build',
    state: 'ready',
    chats: 2,
    passwordSaved: true,
    passphraseSaved: false,
  };
  assert.deepEqual(Object.keys(hostOf(row)).sort(), Object.keys(emptyHost()).sort());
  assert.equal(hostOf(row).id, 'h1');
});

test('secrets to save: typed ones, none for an empty field, and forgetting what isn’t used', () => {
  const none = { password: '', passphrase: '' };
  const savedBoth = { passwordSaved: true, passphraseSaved: true };
  // Nothing typed and nothing saved: nothing changes.
  assert.deepEqual(secretsToSave('key', none, undefined), {
    password: undefined,
    passphrase: undefined,
  });
  // A key: its passphrase as typed, and a password saved before forgotten.
  assert.deepEqual(secretsToSave('key', { password: 'x', passphrase: 'p' }, savedBoth), {
    password: '',
    passphrase: 'p',
  });
  // A password left empty keeps the saved one; the key's passphrase goes.
  assert.deepEqual(secretsToSave('password', none, savedBoth), {
    password: undefined,
    passphrase: '',
  });
  assert.deepEqual(
    secretsToSave('keyboard-interactive', { password: 'pw', passphrase: '' }, undefined),
    {
      password: 'pw',
      passphrase: undefined,
    }
  );
  // The agent and the system's ssh take neither.
  assert.deepEqual(secretsToSave('agent', none, savedBoth), { password: '', passphrase: '' });
});

test('how a setup script ended, in words', () => {
  assert.deepEqual(setupVerdict({ status: 0, output: '' }), {
    label: 'The setup script worked.',
    tone: 'ok',
  });
  assert.equal(setupVerdict({ status: 2, output: '' }).tone, 'error');
  assert.match(setupVerdict({ status: 2, output: '' }).label, /exit status 2/);
  assert.equal(setupVerdict({ status: null, output: '' }).tone, 'warn');
});
