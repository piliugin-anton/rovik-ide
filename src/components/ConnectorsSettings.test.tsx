// Component test: Settings → Connectors' form, laid out the same for every
// kind (ConnectorsSettings.tsx): its name; the Connection section, with
// the kind's sign-in fields before the test and Set up; the kind's own
// options (connectorFields.ts); its models. And the status each
// connector's last check found, checked again as the page and a form open.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorsSettings from './ConnectorsSettings';
import { ConfirmDialogHost } from './ConfirmDialog';
import {
  authStateOf,
  cantRun,
  connectorAuthState,
  newConnector,
  setConnectorAuthState,
  type ConnectorProbe,
} from '../lib/connectors';

// The cloud hosts to test on (none, unless a test names some); the
// connector's program, as each test has it answer (a failure as the app's
// commands give one: its text): installed and signed in, unless it says.
let hosts: { id: string; name: string }[] = [];
let answer: (cmd: string, args: any) => unknown = (cmd) =>
  cmd === 'connector_probe' ? READY : null;
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => (cmd === 'hosts_list' ? { hosts } : answer(cmd, args)),
};

const MISSING =
  "claude isn't installed, or isn't on the PATH. Install Claude Code (curl -fsSL https://claude.ai/install.sh | bash, or your package manager), or name the program's path in the connector's settings.";
const READY: ConnectorProbe = {
  program: 'claude',
  version: '2.1.310 (Claude Code)',
  models: [],
  auth: { loggedIn: true },
};

const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(check: () => boolean, ms = 5_000) {
  const end = Date.now() + ms;
  while (!check() && Date.now() < end) await settle(10);
  assert.ok(check(), 'timed out');
}

/** A button by its text, anywhere in `root` (the document: a confirm dialog's). */
const button = (root: ParentNode, text: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent === text) as HTMLButtonElement;

/** The errors the form shows, by their text. */
const errors = (root: HTMLElement) =>
  [...root.querySelectorAll('.provider-test-status.error')].map((e) => e.textContent);

/** Settings → Connectors with one connector, and the app's confirm dialogs. */
function page(connector: ReturnType<typeof newConnector>) {
  const root = container();
  const dispose = render(
    () => (
      <>
        <ConnectorsSettings providers={[connector]} onChange={() => {}} />
        <ConfirmDialogHost />
      </>
    ),
    root
  );
  return { root, dispose };
}

/** The form's Computer: where it tests and sets up. */
const computer = (root: ParentNode) =>
  root.querySelector<HTMLSelectElement>('select[aria-label="Where to test"]');

/** Picks `host` in the form's Computer ('' for this computer), as the user does. */
function pick(root: ParentNode, host: string) {
  const select = computer(root)!;
  select.value = host;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

/** What the form's test found, as it shows it. */
const testStatus = (root: ParentNode) =>
  root.querySelector('.connector-test-status.ok')?.textContent ?? '';

/** The buttons of the form's Set up row. */
const setUpButtons = (root: ParentNode) =>
  [...root.querySelectorAll('.connector-setup button')].map((b) => b.textContent);

/**
 * What one of the form's parts is: a section by its title (one without, by
 * its first row), a row by its label, anything else by its class.
 */
function partName(el: Element): string {
  if (el.classList.contains('settings-section'))
    return (
      el.querySelector('.settings-section-title, .setting-row-label')?.textContent ?? 'section'
    );
  if (el.classList.contains('setting-row'))
    return el.querySelector('.setting-row-label')?.textContent ?? 'setting-row';
  return el.classList[0] ?? el.tagName;
}

test('a connector’s form: its name, how it connects, its kind’s options, its models', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1' };
  const root = container();
  const dispose = render(
    () => <ConnectorsSettings providers={[claude]} onChange={() => {}} />,
    root
  );
  const edit = [...root.querySelectorAll('button')].find((b) => b.textContent === 'Edit');
  (edit as HTMLButtonElement).click();

  const form = root.querySelector('.connector-form')!;
  assert.deepEqual([...form.children].map(partName), [
    'Name',
    'Connection',
    'How chats use it',
    'Permissions',
    'Limits',
    'Advanced',
    'provider-models',
    'profile-form-actions',
  ]);
  // How it signs in comes first in Connection, then the test of it and Set up.
  const connection = [...form.children].find((el) => partName(el) === 'Connection')!;
  assert.deepEqual([...connection.children].map(partName), [
    'settings-section-title',
    'Signs in with',
    'settings-hint',
    'connector-checks',
  ]);
  const checks = connection.querySelector('.connector-checks')!;
  assert.deepEqual([...checks.children].map(partName), ['provider-test-row', 'connector-setup']);
  dispose();
});

test('Set up shows no error for the program it installs, while it installs or after', async () => {
  let installed = false;
  let finishInstall = () => {};
  answer = (cmd, args) => {
    if (cmd === 'connector_probe') return installed ? READY : Promise.reject(MISSING);
    if (cmd === 'connector_setup' && args.op.op === 'start') {
      // The installer runs until the test lets it end.
      return new Promise((resolve) => {
        finishInstall = () => {
          installed = true;
          resolve({
            id: 'j1',
            action: 'install',
            command: 'curl -fsSL https://claude.ai/install.sh | bash',
            output: 'Installation complete!',
            done: true,
          });
        };
      });
    }
    return null;
  };
  const { root, dispose } = page({ ...newConnector('claude-code'), id: 'c2' });
  button(root, 'Edit').click();
  button(root.querySelector('.connector-setup')!, 'Set up').click();

  // The test found it missing: Set up asks to install it, and that's no failure.
  await until(() => !!document.querySelector('.confirm-dialog'));
  assert.deepEqual(errors(root), []);
  button(document, 'Install').click();
  await until(() => root.querySelector('.connector-setup-stage')?.textContent === 'Installing…');
  assert.deepEqual(errors(root), []);

  finishInstall();
  await until(() => /Claude Code is ready on this computer\./.test(root.textContent ?? ''));
  assert.deepEqual(errors(root), []);
  assert.match(root.textContent ?? '', /Connected: 2\.1\.310 \(Claude Code\)/);
  dispose();
});

test('a failure Set up can’t act on shows once, as its result; the Test button shows its own', async () => {
  const failed = 'claude --version failed: bad CPU type in executable';
  answer = (cmd) => (cmd === 'connector_probe' ? Promise.reject(failed) : null);
  const { root, dispose } = page({ ...newConnector('claude-code'), id: 'c3' });
  button(root, 'Edit').click();
  button(root.querySelector('.connector-setup')!, 'Set up').click();
  await until(() => errors(root).length > 0);
  await settle(20);
  assert.deepEqual(errors(root), [failed]);
  assert.ok(root.querySelector('.connector-setup .provider-test-status.error'));

  button(root, 'Test connection').click();
  await until(() => !!root.querySelector('.connector-test-row ~ .provider-test-status.error'));
  assert.equal(
    root.querySelector('.connector-test-row ~ .provider-test-status.error')?.textContent,
    failed
  );
  dispose();
});

test('the list’s Set up shows what its first test found', async () => {
  answer = (cmd) => (cmd === 'connector_probe' ? READY : null);
  const claude = { ...newConnector('claude-code'), id: 'c4' };
  // Found signed out at start: its row offers Set up.
  setConnectorAuthState(claude.id, { state: 'signedOut', auth: { loggedIn: false } });
  const { root, dispose } = page(claude);
  button(root, 'Set up').click();
  await until(() => /Claude Code is ready on this computer\./.test(root.textContent ?? ''));
  assert.match(root.textContent ?? '', /Connected: 2\.1\.310 \(Claude Code\)/);
  dispose();
});

test('the list’s Set up runs on this computer, even after a form tested on a host', async () => {
  // Signed in on the host; signed out here until Set up signs it in.
  let signedInHere = false;
  const calls: { cmd: string; host: string | null }[] = [];
  answer = (cmd, args) => {
    calls.push({ cmd, host: args.host });
    if (cmd === 'connector_probe') {
      return { ...READY, auth: { loggedIn: !!args.host || signedInHere } };
    }
    if (cmd !== 'connector_setup') return null;
    if (!args.host) signedInHere = true;
    return { id: 'j1', action: 'login', command: 'claude auth login', output: '', done: true };
  };
  hosts = [{ id: 'h1', name: 'Build server' }];
  const { root, dispose } = page({ ...newConnector('claude-code'), id: 'c5' });
  try {
    // A form tests on the host its Computer picks, then closes.
    button(root, 'Edit').click();
    await until(() => computer(root)?.options.length === 2);
    pick(root, 'h1');
    await until(() => /Signed in/.test(testStatus(root)));
    assert.equal(calls[calls.length - 1].host, 'h1');
    button(root, 'Cancel').click();

    // Signed out here, so its row offers Set up: it opens a form on this
    // computer and sets it up here.
    calls.length = 0;
    button(root, 'Set up').click();
    await until(() => /is ready/.test(root.textContent ?? ''));
    assert.equal(computer(root)?.value, '');
    assert.deepEqual(calls, [
      { cmd: 'connector_probe', host: null },
      { cmd: 'connector_setup', host: null },
      { cmd: 'connector_probe', host: null },
    ]);
    assert.match(root.textContent ?? '', /Claude Code is ready on this computer\./);
  } finally {
    hosts = [];
    dispose();
  }
});

test('picking another computer tests there; the last one’s test, or one still running, isn’t kept', async () => {
  // Signed out here; signed in on the host, where a newer version is out.
  // A test on the host answers at once, unless this test holds it.
  let hold = false;
  let answerHost = () => {};
  answer = (cmd, args) => {
    if (cmd !== 'connector_probe') return null;
    if (!args.host) return { ...READY, auth: { loggedIn: false } };
    const there = { ...READY, update: '2.2.0' };
    return hold ? new Promise((resolve) => (answerHost = () => resolve(there))) : there;
  };
  hosts = [{ id: 'h1', name: 'Build server' }];
  const claude = { ...newConnector('claude-code'), id: 'c6' };
  const { root, dispose } = page(claude);
  try {
    button(root, 'Edit').click();
    await until(
      () => computer(root)?.options.length === 2 && /Not signed in/.test(testStatus(root))
    );
    assert.deepEqual(setUpButtons(root), ['Set up']);

    // On the host: what's there, with its Update and Sign out.
    pick(root, 'h1');
    await until(() => /Signed in/.test(testStatus(root)));
    assert.deepEqual(setUpButtons(root), ['Set up', 'Update to 2.2.0', 'Sign out']);

    // Back here: this computer's, none of the host's.
    pick(root, '');
    await until(() => /Not signed in/.test(testStatus(root)));
    assert.deepEqual(setUpButtons(root), ['Set up']);
    assert.doesNotMatch(testStatus(root), /2\.2\.0/);

    // A test on the host that answers after the pick isn't shown, nor kept
    // as this computer's.
    hold = true;
    pick(root, 'h1');
    pick(root, '');
    await until(() => /Not signed in/.test(testStatus(root)));
    answerHost();
    await settle(20);
    assert.match(testStatus(root), /Not signed in/);
    assert.equal(connectorAuthState(claude.id)?.state, 'signedOut');
  } finally {
    hosts = [];
    dispose();
  }
});

test('the list shows what the last check found at once, and checks each connector again, for no tokens', async () => {
  const asked: { request: boolean; host: string | null }[] = [];
  let answerCheck = (_: ConnectorProbe) => {};
  answer = (cmd, args) => {
    if (cmd !== 'connector_probe') return null;
    asked.push({ request: args.args.request, host: args.host });
    return new Promise((resolve) => (answerCheck = resolve));
  };
  const claude = { ...newConnector('claude-code'), id: 'c7' };
  // What the check as the app started found: signed in, an older version.
  setConnectorAuthState(
    claude.id,
    authStateOf({ probe: { ...READY, version: '2.1.300 (Claude Code)' } })
  );
  const { root, dispose } = page(claude);
  const row = () => root.querySelector('.connector-row')?.textContent ?? '';

  // Shown at once, while it's checked again here, with no request.
  assert.match(row(), /Installed: 2\.1\.300 \(Claude Code\)/);
  assert.match(row(), /Signed in/);
  assert.deepEqual(asked, [{ request: false, host: null }]);
  assert.ok(button(root, 'Check status').disabled);

  // What the check finds replaces it.
  answerCheck({ ...READY, auth: { loggedIn: false }, update: '2.2.0' });
  await until(() => /Not signed in/.test(row()));
  assert.match(row(), /Installed: 2\.1\.310 \(Claude Code\) · 2\.2\.0 is available/);
  assert.ok(button(root, 'Set up'), 'signed out: its row offers Set up');
  assert.ok(!button(root, 'Check status').disabled);
  dispose();
});

test('a connector’s form shows what the last check found at once, and tests it again', async () => {
  let probes = 0;
  let release = () => {};
  answer = (cmd) => {
    if (cmd !== 'connector_probe') return null;
    const found = { ...READY, update: '2.2.0' };
    // The list's check answers at once; the form's own test when this test says.
    if (++probes === 1) return found;
    return new Promise(
      (resolve) => (release = () => resolve({ ...found, version: '2.1.320 (Claude Code)' }))
    );
  };
  const { root, dispose } = page({ ...newConnector('claude-code'), id: 'c8' });
  await until(() => /2\.2\.0 is available/.test(root.textContent ?? ''));
  button(root, 'Edit').click();

  // At once: what the list's check found here, with its Update and Sign out.
  assert.match(testStatus(root), /Connected: 2\.1\.310 \(Claude Code\)/);
  assert.match(testStatus(root), /Signed in/);
  assert.deepEqual(setUpButtons(root), ['Set up', 'Update to 2.2.0', 'Sign out']);

  // Tested again meanwhile; what that finds replaces it.
  assert.ok(button(root, 'Testing…').disabled);
  release();
  await until(() => /2\.1\.320/.test(testStatus(root)));
  assert.ok(button(root, 'Test connection'));
  dispose();
});

test('a form’s own test makes no request, even with “with a request” picked; the Test button’s does', async () => {
  const requests: boolean[] = [];
  answer = (cmd, args) => {
    if (cmd !== 'connector_probe') return null;
    requests.push(args.args.request);
    return READY;
  };
  const { root, dispose } = page({ ...newConnector('claude-code'), id: 'c9' });
  button(root, 'Edit').click();
  await until(() => requests.length === 2 && !!button(root, 'Test connection'));

  const withRequest = root.querySelector<HTMLInputElement>('.connector-test-option input')!;
  withRequest.checked = true;
  withRequest.dispatchEvent(new Event('change', { bubbles: true }));
  button(root, 'Test connection').click();
  await until(() => requests.length === 3);

  // The next form opens with it still picked: its own test makes none.
  button(root, 'Cancel').click();
  button(root, 'Edit').click();
  await until(() => requests.length === 4);
  // The list's check and the first form's, the Test button's, the next form's.
  assert.deepEqual(requests, [false, false, true, false]);
  dispose();
});

test('a test that fails on this computer is this computer’s status too', async () => {
  // Installed as the page opens; gone by the time the form tests it.
  let probes = 0;
  answer = (cmd) =>
    cmd !== 'connector_probe' ? null : ++probes === 1 ? READY : Promise.reject(MISSING);
  const claude = { ...newConnector('claude-code'), id: 'c10' };
  const { root, dispose } = page(claude);
  await until(() => connectorAuthState(claude.id)?.state === 'signedIn');
  button(root, 'Edit').click();
  await until(() => errors(root).length > 0);
  assert.deepEqual(errors(root), [MISSING]);
  assert.ok(cantRun(connectorAuthState(claude.id)));
  button(root, 'Cancel').click();
  assert.ok(button(root, 'Set up'), 'its row offers Set up');
  dispose();
});
