// Component tests: Settings → Network. The timeout; the proxies, each typed
// once: added with its password, edited, its password kept or forgotten,
// removed once nothing goes through it, a proxy's URL pasted whole taken
// apart, the encrypted file unlocked to save a password; and where each
// kind of connection goes, each menu with only what its kind takes, saved
// as it's chosen, with the notes that apply then and only then.
// The app is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import NetworkSettings from './NetworkSettings';
import {
  DEFAULT_ROUTES,
  type NetworkSettings as Settings,
  type NetworkView,
  type RouteRow,
  type SavedProxy,
} from '../lib/network';

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

/** The app, with these settings and this view, refusing saves with `refuse`. */
function app(settings: Partial<Settings> = {}, more: Partial<NetworkView> = {}, refuse?: string) {
  calls = [];
  kept = {
    settings: { proxies: [], routes: { ...DEFAULT_ROUTES }, ...settings },
    system: { https: null, http: null },
    defaultTimeoutSecs: 30,
    minTimeoutSecs: 5,
    maxTimeoutSecs: 300,
    ...more,
  };
  backend = {
    network_settings: () => kept,
    network_save: ({ settings, passwords }) => {
      if (refuse) throw refuse;
      const before = new Map(kept.settings.proxies.map((p) => [p.id, p.passwordSaved]));
      const proxies = (settings as Settings).proxies.map((p) => {
        const typed = passwords?.[p.id];
        const saved = typed === undefined ? Boolean(before.get(p.id)) : typed !== '';
        const { passwordSaved: _, ...rest } = p;
        return saved ? { ...rest, passwordSaved: true } : rest;
      });
      kept = { ...kept, settings: { ...settings, proxies } };
      return kept;
    },
  };
}

const SYSTEM = { https: 'http://proxy.corp:3128', http: 'http://proxy.corp:3128' };
const P1: SavedProxy = {
  id: 'p1',
  kind: 'http',
  host: 'proxy.example.com',
  port: 3128,
  username: 'alice',
  passwordSaved: true,
};
const S1: SavedProxy = { id: 's1', kind: 'socks5', host: '10.0.0.2', port: 1080, username: 'bob' };

const MENUS: Record<RouteRow, string> = {
  models: 'Model providers and downloads on this computer',
  ssh: 'SSH to cloud hosts',
  browser: 'Agent’s browser on this computer',
  hostModels: 'Model providers and downloads on cloud hosts',
  hostBrowser: 'Agent’s browser on cloud hosts',
};
const ROWS = Object.keys(MENUS) as RouteRow[];

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
const text = (el: Element | null | undefined) => el?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
const menu = (row: RouteRow) =>
  document.querySelector<HTMLSelectElement>(`select[aria-label="${MENUS[row]}"]`)!;
const options = (row: RouteRow) => [...menu(row).options].map((o) => o.value);
const shown = (row: RouteRow) => text(menu(row).options[menu(row).selectedIndex]);
const optionLabel = (row: RouteRow, value: string) =>
  text([...menu(row).options].find((o) => o.value === value));
const button = (label: string, within: ParentNode = document) =>
  [...within.querySelectorAll<HTMLButtonElement>('button')].find((b) => text(b) === label);
const form = () => document.querySelector('.network-proxy-form');
const field = (label: string) =>
  [...document.querySelectorAll('.network-proxy-form label')]
    .find((l) => text(l).startsWith(label))!
    .querySelector('input') as HTMLInputElement;
const proxyRows = () => [...document.querySelectorAll('.network-proxy')];
/** A saved proxy's row: what it is, and what's said under it. */
const said = (row: Element) => [
  text(row.querySelector('.profile-row-label')),
  text(row.querySelector('.profile-row-sub')),
];
const notes = () => [...document.querySelectorAll('.network-note')].map(text);
const statuses = () => [...document.querySelectorAll('.settings-status')].map(text);
const timeoutField = () => document.querySelector<HTMLInputElement>('.setting-number-field input')!;

async function pick(row: RouteRow, value: string) {
  menu(row).value = value;
  menu(row).dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
}

async function type(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

/** Types `value`, then leaves the field. */
async function enter(input: HTMLInputElement, value: string) {
  await type(input, value);
  input.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
}

async function click(target: HTMLButtonElement | undefined) {
  assert.ok(target, 'the button is there');
  target.click();
  await flush();
}

test('at first: no proxies, and each kind of connection where it goes by default', async () => {
  app();
  await page();
  assert.equal(proxyRows().length, 0);
  assert.match(text(document.body), /None yet: add one to send connections through it\./);
  assert.match(text(document.body), /This computer’s proxy, for “System proxy”: none set\./);

  const groups = [...document.querySelectorAll('.network-route-group')];
  assert.deepEqual(
    groups.map((g) => text(g.querySelector('.network-route-group-title'))),
    ['This computer', 'Cloud hosts']
  );
  assert.deepEqual(
    groups.map((g) => [...g.querySelectorAll('select')].map((s) => s.getAttribute('aria-label'))),
    [
      [MENUS.models, MENUS.ssh, MENUS.browser],
      [MENUS.hostModels, MENUS.hostBrowser],
    ]
  );
  assert.deepEqual(
    ROWS.map((row) => menu(row).value),
    ['auto', 'direct', 'own', 'own', 'own']
  );
  assert.deepEqual(ROWS.map(shown), [
    'Automatic (no proxy)',
    'No proxy',
    'Its own settings',
    'The host’s own settings',
    'Its own settings',
  ]);
  assert.deepEqual(options('models'), ['auto', 'direct', 'system']);
  assert.deepEqual(options('ssh'), ['direct', 'system']);
  assert.deepEqual(options('browser'), ['own', 'direct', 'system']);
  assert.deepEqual(options('hostModels'), ['own', 'direct']);
  assert.deepEqual(options('hostBrowser'), ['own', 'direct']);

  assert.deepEqual(notes(), []);
  assert.deepEqual(statuses(), []);
  assert.equal(timeoutField().placeholder, '30');
  assert.deepEqual(saves(), []);
});

test('automatically, this computer’s proxy where it has one set', async () => {
  app({}, { system: SYSTEM });
  await page();
  assert.equal(shown('models'), 'Automatic (system proxy)');
  assert.match(
    text(document.body),
    /This computer’s proxy, for “System proxy”: proxy\.corp:3128 \(HTTP\)\./
  );
});

test('a proxy is added with its password, then each kind of connection can go through it', async () => {
  app();
  await page();
  await click(button('+ Add a proxy'));
  assert.ok(form(), 'the editor opens');
  assert.equal(button('+ Add a proxy'), undefined, 'in place of the button');
  assert.equal(button('Save')!.disabled, true, 'not without a host and a port');
  assert.equal(field('Port').placeholder, '3128');

  await type(field('Host'), 'proxy.example.com');
  assert.equal(button('Save')!.disabled, true);
  await type(field('Port'), '3128');
  await type(field('Username'), ' alice ');
  await type(field('Password'), 's3cret');
  assert.deepEqual(saves(), [], 'saved with Save');
  await click(button('Save'));

  assert.equal(saves().length, 1);
  const id = saves()[0].settings.proxies[0].id;
  assert.match(id, /^proxy-[a-z0-9]{8}$/);
  assert.deepEqual(saves()[0], {
    settings: {
      proxies: [{ id, kind: 'http', host: 'proxy.example.com', port: 3128, username: 'alice' }],
      routes: DEFAULT_ROUTES,
    },
    passwords: { [id]: 's3cret' },
  });
  assert.equal(form(), null, 'closed once saved');
  assert.deepEqual(proxyRows().map(said), [['HTTP proxy.example.com:3128', 'Signs in as alice']]);
  for (const row of ROWS) assert.ok(options(row).includes(`proxy:${id}`), row);
  assert.equal(optionLabel('hostModels', `proxy:${id}`), 'HTTP proxy.example.com:3128 (alice)');
  assert.match(
    text(document.body),
    /Addresses on this computer \(localhost\) are reached directly/
  );

  // Cancel saves nothing.
  await click(button('+ Add a proxy'));
  await type(field('Host'), 'other.example.com');
  await type(field('Port'), '8080');
  await click(button('Cancel'));
  assert.equal(form(), null);
  assert.equal(saves().length, 1);
  assert.equal(proxyRows().length, 1);
});

test('choosing where a kind of connection goes saves it', async () => {
  app({ proxies: [P1], timeoutSecs: 45 });
  await page();
  await pick('models', 'proxy:p1');
  assert.deepEqual(saves().at(-1)!.settings.routes, { ...DEFAULT_ROUTES, models: { proxy: 'p1' } });
  assert.equal(saves().at(-1)!.passwords, undefined);
  assert.equal(saves().at(-1)!.settings.timeoutSecs, 45, 'the rest as it was');
  await pick('ssh', 'system');
  await pick('hostBrowser', 'direct');
  assert.deepEqual(saves().at(-1)!.settings.routes, {
    ...DEFAULT_ROUTES,
    models: { proxy: 'p1' },
    ssh: 'system',
    hostBrowser: 'direct',
  });
  assert.equal(menu('models').value, 'proxy:p1');
  assert.equal(shown('ssh'), 'System proxy');
  assert.equal(shown('hostBrowser'), 'No proxy');

  // Each menu has only what its kind takes, then the proxy.
  assert.deepEqual(options('ssh'), ['direct', 'system', 'proxy:p1']);
  assert.deepEqual(options('hostModels'), ['own', 'direct', 'proxy:p1']);
  assert.deepEqual(options('hostBrowser'), ['own', 'direct', 'proxy:p1']);
});

test('a proxy is edited: its port, its password kept when left empty, and forgotten on request', async () => {
  app({ proxies: [P1] });
  await page();
  await click(button('Edit', proxyRows()[0]));
  assert.equal(field('Host').value, 'proxy.example.com');
  assert.equal(field('Username').value, 'alice');
  assert.equal(field('Password').placeholder, 'Saved: leave empty to keep it');
  await type(field('Port'), '8080');
  await click(button('Save'));
  assert.deepEqual(saves().at(-1), {
    settings: {
      proxies: [
        { id: 'p1', kind: 'http', host: 'proxy.example.com', port: 8080, username: 'alice' },
      ],
      routes: DEFAULT_ROUTES,
    },
    passwords: undefined,
  });
  assert.equal(kept.settings.proxies[0].passwordSaved, true, 'kept');
  assert.equal(form(), null);

  await click(button('Edit', proxyRows()[0]));
  await click(button('Forget the saved password'));
  assert.deepEqual(saves().at(-1)!.passwords, { p1: '' });
  assert.equal(field('Password').placeholder, 'Only if the proxy asks for one');
  assert.equal(button('Forget the saved password'), undefined);

  // Another kind keeps the rest.
  await click(
    [...document.querySelectorAll<HTMLButtonElement>('.network-proxy-kind button')].find(
      (b) => text(b) === 'SOCKS5'
    )
  );
  assert.equal(field('Port').placeholder, '1080');
  await click(button('Save'));
  assert.deepEqual(saves().at(-1)!.settings.proxies, [
    { id: 'p1', kind: 'socks5', host: 'proxy.example.com', port: 8080, username: 'alice' },
  ]);
  assert.deepEqual(said(proxyRows()[0]), [
    'SOCKS5 proxy.example.com:8080',
    'Signs in as alice, no password saved',
  ]);
});

test('a proxy in use can’t be removed until nothing goes through it', async () => {
  app({
    proxies: [P1],
    routes: { ...DEFAULT_ROUTES, models: { proxy: 'p1' }, hostBrowser: { proxy: 'p1' } },
  });
  await page();
  const remove = () => button('Remove', proxyRows()[0])!;
  assert.equal(
    said(proxyRows()[0])[1],
    'Signs in as alice · In use: model providers and cloud hosts’ browser'
  );
  assert.equal(remove().disabled, true);
  assert.equal(
    remove().title,
    'In use: model providers and cloud hosts’ browser. Choose something else there first.'
  );
  await pick('models', 'direct');
  assert.equal(remove().disabled, true, 'still the cloud hosts’ browser’s');
  await pick('hostBrowser', 'own');
  assert.equal(remove().disabled, false);
  assert.equal(said(proxyRows()[0])[1], 'Signs in as alice');
  await click(remove());
  assert.deepEqual(saves().at(-1)!.settings.proxies, []);
  assert.equal(proxyRows().length, 0);
  assert.equal(options('models').includes('proxy:p1'), false);
});

test('the notes say what applies, and only while it does', async () => {
  app({ proxies: [P1, S1] });
  await page();
  assert.deepEqual(notes(), []);
  assert.deepEqual(statuses(), []);

  await pick('hostModels', 'proxy:p1');
  assert.deepEqual(notes(), [
    'Cloud hosts are given the proxy, and its password, each time this computer connects to them, and keep the password in memory only. They have to be able to reach it; a host other computers use follows the last one to connect.',
  ]);
  await pick('hostModels', 'own');
  await pick('hostBrowser', 'proxy:s1');
  assert.equal(notes().length, 2, notes().join(' | '));
  assert.ok(
    notes().includes(
      'The agent’s browser can’t sign in to a SOCKS5 proxy, so pages it opens won’t load through 10.0.0.2:1080.'
    ),
    notes().join(' | ')
  );
  await pick('hostBrowser', 'own');
  await pick('browser', 'proxy:s1');
  assert.deepEqual(notes(), [
    'The agent’s browser can’t sign in to a SOCKS5 proxy, so pages it opens won’t load through 10.0.0.2:1080.',
  ]);
  await pick('browser', 'own');

  await pick('ssh', 'system');
  assert.deepEqual(notes(), [
    'Hosts set to use your system’s ssh follow its own settings (ProxyJump, ProxyCommand) instead.',
  ]);
  assert.deepEqual(statuses(), [
    'This computer has no proxy set, so “System proxy” goes directly for now.',
  ]);
  await pick('ssh', 'direct');
  assert.deepEqual(notes(), []);
  assert.deepEqual(statuses(), []);
});

test('“System proxy” is said to go directly only while this computer has none', async () => {
  app({ routes: { ...DEFAULT_ROUTES, browser: 'system' } }, { system: SYSTEM });
  await page();
  assert.deepEqual(statuses(), []);
});

test('what the backend refuses is said, and the editor keeps what was typed', async () => {
  app({}, {}, 'Enter only the proxy’s name or address, such as proxy.example.com.');
  await page();
  await click(button('+ Add a proxy'));
  await type(field('Host'), 'me@proxy');
  await type(field('Port'), '8080');
  await click(button('Save'));
  assert.equal(saves().length, 1);
  assert.deepEqual(statuses(), [
    'Enter only the proxy’s name or address, such as proxy.example.com.',
  ]);
  assert.ok(form(), 'still open');
  assert.equal(field('Host').value, 'me@proxy');
  assert.equal(field('Port').value, '8080');
  assert.equal(proxyRows().length, 0);
});

test('without a keychain, the file is unlocked here and the proxy saved after', async () => {
  app({}, { store: { kind: 'file', unlocked: false } });
  const save = backend.network_save;
  backend.network_save = (args) => {
    if (args.passwords && kept.store?.kind === 'file' && !kept.store.unlocked) {
      throw 'The saved passwords and keys are locked: enter their passphrase first.';
    }
    return save(args);
  };
  backend.hosts_unlock = ({ passphrase }) => {
    assert.equal(passphrase, 'open sesame');
    kept = { ...kept, store: { kind: 'file', unlocked: true } };
  };
  await page();
  await click(button('+ Add a proxy'));
  assert.doesNotMatch(text(form()), /no keychain/, 'nothing to keep yet');
  await type(field('Host'), 'proxy.example.com');
  await type(field('Port'), '3128');
  await type(field('Username'), 'alice');
  assert.match(text(form()), /This computer has no keychain/);
  await type(field('Password'), 's3cret');
  await click(button('Save'));
  assert.ok(
    statuses().includes('The saved passwords and keys are locked: enter their passphrase first.'),
    statuses().join(' | ')
  );
  assert.equal(proxyRows().length, 0, 'nothing saved');
  assert.equal(field('Password').value, 's3cret', 'kept to save once unlocked');

  const passphrase = document.querySelector<HTMLInputElement>(
    'input[aria-label="passphrase of the secrets file"]'
  )!;
  await type(passphrase, 'open sesame');
  await click(button('Unlock'));
  const tries = saves();
  assert.equal(tries.length, 2, 'refused, then saved once unlocked');
  const id = tries[1].settings.proxies[0].id;
  assert.equal(tries[0].settings.proxies[0].id, id, 'the same proxy');
  assert.deepEqual(tries[1].passwords, { [id]: 's3cret' });
  assert.equal(form(), null);
  assert.equal(kept.settings.proxies[0].passwordSaved, true);
  assert.deepEqual(said(proxyRows()[0]), ['HTTP proxy.example.com:3128', 'Signs in as alice']);
});

test('a proxy’s URL pasted in the host field gives its kind and port', async () => {
  app();
  await page();
  await click(button('+ Add a proxy'));
  await enter(field('Host'), 'socks5://10.0.0.2:1080/');
  assert.equal(text(document.querySelector('.network-proxy-kind [aria-checked="true"]')), 'SOCKS5');
  assert.equal(field('Host').value, '10.0.0.2');
  assert.equal(field('Port').value, '1080');
  assert.deepEqual(saves(), []);
  await click(button('Save'));
  assert.deepEqual(saves()[0].settings.proxies[0].kind, 'socks5');
});

test('the timeout is saved as it changes', async () => {
  app();
  await page();
  await enter(timeoutField(), '60');
  assert.deepEqual(saves().at(-1), {
    settings: { proxies: [], routes: DEFAULT_ROUTES, timeoutSecs: 60 },
    passwords: undefined,
  });
  await enter(timeoutField(), '');
  assert.equal(saves().at(-1)!.settings.timeoutSecs, undefined, 'empty: the default');
});
