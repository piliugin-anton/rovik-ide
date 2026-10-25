// Component test: Settings → Connectors' form, laid out the same for every
// kind (ConnectorsSettings.tsx): its name; the Connection section, with
// the kind's sign-in fields before the test and Set up; the kind's own
// options (connectorFields.ts); its models.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorsSettings from './ConnectorsSettings';
import { ConfirmDialogHost } from './ConfirmDialog';
import {
  connectorAuthState,
  newConnector,
  setConnectorAuthState,
  type ConnectorProbe,
} from '../lib/connectors';

// The cloud hosts to test on (none, unless a test names some); the
// connector's program, as each test has it answer (a failure as the app's
// commands give one: its text).
let hosts: { id: string; name: string }[] = [];
let answer: (cmd: string, args: any) => unknown = () => null;
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
  // Where each test ran: a host's id, or null for this computer.
  const testedOn: (string | null)[] = [];
  answer = (cmd, args) => {
    if (cmd !== 'connector_probe') return null;
    testedOn.push(args.host);
    return READY;
  };
  hosts = [{ id: 'h1', name: 'Build server' }];
  const claude = { ...newConnector('claude-code'), id: 'c5' };
  // Found signed out on this computer: its row offers Set up.
  setConnectorAuthState(claude.id, { state: 'signedOut', auth: { loggedIn: false } });
  const { root, dispose } = page(claude);
  const computer = () =>
    root.querySelector<HTMLSelectElement>('select[aria-label="Where to test"]');
  try {
    // A form tests on the host its Computer picks, then closes.
    button(root, 'Edit').click();
    await until(() => computer()?.options.length === 2);
    computer()!.value = 'h1';
    computer()!.dispatchEvent(new Event('change', { bubbles: true }));
    button(root, 'Test connection').click();
    await until(() => /Connected/.test(root.textContent ?? ''));
    assert.deepEqual(testedOn, ['h1']);
    button(root, 'Cancel').click();

    // The row's Set up opens a form on this computer and sets it up here.
    button(root, 'Set up').click();
    await until(() => /is ready/.test(root.textContent ?? ''));
    assert.equal(computer()?.value, '');
    assert.deepEqual(testedOn, ['h1', null]);
    assert.match(root.textContent ?? '', /Claude Code is ready on this computer\./);
  } finally {
    hosts = [];
    dispose();
  }
});

test('picking another computer forgets the last one’s test, and a test still running there', async () => {
  // Signed in on the host, where a newer version is out: each test there
  // answers when this test says.
  let answerHost = () => {};
  answer = (cmd) =>
    cmd === 'connector_probe'
      ? new Promise((resolve) => (answerHost = () => resolve({ ...READY, update: '2.2.0' })))
      : null;
  hosts = [{ id: 'h1', name: 'Build server' }];
  const claude = { ...newConnector('claude-code'), id: 'c6' };
  const { root, dispose } = page(claude);
  const computer = () =>
    root.querySelector<HTMLSelectElement>('select[aria-label="Where to test"]');
  const pick = (host: string) => {
    computer()!.value = host;
    computer()!.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const setUpButtons = () =>
    [...root.querySelectorAll('.connector-setup button')].map((b) => b.textContent);
  try {
    button(root, 'Edit').click();
    await until(() => computer()?.options.length === 2);

    // A test on the host shows what it found there, and offers its Update and Sign out.
    pick('h1');
    button(root, 'Test connection').click();
    answerHost();
    await until(() => /Connected/.test(root.textContent ?? ''));
    assert.deepEqual(setUpButtons(), ['Set up', 'Update to 2.2.0', 'Sign out']);

    // Back on this computer, none of that is shown or offered here.
    pick('');
    assert.doesNotMatch(root.textContent ?? '', /Connected/);
    assert.deepEqual(setUpButtons(), ['Set up']);

    // A test on the host that ends after the pick isn't this computer's sign-in.
    pick('h1');
    button(root, 'Test connection').click();
    pick('');
    answerHost();
    await settle(20);
    assert.doesNotMatch(root.textContent ?? '', /Connected/);
    assert.equal(connectorAuthState(claude.id), undefined);
  } finally {
    hosts = [];
    dispose();
  }
});
