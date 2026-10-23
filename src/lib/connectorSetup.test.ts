// "Set up" (lib/connectorSetup.ts): the flow the same for every connector,
// and a step followed to its end. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Provider } from '../types';
import { newConnector, type ConnectorProbe } from './connectors';
import {
  runSetupStep,
  setUpConnector,
  SetupDeclined,
  type SetupAction,
  type SetupFlow,
  type SetupJob,
  type SetupOp,
} from './connectorSetup';

const probe = (loggedIn?: boolean): ConnectorProbe => ({
  program: 'claude',
  version: '2.1.309 (Claude Code)',
  models: [],
  auth: loggedIn === undefined ? null : { loggedIn },
});

/** A flow whose probes answer in turn, recording what it was asked. */
function flow(answers: (ConnectorProbe | Error)[], install = true) {
  const asked: string[] = [];
  const f: SetupFlow = {
    probe: async () => {
      asked.push('probe');
      const next = answers.shift()!;
      if (next instanceof Error) throw next;
      return next;
    },
    step: async (action: SetupAction) => {
      asked.push(action);
      return { id: action, action, command: action, output: '', done: true };
    },
    confirmInstall: async () => {
      asked.push('confirm');
      return install;
    },
  };
  return { f, asked };
}

const missing = () => new Error("claude isn't installed, or isn't on the PATH.");

test('a connector installed and signed in is only tested', async () => {
  const { f, asked } = flow([probe(true)]);
  assert.equal((await setUpConnector(f)).auth?.loggedIn, true);
  assert.deepEqual(asked, ['probe']);
});

test('a missing program is installed once agreed, then signed in', async () => {
  const { f, asked } = flow([missing(), probe(false), probe(true)]);
  assert.equal((await setUpConnector(f)).auth?.loggedIn, true);
  assert.deepEqual(asked, ['probe', 'confirm', 'install', 'probe', 'login', 'probe']);
});

test('declining the install changes nothing', async () => {
  const { f, asked } = flow([missing()], false);
  await assert.rejects(setUpConnector(f), SetupDeclined);
  assert.deepEqual(asked, ['probe', 'confirm']);
});

test('other failures are the test’s, and a sign-in it can’t check is left alone', async () => {
  const { f } = flow([new Error('claude --version failed: boom')]);
  await assert.rejects(setUpConnector(f), /boom/);
  const unknown = flow([probe()]);
  await setUpConnector(unknown.f);
  assert.deepEqual(unknown.asked, ['probe']);
});

const connector: Provider = { ...newConnector('claude-code'), id: 'c1', apiKey: 'k' };

/** A step's calls: it runs for `polls` polls, then ends with `error`. */
function fakeStep(polls: number, error?: string) {
  const ops: SetupOp[] = [];
  let left = polls;
  const job = (done: boolean): SetupJob => ({
    id: 'j1',
    action: 'login',
    command: 'claude auth login',
    output: 'Paste code here >',
    url: 'https://claude.com/x',
    done,
    error: done ? error : null,
  });
  const call = async (op: SetupOp) => {
    ops.push(op);
    if (op.op === 'cancel') left = 0;
    if (op.op === 'poll') left--;
    return job(left <= 0);
  };
  return { call, ops };
}

test('a step is followed to its end, with the connector as the core takes it', async () => {
  const { call, ops } = fakeStep(2);
  const seen: boolean[] = [];
  const job = await runSetupStep(call, connector, 'login', (j) => seen.push(j.done), undefined, 1);
  assert.equal(job.done, true);
  assert.deepEqual(seen, [false, false, true]);
  assert.deepEqual(ops[0], {
    op: 'start',
    action: 'login',
    args: { kind: 'claude-code', settings: connector.settings, apiKey: 'k', request: false },
  });
  assert.deepEqual(
    ops.slice(1).map((o) => o.op),
    ['poll', 'poll']
  );
});

test('a failed step throws its error; an abort stops it', async () => {
  const failed = fakeStep(1, 'Login failed: 400');
  await assert.rejects(
    runSetupStep(failed.call, connector, 'login', () => {}, undefined, 1),
    /Login failed: 400/
  );

  const stopping = fakeStep(1_000, 'Stopped.');
  const abort = new AbortController();
  const running = runSetupStep(
    stopping.call,
    connector,
    'login',
    () => abort.abort(),
    abort.signal,
    1
  );
  await assert.rejects(running, /Stopped\./);
  assert.ok(stopping.ops.some((o) => o.op === 'cancel'));
});
