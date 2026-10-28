// Component tests: Settings → MCP servers (McpServersSettings.tsx). A
// server added with its command and env vars, its token saved in the
// keychain and never in the settings; one reached by URL tested with what's
// typed and its tools picked; a failed test shown with the server's own
// words; a name that can't be saved refused with why; servers pasted as
// JSON; a server removed, its secrets forgotten; turned off from the list;
// the secrets file unlocked here; the form's kind switched and back.
// The app is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import McpServersSettings, { type McpServersSettingsController } from './McpServersSettings';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { McpServer, McpTested } from '../lib/mcpServers';
import { emptyHost, type HostRow, type StoreKind } from '../lib/hosts';
import type { SettingsPage } from '../lib/appSettings';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
const listeners: { event: string; handler: number }[] = [];
const callbacks = new Map<number, (e: unknown) => void>();
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
/** The app sends `event`: every listener for it hears. */
function emit(event: string, payload: unknown) {
  for (const l of listeners.filter((l) => l.event === event)) {
    callbacks.get(l.handler)!({ event, id: 1, payload });
  }
}
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (cb: (e: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, cb);
    return id;
  },
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, handler: args.handler });
      return args.handler;
    }
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
afterEach(() => {
  dispose();
  document.body.innerHTML = '';
});

const github: McpServer = {
  id: 'gh',
  name: 'github',
  enabled: true,
  transport: {
    kind: 'command',
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-github'],
    env: [{ name: 'GITHUB_TOKEN', value: '', secret: true, saved: true }],
  },
  disabledTools: [],
};

const docs: McpServer = {
  id: 'docs-id',
  name: 'docs',
  enabled: true,
  transport: {
    kind: 'url',
    url: 'https://docs.example.com/mcp',
    headers: [{ name: 'Authorization', value: '', secret: true, saved: true }],
  },
  disabledTools: [],
};

const TESTED: McpTested = {
  server: 'docs 1.0',
  protocol: '2025-11-25',
  tools: [
    { name: 'read', description: 'Read a page of the docs.\nAny page.', readOnly: true },
    { name: 'write', description: 'Write a page.', readOnly: false },
  ],
};

/** The page with these servers, its secrets where `store` says, and the confirm dialogs. */
async function page(
  servers: McpServer[] = [],
  store: StoreKind = { kind: 'keychain' },
  hosts: HostRow[] = []
) {
  calls = [];
  backend = {
    mcp_store: () => store,
    hosts_list: () => ({ hosts, store, build: undefined }),
    mcp_save_secrets: () => null,
  };
  const [current, setCurrent] = createSignal(servers);
  const opened: SettingsPage[] = [];
  let api: McpServersSettingsController | undefined;
  dispose = render(
    () => (
      <>
        <McpServersSettings
          servers={current()}
          onChange={setCurrent}
          rootPath="/home/me/project"
          onOpenPage={(p) => opened.push(p)}
          controller={(c) => (api = c)}
        />
        <ConfirmDialogHost />
      </>
    ),
    container()
  );
  await flush();
  return { current, opened, api: () => api! };
}

/** A deployed host, in this state, whose daemon serves these. */
const hostRow = (id: string, state: HostRow['state'], serves: string[]): HostRow => ({
  ...emptyHost(),
  id,
  name: id,
  deploy: { state: 'deployed', version: '0.2.0' },
  state,
  chats: 0,
  passwordSaved: false,
  passphraseSaved: false,
  serves,
});

const text = (el: Element | null | undefined) => el?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
const button = (label: string, within: ParentNode = document) =>
  [...within.querySelectorAll<HTMLButtonElement>('button')].find((b) => text(b) === label);
const field = (label: string) =>
  document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
const valueRows = () => [...document.querySelectorAll('.mcp-value-row')];
const serverRows = () => [...document.querySelectorAll('.mcp-server-row')];
/** What a server's row says: its name, then each line under it. */
const said = (r: Element) =>
  [...r.querySelectorAll('.profile-row-label, .profile-row-sub')].map(text);
/** Each tool listed: its name, its read-only mark, the start of its description. */
const toolsShown = () =>
  [...document.querySelectorAll('.mcp-tool')].map((t) => [...t.querySelectorAll('span')].map(text));
const statuses = () => [...document.querySelectorAll('.settings-status')].map(text);
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

async function type(input: HTMLInputElement | HTMLTextAreaElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

async function tick(box: HTMLInputElement, on: boolean) {
  box.checked = on;
  box.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
}

async function click(b: HTMLButtonElement | undefined) {
  assert.ok(b, 'the button is there');
  b.click();
  await flush();
}

/** A value row's name, value and Secret box. */
function row(i: number) {
  const r = valueRows()[i];
  const [name, value] = [...r.querySelectorAll<HTMLInputElement>('input:not([type="checkbox"])')];
  const secret = r.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
  return { name, value, secret };
}

test('a command server is added with its env vars, its token in the keychain and never in the settings', async () => {
  const { current, api } = await page();
  await click(button('+ Add a server'));
  assert.equal(api().draftTitle(), 'New MCP server');
  await type(field('Name'), 'github');
  assert.equal(api().draftTitle(), 'github');
  await type(field('Command'), 'npx');
  await type(field('Arguments'), '-y\n@modelcontextprotocol/server-github\n');
  await click(button('+ Add a variable'));
  assert.equal(row(0).secret.checked, true, 'a secret unless it’s said not to be');
  assert.equal(row(0).value.type, 'password');
  assert.equal(row(0).value.placeholder, 'Not saved yet');
  await type(row(0).name, 'GITHUB_TOKEN');
  await type(row(0).value, 'ghp_secret');
  await click(button('+ Add a variable'));
  await type(row(1).name, 'LOG_LEVEL');
  await tick(row(1).secret, false);
  assert.equal(row(1).value.type, 'text');
  await type(row(1).value, 'debug');
  await click(button('Save'));

  const [saved] = current();
  assert.deepEqual(sent('mcp_save_secrets'), [
    { server: saved.id, values: { 'env/GITHUB_TOKEN': 'ghp_secret' } },
  ]);
  assert.deepEqual(saved, {
    id: saved.id,
    name: 'github',
    enabled: true,
    transport: {
      kind: 'command',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-github'],
      env: [
        { name: 'GITHUB_TOKEN', value: '', secret: true, saved: true },
        { name: 'LOG_LEVEL', value: 'debug', secret: false },
      ],
    },
    disabledTools: [],
  });
  assert.doesNotMatch(JSON.stringify(current()), /ghp_secret/);
  assert.equal(api().draftTitle(), null, 'back on the list');
  assert.deepEqual(serverRows().map(said), [
    ['github', 'npx -y @modelcontextprotocol/server-github'],
  ]);
});

test('a URL server is tested with what’s typed, its tools picked, and its saved token kept', async () => {
  const { current } = await page([docs]);
  backend.mcp_test = () => TESTED;
  await click(button('Edit'));
  assert.equal(row(0).value.placeholder, 'Saved: leave empty to keep it');
  await click(button('Test'));
  assert.deepEqual(sent('mcp_test'), [
    { server: docs, values: {}, root: '/home/me/project', host: null },
  ]);
  assert.match(
    text(document.querySelector('.provider-test-status.ok')),
    /Connected to docs 1\.0: 2 tools, MCP 2025-11-25/
  );
  assert.deepEqual(toolsShown(), [
    ['read', 'read-only', 'Read a page of the docs.'],
    ['write', 'Write a page.'],
  ]);
  const tools = [...document.querySelectorAll('.mcp-tool')];
  await tick(tools[1].querySelector('input')!, false);

  // A new token typed is what the next test uses.
  await type(row(0).value, 'Bearer new');
  assert.equal(
    document.querySelector('.provider-test-status.ok'),
    null,
    'the last test no longer says'
  );
  assert.deepEqual(toolsShown(), [['write']], 'still off');
  await click(button('Test'));
  assert.deepEqual(sent('mcp_test')[1].values, { 'header/Authorization': 'Bearer new' });
  await type(row(0).value, '');
  await click(button('Save'));
  assert.deepEqual(sent('mcp_save_secrets'), [], 'nothing typed: nothing to change');
  assert.deepEqual(current(), [{ ...docs, disabledTools: ['write'] }]);
  assert.deepEqual(said(serverRows()[0]), [
    'docs',
    'https://docs.example.com/mcp',
    '1 tool turned off',
  ]);
});

test('a failed test shows the server’s own words, and a change forgets it', async () => {
  await page([github]);
  backend.mcp_test = () => {
    throw 'npx couldn’t start the server: it exited (1)\nnpm ERR! 404 Not Found';
  };
  await click(button('Edit'));
  await click(button('Test'));
  const error = document.querySelector('.mcp-test-error')!;
  assert.equal(
    error.textContent,
    'npx couldn’t start the server: it exited (1)\nnpm ERR! 404 Not Found'
  );
  await type(field('Command'), 'bunx');
  assert.equal(document.querySelector('.mcp-test-error'), null);

  // What can't start is said without asking the app.
  await type(field('Command'), ' ');
  await click(button('Test'));
  assert.equal(
    text(document.querySelector('.mcp-test-error')),
    'Give the command that starts the server.'
  );
  assert.equal(sent('mcp_test').length, 1);
});

test('a server that can’t be saved says why, and keeps its form and its secrets unsent', async () => {
  const { current } = await page([github]);
  await click(button('+ Add a server'));
  await type(field('Command'), 'uvx');
  await click(button('+ Add a variable'));
  await type(row(0).name, 'API_KEY');
  await type(row(0).value, 'k-1');
  await click(button('Save'));
  assert.deepEqual(statuses(), ['Give the server a name.']);
  await type(field('Name'), 'github');
  await click(button('Save'));
  assert.deepEqual(statuses(), ['Another server is named github.']);
  assert.deepEqual(sent('mcp_save_secrets'), []);
  assert.deepEqual(current(), [github]);
  assert.equal(row(0).value.value, 'k-1', 'kept to save once it can be');

  // A keychain that refuses the secrets leaves the settings as they were.
  backend.mcp_save_secrets = () => {
    throw 'The saved passwords and keys: the keychain said no';
  };
  await type(field('Name'), 'keys');
  await click(button('Save'));
  assert.deepEqual(statuses(), ['The saved passwords and keys: the keychain said no']);
  assert.deepEqual(current(), [github]);
});

test('JSON pasted from a setup adds its server, saves its token and opens its form', async () => {
  const { current, api } = await page([github]);
  await click(button('+ Add from JSON'));
  await type(field('Servers as JSON'), '{ nope');
  await click(button('Add'));
  assert.match(statuses()[0], /That isn’t JSON/);
  assert.deepEqual(current(), [github]);

  const setup = {
    mcpServers: {
      github: {
        command: 'npx',
        args: ['-y', '@modelcontextprotocol/server-github'],
        env: { GITHUB_PERSONAL_ACCESS_TOKEN: 'ghp_123' },
      },
    },
  };
  await type(field('Servers as JSON'), JSON.stringify(setup));
  await click(button('Add'));
  const added = current()[1];
  assert.equal(added.name, 'github-2');
  assert.deepEqual(sent('mcp_save_secrets'), [
    { server: added.id, values: { 'env/GITHUB_PERSONAL_ACCESS_TOKEN': 'ghp_123' } },
  ]);
  assert.doesNotMatch(JSON.stringify(current()), /ghp_123/);
  assert.equal(api().draftTitle(), 'github-2', 'its form open, to finish and test');

  // Two at once are added to the list, and no form opens.
  api().closeDraft();
  await flush();
  await click(button('+ Add from JSON'));
  const two = {
    mcpServers: { time: { command: 'uvx', args: ['mcp-server-time'] }, fetch: { command: 'uvx' } },
  };
  await type(field('Servers as JSON'), JSON.stringify(two));
  await click(button('Add'));
  assert.deepEqual(
    current().map((s) => s.name),
    ['github', 'github-2', 'time', 'fetch']
  );
  assert.equal(api().draftTitle(), null);
  assert.equal(field('Servers as JSON'), null, 'the box closed');
});

test('removing a server forgets its secrets, once confirmed', async () => {
  const { current } = await page([github, docs]);
  await click(button('Remove', serverRows()[0]));
  assert.match(
    text(document.querySelector('.confirm-dialog')),
    /github will be removed from Rovik, and the secret values saved for it forgotten/
  );
  await click(button('Cancel', document.querySelector('.confirm-dialog')!));
  assert.equal(current().length, 2);
  assert.deepEqual(sent('mcp_save_secrets'), []);

  await click(button('Remove', serverRows()[0]));
  await click(button('Remove', document.querySelector('.confirm-dialog')!));
  assert.deepEqual(sent('mcp_save_secrets'), [
    { server: 'gh', values: { 'env/GITHUB_TOKEN': '' } },
  ]);
  assert.deepEqual(current(), [docs]);
});

test('a server is turned off and on again from the list, its settings kept', async () => {
  const { current } = await page([github]);
  await tick(field('Use github'), false);
  assert.deepEqual(current(), [{ ...github, enabled: false }]);
  await tick(field('Use github'), true);
  assert.deepEqual(current(), [github]);
});

test('a secret without a value saved is said on the list', async () => {
  const unsaved: McpServer = {
    ...github,
    transport: {
      kind: 'command',
      command: 'npx',
      args: [],
      env: [{ name: 'GITHUB_TOKEN', value: '', secret: true }],
    },
  };
  await page([unsaved]);
  assert.deepEqual(said(serverRows()[0]), [
    'github',
    'npx',
    'No value saved for GITHUB_TOKEN: Edit to enter it',
  ]);
});

test('without a keychain, the secrets file is unlocked here', async () => {
  let store: StoreKind = { kind: 'file', unlocked: false };
  await page([github], store);
  backend.mcp_store = () => store;
  backend.hosts_unlock = ({ passphrase }) => {
    assert.equal(passphrase, 'open sesame');
    store = { kind: 'file', unlocked: true };
  };
  assert.match(text(document.body), /This computer has no keychain/);
  await type(field('passphrase of the secrets file'), 'open sesame');
  await click(button('Unlock'));
  assert.doesNotMatch(text(document.body), /This computer has no keychain/);
});

test('the form switches between a command and a URL, keeping what was typed for each', async () => {
  const { opened } = await page();
  await click(button('Permissions & approval'));
  assert.deepEqual(opened, ['permissions']);
  await click(button('+ Add a server'));
  await type(field('Command'), 'npx');
  await click(button('URL'));
  assert.equal(document.querySelector('[aria-label="Command"]'), null);
  await type(field('URL'), 'https://mcp.example.com/mcp');
  await click(button('+ Add a header'));
  await click(button('Command'));
  assert.equal(field('Command').value, 'npx');
  assert.equal(valueRows().length, 0, 'the headers are the URL’s');
  await click(button('URL'));
  assert.equal(field('URL').value, 'https://mcp.example.com/mcp');
  assert.equal(valueRows().length, 1);
});

const testOn = () => document.querySelector<HTMLSelectElement>('select[aria-label="Test on"]');

test('a server is tested on the host chosen in Test on, and choosing clears the last result', async () => {
  const hosts = [
    hostRow('build', 'ready', ['mcp']),
    hostRow('old', 'ready', []),
    hostRow('away', 'offline', ['mcp']),
  ];
  const {} = await page([docs], { kind: 'keychain' }, hosts);
  backend.mcp_test = () => TESTED;
  await click(button('Edit'));
  assert.deepEqual(
    [...testOn()!.options].map((o) => [o.value, text(o)]),
    [
      ['', 'This computer'],
      ['build', 'build'],
    ]
  );
  await click(button('Test'));
  assert.equal(sent('mcp_test')[0].host, null, 'here until another place is chosen');
  assert.ok(document.querySelector('.provider-test-status.ok'));
  testOn()!.value = 'build';
  testOn()!.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal(
    document.querySelector('.provider-test-status'),
    null,
    'the result was of another place'
  );
  await click(button('Test'));
  assert.equal(sent('mcp_test')[1].host, 'build');
});

test('there is no Test on choice without a host that runs MCP servers', async () => {
  await page([docs], { kind: 'keychain' }, [
    hostRow('old', 'ready', []),
    hostRow('away', 'offline', ['mcp']),
  ]);
  await click(button('Edit'));
  assert.equal(testOn(), null);
});

test('a host that drops out after it was chosen is no longer tested on', async () => {
  const build = hostRow('build', 'ready', ['mcp']);
  await page([docs], { kind: 'keychain' }, [build]);
  backend.mcp_test = () => TESTED;
  await click(button('Edit'));
  testOn()!.value = 'build';
  testOn()!.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  backend.hosts_list = () => ({
    hosts: [{ ...build, state: 'offline' }],
    store: { kind: 'keychain' },
  });
  emit('cloud-event', { type: 'host', host: 'build', state: 'offline' });
  await flush();
  assert.equal(testOn(), null, 'nothing but this computer is left to choose');
  await click(button('Test'));
  assert.equal(sent('mcp_test')[0].host, null);
});
