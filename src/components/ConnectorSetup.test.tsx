// Component test: a connector's "Set up" on a cloud host whose program is
// installed but signed out. The sign-in runs there; its page opens in this
// computer's browser, and the code pasted here goes to it, or, where the
// page asks for a code the sign-in printed, the user is told where it is.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorSetup from './ConnectorSetup';
import { newConnector, type ConnectorProbe } from '../lib/connectors';
import type { SetupJob } from '../lib/connectorSetup';

const URL = 'https://claude.com/cai/oauth/authorize?code=true';
const DEVICE_URL = 'https://auth.openai.com/codex/device';
const sent: { cmd: string; args: any }[] = [];
let signedIn = false;
let code = '';
/** Codex's sign-in, with a device code: done once the user entered it. */
let device = false;
let entered = false;

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    sent.push({ cmd, args });
    if (cmd !== 'connector_setup') return null;
    if (args.op.op === 'start') device = args.op.args?.kind === 'codex';
    if (device) {
      return {
        id: 'd1',
        action: 'login',
        command: 'codex login --device-auth',
        output: `Open ${DEVICE_URL} and enter this one-time code: ABCD-1234`,
        url: DEVICE_URL,
        deviceCode: true,
        done: entered,
      } satisfies SetupJob;
    }
    // An update is done as soon as it starts.
    if (args.op.op === 'start' && args.op.action === 'update') {
      return {
        id: 'u1',
        action: 'update',
        command: 'claude update',
        output: 'Updated',
        done: true,
      };
    }
    const job = (done: boolean): SetupJob => ({
      id: 'j1',
      action: 'login',
      command: 'claude auth login',
      output: `visit: ${URL}\nPaste code here if prompted >`,
      url: URL,
      done,
    });
    if (args.op.op === 'send') code = args.op.text;
    // Signed in once the code came.
    signedIn = code !== '';
    return job(signedIn);
  },
};

const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(check: () => boolean, ms = 5_000) {
  const end = Date.now() + ms;
  while (!check() && Date.now() < end) await settle(20);
  assert.ok(check(), 'timed out');
}

const button = (root: HTMLElement, text: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent === text) as HTMLButtonElement;

test('signing in on a host opens its page here and sends the pasted code', async () => {
  const probes: string[] = [];
  const probe = async (): Promise<ConnectorProbe> => {
    probes.push(signedIn ? 'in' : 'out');
    return { program: 'claude', version: '2.1.309', models: [], auth: { loggedIn: signedIn } };
  };
  const root = container();
  const dispose = render(
    () => (
      <ConnectorSetup
        connector={{ ...newConnector('claude-code'), id: 'c1' }}
        title="Claude Code"
        host="h1"
        where="on Build server"
        probe={probe}
        signedIn={false}
      />
    ),
    root
  );

  button(root, 'Set up').click();
  await until(() => !!root.querySelector('input[aria-label="Sign-in code"]'));
  const start = sent.find((s) => s.cmd === 'connector_setup')!;
  assert.equal(start.args.host, 'h1');
  assert.equal(start.args.op.op, 'start');
  assert.equal(start.args.op.action, 'login');
  // On a host, the page is opened in this computer's browser, once.
  await until(() => sent.some((s) => s.cmd === 'open_url'));
  assert.deepEqual(
    sent.filter((s) => s.cmd === 'open_url').map((s) => s.args.url),
    [URL]
  );
  assert.match(root.textContent ?? '', /paste the code it shows/);

  const input = root.querySelector('input[aria-label="Sign-in code"]') as HTMLInputElement;
  input.value = 'abc#xyz';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  button(root, 'Send').click();
  await until(() => /Claude Code is ready on Build server\./.test(root.textContent ?? ''));
  assert.equal(code, 'abc#xyz');
  assert.deepEqual(probes, ['out', 'in']);
  assert.equal(sent.filter((s) => s.cmd === 'open_url').length, 1);
  dispose();
});

test('a newer version is offered, updated to, and tested again', async () => {
  let version = '2.1.309';
  const probe = async (): Promise<ConnectorProbe> => {
    const probed = { program: 'claude', version, models: [], update: null };
    version = '2.1.310';
    return probed;
  };
  const root = container();
  const dispose = render(
    () => (
      <ConnectorSetup
        connector={{ ...newConnector('claude-code'), id: 'c2' }}
        title="Claude Code"
        host={null}
        where="on this computer"
        probe={probe}
        signedIn={true}
        update="2.1.310"
      />
    ),
    root
  );
  version = '2.1.310';
  button(root, 'Update to 2.1.310').click();
  await until(() => /Updated: 2\.1\.310\./.test(root.textContent ?? ''));
  const start = sent.filter((s) => s.cmd === 'connector_setup').at(-1)!;
  assert.equal(start.args.op.action, 'update');
  assert.equal(start.args.host, null);
  dispose();
});

test('a sign-in whose page asks for the code it printed says where it is, with nothing to paste', async () => {
  let loggedIn = false;
  const probe = async (): Promise<ConnectorProbe> => {
    loggedIn = entered;
    return { program: 'codex', version: 'codex-cli 0.164.0', models: [], auth: { loggedIn } };
  };
  const root = container();
  const dispose = render(
    () => (
      <ConnectorSetup
        connector={{ ...newConnector('codex'), id: 'c3' }}
        title="Codex"
        host="h1"
        where="on Build server"
        probe={probe}
        signedIn={false}
      />
    ),
    root
  );
  button(root, 'Set up').click();
  await until(() => /enter there the code printed below/.test(root.textContent ?? ''));
  assert.equal(
    root.querySelector('input[aria-label="Sign-in code"]'),
    null,
    'nothing to paste back'
  );
  assert.ok(button(root, 'Open sign-in page'));
  // The page opens here; the code is in what the step printed.
  await until(() => sent.some((s) => s.cmd === 'open_url' && s.args.url === DEVICE_URL));
  assert.match(root.querySelector('.connector-setup-output pre')?.textContent ?? '', /ABCD-1234/);
  entered = true;
  await until(() => /Codex is ready on Build server\./.test(root.textContent ?? ''));
  assert.ok(loggedIn);
  dispose();
});
