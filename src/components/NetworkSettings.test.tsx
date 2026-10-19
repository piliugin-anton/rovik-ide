// Component tests: Settings → Network. The timeout; the proxy's three
// choices: none, the first, which hides what a proxy has; the system's (said
// as it is now); or a custom HTTP or SOCKS5 one, saved once it has a host and
// a port, a proxy's URL pasted whole taken apart; its password, kept by the
// keychain and forgotten on request; cloud hosts; and the agent's browsers,
// here, in the cloud or both.
// The app is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import NetworkSettings from './NetworkSettings';
import type { NetworkView } from '../lib/network';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') return 0;
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

/** What the backend keeps, as it answers. */
let kept: NetworkView;

/** The settings besides the proxy, as the backend has them at first. */
const REST = { cloudHosts: false, browser: false, browserPlaces: 'local' as const };

function app(view: Partial<NetworkView> = {}, refuse?: string) {
  calls = [];
  kept = {
    settings: { proxy: 'direct', ...REST },
    passwordSaved: false,
    system: { https: 'http://proxy.corp:3128', http: 'http://proxy.corp:3128' },
    defaultTimeoutSecs: 30,
    minTimeoutSecs: 5,
    maxTimeoutSecs: 300,
    ...view,
  };
  backend = {
    network_settings: () => kept,
    network_save: ({ settings, password }) => {
      if (refuse) throw refuse;
      kept = {
        ...kept,
        settings,
        passwordSaved: password === undefined ? kept.passwordSaved : password !== '',
      };
      return kept;
    },
  };
}

const SAVED_PROXY = { kind: 'http' as const, host: 'proxy.example.com', port: 3128 };

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
after(() => dispose());

async function page() {
  dispose();
  dispose = render(() => <NetworkSettings />, container());
  await flush();
}

const saves = () => calls.filter((c) => c.cmd === 'network_save').map((c) => c.args);
const text = (el: Element | null) => el?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
const radio = (label: string) =>
  [...document.querySelectorAll<HTMLInputElement>('input[name="network-proxy"]')].find((r) =>
    text(r.closest('label')).startsWith(label)
  )!;
const form = () => document.querySelector('.network-proxy-form');
const field = (label: string) =>
  [...document.querySelectorAll('.network-proxy-form label')]
    .find((l) => text(l).startsWith(label))!
    .querySelector('input') as HTMLInputElement;
const statuses = () => [...document.querySelectorAll('.settings-status')].map(text);
const cloudHosts = () =>
  document.querySelector<HTMLInputElement>('.network-cloud-hosts input[type="checkbox"]')!;

async function choose(input: HTMLInputElement) {
  input.checked = true;
  input.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
}

/** Types `value`, then leaves the field. */
async function enter(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  input.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
}

/** Which of what a proxy has the page shows. */
const shown = () => ({
  form: Boolean(form()),
  cloudHosts: Boolean(document.querySelector('.network-cloud-hosts')),
  browser: Boolean(document.querySelector('.network-browser')),
  places: Boolean(document.querySelector('select[aria-label="Which browsers"]')),
});
const NOTHING = { form: false, cloudHosts: false, browser: false, places: false };

test('no proxy is chosen at first, and what a proxy has hides with it', async () => {
  app();
  await page();
  const radios = [...document.querySelectorAll<HTMLInputElement>('input[name="network-proxy"]')];
  assert.deepEqual(
    radios.map((r) => r.nextElementSibling!.firstChild!.textContent!.trim()),
    ['No proxy', 'System proxy', 'Custom']
  );
  assert.deepEqual(
    radios.map((r) => r.checked),
    [true, false, false]
  );
  // The system's is said as it is now, for choosing it.
  assert.match(text(radio('System proxy').closest('label')), /Now: proxy\.corp:3128 \(HTTP\)\./);
  assert.deepEqual(shown(), NOTHING);
  const timeout = document.querySelector<HTMLInputElement>('.setting-number-field input')!;
  assert.equal(timeout.placeholder, '30', 'the timeout is no proxy’s too');
  assert.deepEqual(saves(), []);

  await choose(radio('System proxy'));
  assert.deepEqual(saves(), [{ settings: { proxy: 'system', ...REST }, password: undefined }]);
  assert.deepEqual(shown(), { ...NOTHING, cloudHosts: true, browser: true, places: true });

  await choose(radio('Custom'));
  assert.deepEqual(shown(), { form: true, cloudHosts: true, browser: true, places: true });
  assert.equal(saves().length, 1, 'nothing to save without a host and a port');

  await choose(radio('No proxy'));
  assert.deepEqual(saves().at(-1), { settings: { proxy: 'direct', ...REST }, password: undefined });
  assert.equal(radio('No proxy').checked, true);
  assert.deepEqual(shown(), NOTHING);
});

test('a custom proxy is saved once it has a host and a port', async () => {
  app();
  await page();
  await choose(radio('Custom'));
  assert.ok(form(), 'its fields show');
  assert.equal(radio('Custom').checked, true);
  assert.deepEqual(statuses(), [
    'Not in use yet: enter the proxy’s host and port. Until then, connections go directly.',
  ]);
  assert.equal(field('Port').placeholder, '3128');

  await enter(field('Host'), 'proxy.example.com');
  assert.deepEqual(saves(), [], 'not without a port');
  await enter(field('Port'), '3128');
  assert.deepEqual(saves(), [
    {
      settings: { proxy: 'custom', custom: SAVED_PROXY, ...REST },
      password: undefined,
    },
  ]);
  assert.deepEqual(statuses(), []);
  assert.equal(radio('Custom').checked, true);

  // Signing in: the username is kept with it.
  await enter(field('Username'), ' alice ');
  assert.deepEqual(saves().at(-1)!.settings.custom, { ...SAVED_PROXY, username: 'alice' });
});

test('a proxy’s URL pasted in the host field gives its kind and port', async () => {
  app();
  await page();
  await choose(radio('Custom'));
  await enter(field('Host'), 'socks5://10.0.0.2:1080/');
  assert.deepEqual(saves(), [
    {
      settings: {
        proxy: 'custom',
        custom: { kind: 'socks5', host: '10.0.0.2', port: 1080 },
        ...REST,
      },
      password: undefined,
    },
  ]);
  const selected = document.querySelector('.network-proxy-kind [aria-checked="true"]');
  assert.equal(text(selected), 'SOCKS5');
  assert.equal(field('Host').value, '10.0.0.2');
  assert.equal(field('Port').value, '1080');
});

test('the password goes to the keychain and can be forgotten', async () => {
  app({ settings: { proxy: 'custom', custom: { ...SAVED_PROXY, username: 'alice' }, ...REST } });
  await page();
  const password = () => field('Password');
  assert.equal(password().placeholder, 'Only if the proxy asks for one');
  await enter(password(), 's3cret');
  assert.deepEqual(saves(), [{ settings: kept.settings, password: 's3cret' }]);
  assert.equal(password().value, '', 'not shown once saved');
  assert.equal(password().placeholder, 'Saved: leave empty to keep it');

  // Leaving it empty keeps it.
  await enter(password(), '');
  assert.equal(saves().length, 1);

  const forget = [...document.querySelectorAll('button')].find(
    (b) => text(b) === 'Forget the saved password'
  )!;
  forget.click();
  await flush();
  assert.deepEqual(saves().at(-1), { settings: kept.settings, password: '' });
  assert.equal(password().placeholder, 'Only if the proxy asks for one');

  // Another kind keeps the rest.
  [...document.querySelectorAll<HTMLButtonElement>('.network-proxy-kind button')]
    .find((b) => text(b) === 'SOCKS5')!
    .click();
  await flush();
  assert.deepEqual(saves().at(-1)!.settings.custom, {
    ...SAVED_PROXY,
    kind: 'socks5',
    username: 'alice',
  });
});

test('no proxy or the system’s keeps the custom one for later', async () => {
  app({ settings: { proxy: 'custom', custom: SAVED_PROXY, ...REST } });
  await page();
  assert.equal(radio('Custom').checked, true);
  await choose(radio('System proxy'));
  assert.deepEqual(saves(), [
    { settings: { proxy: 'system', custom: SAVED_PROXY, ...REST }, password: undefined },
  ]);
  assert.equal(form(), null);
  await choose(radio('No proxy'));
  assert.deepEqual(saves().at(-1), {
    settings: { proxy: 'direct', custom: SAVED_PROXY, ...REST },
    password: undefined,
  });
  await choose(radio('Custom'));
  assert.deepEqual(saves().at(-1), {
    settings: { proxy: 'custom', custom: SAVED_PROXY, ...REST },
    password: undefined,
  });
  assert.equal(field('Host').value, 'proxy.example.com');
  // Emptied, it isn't saved: the saved one stays in use, and the page says so.
  await enter(field('Host'), '');
  assert.equal(saves().length, 3);
  assert.deepEqual(statuses(), [
    'Not in use yet: enter the proxy’s host and port. Until then, connections use the saved one, proxy.example.com:3128.',
  ]);
});

test('the timeout and cloud hosts are saved as they change', async () => {
  app({ settings: { proxy: 'system', ...REST }, system: { https: null, http: null } });
  await page();
  const timeout = document.querySelector<HTMLInputElement>('.setting-number-field input')!;
  await enter(timeout, '60');
  assert.deepEqual(saves().at(-1), {
    settings: { proxy: 'system', ...REST, timeoutSecs: 60 },
    password: undefined,
  });
  await enter(timeout, '');
  assert.equal(saves().at(-1)!.settings.timeoutSecs, undefined, 'empty: the default');

  cloudHosts().checked = true;
  cloudHosts().dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal(saves().at(-1)!.settings.cloudHosts, true);
  assert.deepEqual(statuses(), ['No effect now: this computer has no proxy set.']);
});

test('what the backend refuses is said, and the fields keep what was typed', async () => {
  app({}, 'Enter only the proxy’s name or address, such as proxy.example.com.');
  await page();
  await choose(radio('Custom'));
  await enter(field('Port'), '8080');
  await enter(field('Host'), 'me@proxy');
  assert.equal(saves().length, 1);
  assert.ok(
    statuses().includes('Enter only the proxy’s name or address, such as proxy.example.com.'),
    statuses().join(' | ')
  );
  assert.equal(field('Host').value, 'me@proxy');
  assert.equal(field('Port').value, '8080');
});

test('use proxy for browser: off at first, then for the browsers chosen', async () => {
  app({
    settings: {
      proxy: 'custom',
      custom: { ...SAVED_PROXY, kind: 'socks5', username: 'alice' },
      ...REST,
    },
  });
  await page();
  const browser = document.querySelector<HTMLInputElement>(
    '.network-browser input[type="checkbox"]'
  )!;
  const places = document.querySelector<HTMLSelectElement>('select[aria-label="Which browsers"]')!;
  assert.equal(browser.checked, false);
  assert.equal(places.disabled, true, 'nothing to choose while it is off');
  assert.deepEqual(
    [...places.options].map((o) => [o.value, o.textContent]),
    [
      ['local', 'Local only'],
      ['cloud', 'Cloud only'],
      ['both', 'Local + cloud'],
    ]
  );
  assert.doesNotMatch(
    text(form()),
    /can’t sign in to a SOCKS5 proxy/,
    'the browser doesn’t use it yet'
  );

  browser.checked = true;
  browser.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.deepEqual(saves().at(-1)!.settings, { ...kept.settings, browser: true });
  assert.equal(places.disabled, false);
  assert.match(text(form()), /The agent’s browser can’t sign in to a SOCKS5 proxy/);
  assert.doesNotMatch(text(document.body), /A cloud host’s browser is given the proxy/);

  places.value = 'both';
  places.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal(saves().at(-1)!.settings.browserPlaces, 'both');
  assert.match(text(document.body), /A cloud host’s browser is given the proxy, and its password/);
});

test('without a keychain, the file is unlocked here and the password typed is saved', async () => {
  app({
    settings: { proxy: 'custom', custom: { ...SAVED_PROXY, username: 'alice' }, ...REST },
    store: { kind: 'file', unlocked: false },
  });
  const save = backend.network_save;
  backend.network_save = (args) => {
    if (args.password !== undefined && kept.store?.kind === 'file' && !kept.store.unlocked) {
      throw 'The saved passwords and keys are locked: enter their passphrase first.';
    }
    return save(args);
  };
  backend.hosts_unlock = ({ passphrase }) => {
    assert.equal(passphrase, 'open sesame');
    kept = { ...kept, store: { kind: 'file', unlocked: true } };
  };
  await page();
  assert.match(text(form()), /This computer has no keychain/);
  await enter(field('Password'), 's3cret');
  assert.ok(
    statuses().includes('The saved passwords and keys are locked: enter their passphrase first.'),
    statuses().join(' | ')
  );
  assert.equal(field('Password').value, 's3cret', 'kept to save once unlocked');

  const passphrase = document.querySelector<HTMLInputElement>(
    'input[aria-label="passphrase of the secrets file"]'
  )!;
  passphrase.value = 'open sesame';
  passphrase.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  [...document.querySelectorAll<HTMLButtonElement>('button')]
    .find((b) => text(b) === 'Unlock')!
    .click();
  await flush();
  assert.deepEqual(saves().at(-1), { settings: kept.settings, password: 's3cret' });
  assert.equal(kept.passwordSaved, true);
  assert.doesNotMatch(text(form()), /This computer has no keychain/, 'unlocked: no more box');
  assert.deepEqual(statuses(), []);
  assert.equal(field('Password').placeholder, 'Saved: leave empty to keep it');
});
