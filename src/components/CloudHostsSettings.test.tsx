// Component tests: Settings → Cloud hosts (U01), its states (not tested,
// testing, a failed step, needs someone with root, deployed, upgrade
// available, an administrator's view), and that its key list never shows a
// key. The app is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import CloudHostsSettings, { type HostsSettingsController } from './CloudHostsSettings';
import { taskLine as hostTaskLine } from './HostTasks';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { HostDetails, HostRow, HostsList, Step } from '../lib/hosts';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
/** The commands the page sent, in order. */
let calls: { cmd: string; args: any }[] = [];

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
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
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

/** The app sends `event`: every listener for it hears. */
function emit(event: string, payload: unknown) {
  for (const l of listeners.filter((l) => l.event === event)) {
    callbacks.get(l.handler)!({ event, id: 1, payload });
  }
}

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const HOST: HostRow = {
  id: 'h1',
  name: 'Build server',
  host: 'build.example.com',
  port: 22,
  user: 'dev',
  auth: 'key',
  keyPath: '~/.ssh/id_ed25519',
  approvalPolicy: 'waitForMe',
  deploy: { state: 'notDeployed' },
  chats: 0,
  passwordSaved: false,
  passphraseSaved: false,
  serves: [],
};

const DEPLOYED: Partial<HostRow> = {
  deploy: { state: 'deployed', version: '0.2.0', sha256: 'ab12' },
  state: 'ready',
};

function hostsList(row: Partial<HostRow> = {}): HostsList {
  return { hosts: [{ ...HOST, ...row }], store: { kind: 'keychain' }, build: '/opt/rovik-agentd' };
}

const DETAILS: HostDetails = {
  info: {
    version: '0.2.0',
    protocol: 3,
    minProtocol: 1,
    os: 'linux',
    arch: 'x86_64',
    user: 'dev',
    virtualFiles: false,
    userNamespaces: true,
    install: { admins: ['alice', 'bob'] },
    otherDaemons: 1,
  },
  keys: [{ provider: 'OpenAI', baseUrl: 'https://api.openai.com/v1' }],
  storage: { projects: 3 * 1024 ** 3, work: 512 * 1024 ** 2, data: 20 * 1024 ** 2, tmp: 0 },
  admin: false,
  upgrade: false,
  appVersion: '0.2.0',
  cleanUp: true,
  chatStorage: false,
};

const step = (
  check: Step['check'],
  outcome: Step['outcome'],
  message = `${check}: ${outcome}`
) => ({
  check,
  outcome,
  message,
});

const button = (root: ParentNode, label: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as
    HTMLButtonElement | undefined;

const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** An answer the test gives when it chooses. */
function later<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

function type(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

interface Page {
  root: HTMLElement;
  api: HostsSettingsController;
}

/** The page rendered last: gone when the next one renders. */
let disposePage = () => {};

/** Renders the page on the list, with the app answering as `answers` says. */
async function hostsPage(list: HostsList, answers: typeof backend = {}): Promise<Page> {
  disposePage();
  calls = [];
  backend = { hosts_list: () => list, hosts_summaries: () => [], ...answers };
  const root = container();
  let api: HostsSettingsController | undefined;
  disposePage = render(
    () => (
      <>
        <CloudHostsSettings controller={(controller) => (api = controller)} />
        <ConfirmDialogHost />
      </>
    ),
    root
  );
  await flush();
  return { root, api: api! };
}

/** Renders the page and opens its one host. */
async function hostPage(list: HostsList, answers: typeof backend = {}): Promise<Page> {
  const page = await hostsPage(list, answers);
  button(page.root, 'Open')!.click();
  await flush();
  return page;
}

test('the list: each host’s address, status and chats, and where secrets are kept', async () => {
  const list: HostsList = {
    hosts: [
      { ...HOST, ...DEPLOYED, chats: 2 },
      { ...HOST, id: 'h2', name: 'Spare', host: '203.0.113.7', port: 2222, state: undefined },
    ],
    store: { kind: 'file', unlocked: false },
  };
  const { root, api } = await hostsPage(list);
  const rows = [...root.querySelectorAll('.host-row')].map((r) => r.textContent);
  assert.equal(rows.length, 2);
  assert.match(rows[0]!, /Build server.*dev@build\.example\.com.*Connected · 2 chats/);
  assert.match(rows[1]!, /Spare.*dev@203\.0\.113\.7:2222.*Not deployed/);
  assert.match(root.textContent!, /encrypted under a passphrase.*The file is locked/);
  assert.equal(api.title(), null, 'the list is the page itself');
});

test('not tested: the host’s page offers a test and a deploy, and says nothing of a test', async () => {
  const { root, api } = await hostPage(hostsList());
  assert.equal(api.title(), 'Build server');
  assert.match(root.textContent!, /dev@build\.example\.com · Not deployed/);
  assert.ok(button(root, 'Test connection'));
  assert.ok(button(root, 'Deploy'));
  assert.equal(root.querySelector('.host-steps'), null);
  assert.deepEqual(sent('hosts_test'), []);
  assert.deepEqual(sent('hosts_details'), [], 'nothing to ask a server Rovik isn’t on');
  // Back goes to the list.
  api.back();
  await flush();
  assert.ok(button(root, '+ Add host'));
});

test('not deployed, and no build to deploy: says so instead of offering it', async () => {
  const { root } = await hostPage({ ...hostsList(), build: undefined });
  assert.equal(button(root, 'Deploy'), undefined);
  assert.match(root.textContent!, /There’s no build to deploy: this build of Rovik has no release/);
  assert.match(root.textContent!, /Set ROVIK_AGENTD_BINARY/);
});

test('testing: each step shows as it ends, then the verdict', async () => {
  let finish: (steps: Step[]) => void = () => {};
  const { root } = await hostPage(hostsList(), {
    hosts_test: () => new Promise((resolve) => (finish = resolve)),
  });
  button(root, 'Test connection')!.click();
  await flush();
  // Only the host goes: the test tries no provider from the server.
  assert.deepEqual(sent('hosts_test'), [{ id: 'h1' }]);
  assert.equal(button(root, 'Testing…')?.disabled, true);
  emit('host-test', { hostId: 'h1', step: step('address', 'passed') });
  emit('host-test', { hostId: 'other', step: step('disk', 'failed') });
  await flush();
  const shown = () => [...root.querySelectorAll('.host-step')].map((s) => s.textContent);
  assert.equal(shown().length, 1, 'another host’s test isn’t this one’s');
  assert.match(shown()[0]!, /Address/);
  finish([step('address', 'passed'), step('host-key', 'passed'), step('log-in', 'passed')]);
  await flush();
  assert.equal(shown().length, 3);
  assert.equal(root.querySelector('.host-test .host-status')?.textContent, 'Everything works');
  assert.ok(button(root, 'Test connection'));
});

test('a failed step: the verdict names it, and the step says why', async () => {
  const { root } = await hostPage(hostsList(), {
    hosts_test: async () => [
      step('address', 'passed'),
      step('log-in', 'failed', 'The server refused the key.'),
      step('system', 'skipped', 'Not tried: no log-in.'),
    ],
  });
  button(root, 'Test connection')!.click();
  await flush();
  assert.equal(root.querySelector('.host-test .host-status')?.textContent, 'Failed: Log-in');
  assert.match(
    root.querySelector('.host-step.failed')!.textContent!,
    /The server refused the key\./
  );
  assert.ok(root.querySelector('.host-step.skipped'));
});

test('a new server’s key: compared, trusted, then tested again', async () => {
  const key = { key: 'ssh-ed25519 AAAAC3Nza', fingerprint: 'SHA256:q4bVnQ' };
  let tests = 0;
  const { root } = await hostPage(hostsList(), {
    hosts_test: async () =>
      ++tests === 1
        ? [step('address', 'passed'), { ...step('host-key', 'confirm'), hostKey: key }]
        : [step('address', 'passed'), step('host-key', 'passed')],
    hosts_trust_key: () => null,
  });
  button(root, 'Test connection')!.click();
  await flush();
  assert.equal(
    root.querySelector('.host-test .host-status')?.textContent,
    'Check the server’s key'
  );
  assert.equal(root.querySelector('.host-key-confirm code')?.textContent, 'SHA256:q4bVnQ');
  button(root, 'Trust this key')!.click();
  await flush();
  assert.deepEqual(sent('hosts_trust_key'), [{ id: 'h1', key: 'ssh-ed25519 AAAAC3Nza' }]);
  assert.equal(tests, 2);
  assert.equal(root.querySelector('.host-key-confirm'), null);
});

test('a build that changed on the server: compared, trusted, then tested again', async () => {
  const build = { sha256: 'cd'.repeat(32), version: '0.3.0' };
  let tests = 0;
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_test: async () =>
      ++tests === 1
        ? [step('install', 'passed'), { ...step('build', 'confirm'), build }]
        : [step('install', 'passed'), step('build', 'passed')],
    hosts_trust_build: () => null,
  });
  button(root, 'Test connection')!.click();
  await flush();
  assert.equal(
    root.querySelector('.host-test .host-status')?.textContent,
    'Check Rovik’s build there'
  );
  assert.equal(root.querySelector('.host-build-confirm code')?.textContent, build.sha256);
  button(root, 'Trust this build')!.click();
  await flush();
  assert.deepEqual(sent('hosts_trust_build'), [{ id: 'h1', sha256: build.sha256 }]);
  assert.equal(tests, 2);
  assert.equal(root.querySelector('.host-build-confirm'), null);
});

test('a deployed host the app can’t reach says how to find out why', async () => {
  const { root } = await hostPage(hostsList({ ...DEPLOYED, state: 'offline' }));
  assert.match(root.textContent!, /Test the connection to see why/);
});

test('needs someone with root: the command to run is shown', async () => {
  const command = 'sudo /tmp/rovik-agentd-0.2.0 install --admin dev';
  const { root } = await hostPage(hostsList(), {
    hosts_deploy: () => {
      throw { kind: 'needsRoot', command, message: 'Installing Rovik takes root.' };
    },
  });
  button(root, 'Deploy')!.click();
  await flush();
  assert.deepEqual(sent('hosts_deploy'), [{ id: 'h1', sudoPassword: null }]);
  assert.equal(root.querySelector('.host-command')?.textContent, command);
  assert.match(root.textContent!, /Ask someone with\s+root to run this/);
  assert.ok(button(root, 'Deploy'), 'and deploy again once they have');
});

test('sudo asks for a password: it goes with the next deploy only', async () => {
  let deploys = 0;
  const { root } = await hostPage(hostsList(), {
    hosts_deploy: () => {
      if (++deploys === 1) throw { kind: 'sudoPassword', message: 'sudo needs a password.' };
      return '0.2.0';
    },
  });
  button(root, 'Deploy')!.click();
  await flush();
  const password = root.querySelector('input[aria-label="sudo password"]') as HTMLInputElement;
  assert.ok(password);
  type(password, 'hunter2');
  await flush();
  button(root.querySelector('.host-sudo')!, 'Deploy')!.click();
  await flush();
  assert.deepEqual(sent('hosts_deploy')[1], { id: 'h1', sudoPassword: 'hunter2' });
  assert.equal(root.querySelector('.host-sudo'), null);
  assert.ok(!root.innerHTML.includes('hunter2'), 'the password isn’t kept on the page');
});

test('deployed: its version, what it stores, and the providers whose keys it keeps', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => DETAILS });
  assert.deepEqual(sent('hosts_details'), [{ id: 'h1' }]);
  const text = root.textContent!;
  assert.match(text, /Connected/);
  assert.match(text, /Version 0\.2\.0\. Its administrators: alice, bob\./);
  assert.doesNotMatch(text, /You’re one of them/);
  assert.match(text, /Projects: 3 GB/);
  assert.match(text, /Chats’ working copies: 512 MB/);
  assert.match(
    root.querySelector('.host-keys-list')!.textContent!,
    /OpenAI https:\/\/api\.openai\.com\/v1/
  );
  assert.ok(button(root, 'Remove keys from this server'));
  assert.match(
    root.querySelector('.host-keys .settings-hint')!.textContent!.replace(/\s+/g, ' '),
    /^Your API keys and MCP servers’ secrets are stored in your account on this server, so chats there run while this computer is away\. The agent’s commands there and anyone with root on it can read them\.$/
  );
  assert.equal(button(root, 'Upgrade for everyone'), undefined);
  assert.equal(root.querySelector('.host-limits'), null, 'limits are for administrators');
});

test('deployed: one line names the connectors installed there, comma separated, or none', async () => {
  const line = async (connectors?: string[]) => {
    const details = { ...DETAILS, info: { ...DETAILS.info, connectors } };
    const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => details });
    return root.querySelector('.host-connectors')?.textContent ?? null;
  };
  assert.equal(await line(['claude-code']), 'Connectors installed: Claude Code.');
  assert.equal(await line(['claude-code', 'codex']), 'Connectors installed: Claude Code, Codex.');
  assert.equal(
    await line(['claude-code', 'aider']),
    'Connectors installed: Claude Code, aider.',
    'a newer daemon’s kind, by its id'
  );
  assert.equal(await line([]), 'Connectors installed: none.');
  assert.equal(await line(undefined), null, 'a daemon that can’t say');
});

test('“Clean up now”: what the server no longer needs goes, and it’s measured again', async () => {
  const after = { ...DETAILS.storage!, work: 128 * 1024 ** 2 };
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => DETAILS,
    hosts_clean_up: () => ({ storage: after, freed: 384 * 1024 ** 2 }),
  });
  assert.match(root.textContent!, /and temporary files unchanged for a day go each day/);
  assert.match(root.textContent!, /only temporary files changed in the last hour stay/);
  button(root, 'Clean up now')!.click();
  await flush();
  assert.deepEqual(sent('hosts_clean_up'), [{ id: 'h1' }]);
  assert.match(root.textContent!, /Chats’ working copies: 128 MB/);
  assert.match(root.textContent!, /384 MB freed\./);
});

const MB = 1024 ** 2;
const CHAT_STORAGE: HostDetails = {
  ...DETAILS,
  chatStorage: true,
  storage: {
    ...DETAILS.storage!,
    chats: [
      {
        chat: 'c2',
        title: 'Port to Tauri',
        source: 15 * MB,
        dependencies: 316 * MB,
        build: 1600 * MB,
        other: 0,
        running: true,
        keepReady: false,
        activeMs: 0,
      },
      {
        chat: 'c1',
        title: '',
        source: 2 * MB,
        dependencies: 0,
        build: 0,
        other: MB,
        running: false,
        keepReady: true,
        activeMs: 0,
      },
    ],
  },
};

test('the server’s chats are listed by size with what each holds', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => CHAT_STORAGE });
  const rows = [...root.querySelectorAll('.host-chat-storage-table tbody tr')].map((row) =>
    [...row.querySelectorAll('td')].map((cell) => cell.textContent)
  );
  assert.deepEqual(
    rows.map((cells) => cells.slice(0, 6)),
    [
      ['Port to TauriRunning', '1.9 GB', '15 MB', '316 MB', '1.6 GB', '0 bytes'],
      ['c1', '3 MB', '2 MB', '0 bytes', '0 bytes', '1 MB'],
    ]
  );
  assert.match(
    root.textContent!,
    /“Clean up now” also removes installed dependencies and build output of chats that aren’t running or kept ready\. Their conversations and changes stay; the next run reinstalls and rebuilds\./
  );
});

test('an older server shows only the totals', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => DETAILS });
  assert.match(root.textContent!, /Chats’ working copies: 512 MB/);
  assert.equal(root.querySelector('.host-chat-storage'), null);
  assert.doesNotMatch(root.textContent!, /also removes installed dependencies/);
});

test('keep ready toggles one chat, and a budget is set in GB, or none', async () => {
  const fitted = { ...CHAT_STORAGE.storage!, budget: 20 * 1024 ** 3 };
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => CHAT_STORAGE,
    hosts_keep_ready: () => undefined,
    hosts_storage_budget: () => fitted,
  });
  const keep = root.querySelector(
    'input[aria-label="Keep Port to Tauri ready"]'
  ) as HTMLInputElement;
  keep.click();
  await flush();
  assert.deepEqual(sent('hosts_keep_ready'), [{ id: 'h1', chat: 'c2', keep: true }]);

  const budget = root.querySelector('#host-storage-budget') as HTMLInputElement;
  type(budget, '20');
  button(root, 'Save budget')!.click();
  await flush();
  type(budget, '');
  button(root, 'Save budget')!.click();
  await flush();
  assert.deepEqual(sent('hosts_storage_budget'), [
    { id: 'h1', bytes: 20 * 1024 ** 3 },
    { id: 'h1', bytes: null },
  ]);
  for (const wrong of ['-3', '0.5', '2e', '1,5']) {
    type(budget, wrong);
    await flush();
    assert.ok(button(root, 'Save budget')!.disabled, `not a size of 1 GB or more: ${wrong}`);
  }
});

test('keep ready that the server refused unticks again', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => CHAT_STORAGE,
    hosts_keep_ready: () => {
      throw new Error('This chat has no working copy on this server.');
    },
  });
  const keep = root.querySelector(
    'input[aria-label="Keep Port to Tauri ready"]'
  ) as HTMLInputElement;
  keep.click();
  await flush();
  assert.equal(keep.checked, false);
  assert.match(root.textContent!, /no working copy on this server/);
});

test('“Clean up now” that freed nothing says so', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => DETAILS,
    hosts_clean_up: () => ({ storage: DETAILS.storage, freed: 0 }),
  });
  button(root, 'Clean up now')!.click();
  await flush();
  assert.match(root.textContent!, /Nothing more to clean up\./);
});

test('a server whose Rovik can’t clean up when asked offers no “Clean up now”', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => ({ ...DETAILS, cleanUp: false }),
  });
  assert.match(root.textContent!, /Projects: 3 GB/);
  assert.equal(button(root, 'Clean up now'), undefined);
});

test('the key list never shows a key, whatever the server sends', async () => {
  const leaky = {
    ...DETAILS,
    keys: [
      {
        provider: 'OpenAI',
        baseUrl: 'https://api.openai.com/v1',
        apiKey: 'sk-live-0001',
        key: 'sk-live-0001',
      },
    ],
  };
  const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => leaky });
  assert.equal(root.querySelectorAll('.host-keys-list li').length, 1);
  assert.ok(!root.innerHTML.includes('sk-live-0001'));
});

test('removing the server’s keys: confirmed first, then the list asked again', async () => {
  let keys = DETAILS.keys!;
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => ({ ...DETAILS, keys }),
    hosts_remove_keys: () => {
      const gone = keys.length;
      keys = [];
      return gone;
    },
  });
  button(root, 'Remove keys from this server')!.click();
  await flush();
  const confirm = document.querySelector(
    '.confirm-dialog-actions button.danger-filled'
  ) as HTMLButtonElement;
  assert.equal(confirm.textContent, 'Remove keys');
  confirm.click();
  await flush();
  assert.deepEqual(sent('hosts_remove_keys'), [{ id: 'h1' }]);
  assert.match(root.querySelector('.host-keys-list')!.textContent!, /No keys are kept there\./);
  assert.equal(button(root, 'Remove keys from this server'), undefined);
});

test('MCP servers’ secrets kept on the server are listed and can be removed, with no API key there', async () => {
  let mcpSecrets = 2;
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => ({ ...DETAILS, keys: [], mcpSecrets }),
    hosts_remove_keys: () => {
      const gone = mcpSecrets;
      mcpSecrets = 0;
      return gone;
    },
  });
  const list = root.querySelector('.host-keys-list')!.textContent!;
  assert.match(list, /MCP servers’ secrets 2 servers/);
  assert.doesNotMatch(list, /No keys are kept there/);
  button(root, 'Remove keys from this server')!.click();
  await flush();
  assert.match(
    document.querySelector('.confirm-dialog')!.textContent!,
    /forgets every API key and MCP server’s secret it keeps, and stops the MCP servers running there/
  );
  (
    document.querySelector('.confirm-dialog-actions button.danger-filled') as HTMLButtonElement
  ).click();
  await flush();
  assert.deepEqual(sent('hosts_remove_keys'), [{ id: 'h1' }]);
  assert.match(root.querySelector('.host-keys-list')!.textContent!, /No keys are kept there\./);
});

test('upgrade available: anyone but an administrator is told whom to ask', async () => {
  const older = { ...DETAILS, upgrade: true, info: { ...DETAILS.info, version: '0.1.0' } };
  const { root } = await hostPage(hostsList(DEPLOYED), { hosts_details: () => older });
  assert.match(
    root.querySelector('.host-upgrade')!.textContent!,
    /Rovik 0\.1\.0 here is older than this app \(0\.2\.0\)\. Ask alice or bob to upgrade it\./
  );
  assert.equal(button(root, 'Upgrade for everyone'), undefined);
  assert.match(root.textContent!, /Version 0\.1\.0\./, 'the daemon’s own, not the one kept');
});

test('upgrade available: an administrator upgrades for everyone', async () => {
  const older = {
    ...DETAILS,
    admin: true,
    upgrade: true,
    info: { ...DETAILS.info, version: '0.1.0' },
  };
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => older,
    hosts_upgrade: () => null,
  });
  assert.doesNotMatch(root.querySelector('.host-upgrade')!.textContent!, /Ask/);
  button(root, 'Upgrade for everyone')!.click();
  await flush();
  assert.deepEqual(sent('hosts_upgrade'), [{ id: 'h1' }]);
});

test('an administrator’s view: the limits for everyone, changed and saved', async () => {
  let limits: [string, string][] = [
    ['memory', '25%'],
    ['cpu', 'none'],
    ['slots', '4'],
  ];
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => ({ ...DETAILS, admin: true }),
    hosts_limits: () => limits,
    hosts_set_limits: ({ changes }: { changes: [string, string][] }) => {
      limits = limits.map(([name, value]) => [
        name,
        changes.find(([n]) => n === name)?.[1] ?? value,
      ]);
      return null;
    },
  });
  assert.match(root.textContent!, /Its administrators: alice, bob\. You’re one of them\./);
  assert.equal(button(root, 'Save limits'), undefined);
  button(root, 'Show the limits')!.click();
  await flush();
  const inputs = [...root.querySelectorAll('.host-limit input')] as HTMLInputElement[];
  assert.deepEqual(
    inputs.map((i) => i.value),
    ['25%', 'none', '4']
  );
  assert.equal(button(root, 'Save limits')!.disabled, true, 'nothing changed yet');
  type(inputs[0]!, ' 40% ');
  await flush();
  button(root, 'Save limits')!.click();
  await flush();
  assert.deepEqual(sent('hosts_set_limits'), [{ id: 'h1', changes: [['memory', '40%']] }]);
  assert.equal((root.querySelector('.host-limit input') as HTMLInputElement).value, '40%');
  assert.match(root.textContent!, /Each daemon takes the new limits when it next starts\./);
});

test('opening a connected host: a spinner till the server answers, what it fills hidden till then', async () => {
  const details = later<HostDetails>();
  const tasks = later<unknown[]>();
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => details.promise,
    hosts_tasks: () => tasks.promise,
  });
  assert.deepEqual(sent('hosts_details'), [{ id: 'h1' }]);
  assert.deepEqual(sent('hosts_tasks'), [{ id: 'h1' }], 'asked with the details, not after');
  const waiting = root.querySelector('.host-waiting');
  assert.ok(waiting?.querySelector('.host-spinner'), 'a spinner');
  assert.equal(waiting!.getAttribute('role'), 'status');
  const filled = ['.host-deploy', '.host-storage', '.host-tasks', '.host-keys'];
  for (const section of filled) assert.equal(root.querySelector(section), null, section);
  // What this computer knows of the host shows at once.
  assert.match(root.textContent!, /Connected/);
  assert.ok(root.querySelector('.host-test'));
  assert.ok(root.querySelector('.host-policy'));

  details.resolve(DETAILS);
  await flush();
  assert.ok(root.querySelector('.host-waiting'), 'the tasks aren’t in yet');
  assert.equal(root.querySelector('.host-storage'), null);

  tasks.resolve([]);
  await flush();
  assert.equal(root.querySelector('.host-waiting'), null);
  for (const section of filled) assert.ok(root.querySelector(section), section);
  assert.match(root.textContent!, /Version 0\.2\.0\. Its administrators: alice, bob\./);
  assert.doesNotMatch(root.textContent!, /Asking the server/);
});

test('the server’s details not coming: why, and no spinner left spinning', async () => {
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => {
      throw new Error('Build server didn’t answer in time.');
    },
    hosts_tasks: () => [],
  });
  assert.equal(root.querySelector('.host-waiting'), null);
  assert.match(root.textContent!, /Build server didn’t answer in time\./);
  assert.doesNotMatch(root.textContent!, /Asking the server/);
  assert.ok(root.querySelector('.host-deploy'), 'what this computer knows of it');
  assert.equal(root.querySelector('.host-storage'), null);
});

test('asked again, the server’s sections stay as they were till it answers', async () => {
  let answer: Promise<HostDetails> = Promise.resolve(DETAILS);
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => answer,
    hosts_tasks: () => [],
    hosts_remove_keys: () => 1,
  });
  const again = later<HostDetails>();
  answer = again.promise;
  button(root, 'Remove keys from this server')!.click();
  await flush();
  (document.querySelector('.confirm-dialog-actions button.danger-filled') as HTMLElement).click();
  await flush();
  assert.equal(sent('hosts_details').length, 2);
  assert.equal(root.querySelector('.host-waiting'), null);
  assert.match(root.querySelector('.host-keys-list')!.textContent!, /OpenAI/);
  again.resolve({ ...DETAILS, keys: [] });
  await flush();
  assert.match(root.querySelector('.host-keys-list')!.textContent!, /No keys are kept there\./);
});

test('the connection coming up asks the server for its details', async () => {
  const { root } = await hostPage(hostsList({ ...DEPLOYED, state: 'connecting' }), {
    hosts_details: () => DETAILS,
  });
  assert.deepEqual(sent('hosts_details'), []);
  assert.match(root.textContent!, /Connect to the server to see its storage and keys\./);
  emit('cloud-event', { type: 'host', host: 'h1', state: 'ready' });
  await flush();
  assert.deepEqual(sent('hosts_details'), [{ id: 'h1' }]);
  assert.match(root.textContent!, /Connected/);
  assert.match(root.textContent!, /Projects: 3 GB/);
});

test('a host’s form: saved secrets aren’t shown, and an empty field keeps them', async () => {
  const { root, api } = await hostPage(
    hostsList({ passphraseSaved: true, passwordSaved: true, state: 'offline' }),
    { hosts_save: ({ host }: { host: unknown }) => host }
  );
  button(root, 'Edit')!.click();
  await flush();
  assert.equal(api.title(), 'Build server');
  const passphrase = root.querySelector('input[type="password"]') as HTMLInputElement;
  assert.equal(passphrase.value, '');
  assert.equal(passphrase.placeholder, 'Saved: leave empty to keep it');
  button(root, 'Save')!.click();
  await flush();
  const [saved] = sent('hosts_save');
  assert.deepEqual(Object.keys(saved.host).sort(), [
    'approvalPolicy',
    'auth',
    'deploy',
    'host',
    'id',
    'keyPath',
    'name',
    'port',
    'user',
  ]);
  // The passphrase stays; the password a key doesn't use is forgotten.
  assert.deepEqual(saved.secrets, { password: '', passphrase: undefined });
  assert.ok(button(root, 'Test connection'), 'back on the host’s page');
});

test('a host’s form: under “Log in with”, a hint says what each way in takes', async () => {
  const { root } = await hostsPage(hostsList());
  button(root, '+ Add host')!.click();
  await flush();
  const field = [...root.querySelectorAll('.host-form label')].find((l) =>
    l.textContent?.startsWith('Log in with')
  )!;
  const select = field.querySelector('select') as HTMLSelectElement;
  assert.deepEqual(
    [...select.options].map((o) => o.textContent),
    [
      'A key file',
      'My SSH agent',
      'A password',
      'A password the server asks for (keyboard-interactive)',
      'My system ssh',
    ]
  );
  const hints: [string, RegExp][] = [
    ['key', /whose public key is in ~\/\.ssh\/authorized_keys on the server/],
    ['agent', /The key never leaves the agent/],
    ['password', /choose “A password the server asks for”/],
    [
      'keyboard-interactive',
      /and no other prompt: for a server that also asks for a two-factor code, use your system ssh/,
    ],
    ['system-ssh', /Testing and deploying need one of the other ways in/],
  ];
  for (const [kind, says] of hints) {
    select.value = kind;
    select.dispatchEvent(new Event('change'));
    await flush();
    const hint = field.nextElementSibling as HTMLElement;
    assert.equal(hint.id, select.getAttribute('aria-describedby'), kind);
    assert.match(hint.textContent!, says, kind);
  }
});

test('a new host: Back from its form returns to the list', async () => {
  const { root, api } = await hostsPage(hostsList());
  button(root, '+ Add host')!.click();
  await flush();
  assert.equal(api.title(), 'New host');
  assert.equal(button(root, 'Remove host'), undefined);
  api.back();
  await flush();
  assert.equal(api.title(), null);
  assert.ok(button(root, '+ Add host'));
});

test('a locked secrets file: unlocked with its passphrase, which the page doesn’t keep', async () => {
  let unlocked = false;
  const list = (): HostsList => ({ ...hostsList(), store: { kind: 'file', unlocked } });
  const { root } = await hostsPage(list(), {
    hosts_list: () => list(),
    hosts_unlock: ({ passphrase }: { passphrase: string }) => {
      if (passphrase !== 'open sesame') throw 'That passphrase doesn’t open the file.';
      unlocked = true;
      return null;
    },
  });
  const field = () =>
    root.querySelector('input[aria-label="passphrase of the secrets file"]') as HTMLInputElement;
  assert.equal(button(root, 'Unlock')!.disabled, true, 'nothing to try yet');
  type(field(), 'not it');
  await flush();
  button(root, 'Unlock')!.click();
  await flush();
  assert.match(root.textContent!, /That passphrase doesn’t open the file\./);
  type(field(), 'open sesame');
  await flush();
  button(root, 'Unlock')!.click();
  await flush();
  assert.deepEqual(
    sent('hosts_unlock').map((args) => args.passphrase),
    ['not it', 'open sesame']
  );
  assert.equal(field(), null, 'unlocked');
  assert.ok(!root.innerHTML.includes('open sesame'));
});

test('a host’s setup script is saved with it', async () => {
  const { root } = await hostsPage(hostsList(), {
    hosts_save: ({ host }: { host: object }) => ({ ...host, id: 'h1' }),
  });
  button(root, '+ Add host')!.click();
  await flush();
  const field = (label: string) =>
    [...root.querySelectorAll('.host-form label')]
      .find((l) => l.textContent?.startsWith(label))!
      .querySelector('input, textarea') as HTMLInputElement;
  type(field('Name'), 'Box');
  type(field('Host'), 'box.example.com');
  type(field('User'), 'dev');
  type(field('Key file'), '~/.ssh/id_ed25519');
  type(field('Setup script'), 'rustup default stable\n');
  await flush();
  button(root, 'Save')!.click();
  await flush();
  const [saved] = sent('hosts_save');
  assert.equal(saved.host.name, 'Box');
  assert.equal(saved.host.setupScript, 'rustup default stable\n');
});

test('a deploy runs the host’s setup script, which can run again', async () => {
  const script = 'npm i -g pnpm\n';
  let deployed = false;
  const list = () => hostsList({ ...(deployed ? DEPLOYED : {}), setupScript: script });
  let runs = 0;
  const { root } = await hostPage(list(), {
    hosts_list: () => list(),
    hosts_deploy: () => {
      deployed = true;
      return '0.2.0';
    },
    hosts_details: () => DETAILS,
    hosts_run_setup: () =>
      ++runs === 1
        ? { status: 0, output: 'added 1 package\n' }
        : { status: 1, output: 'npm ERR! offline\n' },
  });
  assert.equal(button(root, 'Run the setup script'), undefined, 'not before Rovik is there');
  button(root, 'Deploy')!.click();
  await flush();
  assert.deepEqual(sent('hosts_run_setup'), [{ id: 'h1' }]);
  const verdict = () => root.querySelector('.host-setup .host-status')?.textContent;
  assert.equal(verdict(), 'The setup script worked.');
  assert.equal(root.querySelector('.host-setup-output')?.textContent, 'added 1 package\n');
  button(root, 'Run the setup script')!.click();
  await flush();
  assert.equal(runs, 2);
  assert.equal(verdict(), 'The setup script failed, with exit status 1.');
  assert.match(root.querySelector('.host-setup-output')!.textContent!, /npm ERR! offline/);
});

test('removing Rovik with chats on the server: they’re to move back first', async () => {
  const { root } = await hostPage(hostsList({ ...DEPLOYED, chats: 2 }), {
    hosts_details: () => DETAILS,
  });
  button(root, 'Remove Rovik from this server…')!.click();
  await flush();
  assert.match(
    root.textContent!,
    /2 chats are on this server: move them back or delete them first\./
  );
  assert.deepEqual(sent('hosts_removal'), [], 'nothing asked of the server');
});

/** The input in the label that reads `text`. */
const field = (root: ParentNode, text: string) =>
  [...root.querySelectorAll('label')]
    .find((l) => l.textContent?.includes(text))
    ?.querySelector('input') as HTMLInputElement | undefined;

/** Ticks or fills `input` as the user would, ending in its change. */
function change(input: HTMLInputElement, value?: string | boolean) {
  if (typeof value === 'string') input.value = value;
  else input.checked = value ?? true;
  input.dispatchEvent(new Event('change', { bubbles: true }));
}

/** A host page whose app keeps what the page saves (H01). */
async function keepingHostPage(row: Partial<HostRow> = {}) {
  let kept: HostRow = { ...HOST, ...row };
  const page = await hostPage(hostsList(), {
    hosts_list: () => ({ ...hostsList(), hosts: [kept] }),
    hosts_set_policy: ({ policy }: { policy: HostRow['approvalPolicy'] }) => {
      kept = { ...kept, approvalPolicy: policy };
    },
    hosts_set_autonomy: ({ autonomy }: { autonomy: HostRow['autonomy'] }) => {
      if (autonomy?.notifyUrl?.startsWith('ftp:')) {
        throw 'The notify URL is an http:// or https:// address.';
      }
      kept = { ...kept, autonomy };
      return autonomy;
    },
  });
  return { ...page, kept: () => kept };
}

test('works on its own: the third choice shows its block, which an other choice hides', async () => {
  const { root, kept } = await keepingHostPage();
  assert.equal(root.querySelector('.host-autonomy'), null, 'no block while it waits for me');
  const own = field(root, 'Works on its own')!;
  assert.match(own.closest('label')!.textContent!, /Chats on this server run in Autopilot/);
  change(own);
  await flush();
  assert.deepEqual(sent('hosts_set_policy'), [{ id: 'h1', policy: 'autonomous' }]);
  assert.equal(kept().approvalPolicy, 'autonomous');
  const block = root.querySelector('.host-autonomy')!;
  assert.ok(block, 'its block shows');
  // Its defaults: this computer's effort and budget, the result kept on the server.
  assert.ok(field(block, 'As in Settings → Autopilot')!.checked);
  assert.ok(field(block, 'Keep the result on the server')!.checked);
  assert.match(block.textContent!, /The AI reviewer decides\. What it can’t approve is refused/);
  assert.equal(field(block, 'Deploy key on the server'), undefined, 'no key to push with');
  assert.deepEqual(sent('hosts_set_autonomy'), [], 'nothing saved by showing it');
  change(field(root, 'Wait for me')!);
  await flush();
  assert.equal(root.querySelector('.host-autonomy'), null);
});

test('works on its own: each change is saved, the deploy key only once it’s in', async () => {
  const { root, kept } = await keepingHostPage({ approvalPolicy: 'autonomous' });
  const block = () => root.querySelector('.host-autonomy')!;
  change(field(block(), 'Thorough')!);
  await flush();
  assert.deepEqual(sent('hosts_set_autonomy').at(-1), {
    id: 'h1',
    autonomy: { delivery: { kind: 'keepOnHost' }, effort: 'thorough' },
  });
  // Pushing: nothing goes until the key's path is in, the PR box off until then.
  change(field(block(), 'Push to a branch')!);
  await flush();
  assert.equal(sent('hosts_set_autonomy').length, 1);
  const key = field(block(), 'Deploy key on the server')!;
  assert.ok(field(block(), 'Open a pull request')!.disabled);
  assert.match(block().textContent!, /Its path on the server, never the key itself/);
  change(key, '  ~/.ssh/rovik_deploy ');
  await flush();
  assert.deepEqual(kept().autonomy, {
    delivery: { kind: 'pushBranch', deployKey: '~/.ssh/rovik_deploy', openPr: false },
    effort: 'thorough',
  });
  change(field(block(), 'Open a pull request')!, true);
  await flush();
  assert.deepEqual(kept().autonomy!.delivery, {
    kind: 'pushBranch',
    deployKey: '~/.ssh/rovik_deploy',
    openPr: true,
  });
  // A budget: one limit set, the other left as in Settings; then none.
  change(field(block(), 'Stop a task after (hours)')!, '2');
  await flush();
  assert.deepEqual(kept().autonomy!.budget, { maxHours: 2, maxCost: null });
  change(field(block(), 'Stop a task after (hours)')!, '');
  await flush();
  assert.equal(kept().autonomy!.budget, undefined);
  // A token limit: millions on the page, tokens kept; emptied, none.
  change(field(block(), 'or used (million tokens)')!, '1.5');
  await flush();
  assert.deepEqual(kept().autonomy!.budget, {
    maxHours: null,
    maxCost: null,
    maxTokens: 1_500_000,
  });
  change(field(block(), 'or used (million tokens)')!, '');
  await flush();
  assert.equal(kept().autonomy!.budget, undefined);
  // Plan fixing: each field sent inside planFix; emptied, it disappears.
  change(field(block(), 'Max reviews')!, '5');
  await flush();
  assert.deepEqual(sent('hosts_set_autonomy').at(-1)!.autonomy.planFix, { maxRounds: 5 });
  change(field(block(), 'Share of the budget (%)')!, '30');
  await flush();
  assert.deepEqual(kept().autonomy!.planFix, { maxRounds: 5, budgetShare: 0.3 });
  change(field(block(), 'Max reviews')!, '');
  await flush();
  assert.deepEqual(kept().autonomy!.planFix, { budgetShare: 0.3 });
  change(field(block(), 'Share of the budget (%)')!, '');
  await flush();
  assert.equal(kept().autonomy!.planFix, undefined);
  assert.equal(JSON.stringify(sent('hosts_set_autonomy').at(-1)).includes('planFix'), false);
  // The notify URL: kept trimmed; one the app refuses says why, and isn't kept.
  change(field(block(), 'Call this URL')!, ' https://ntfy.sh/rovik-abc ');
  await flush();
  assert.equal(kept().autonomy!.notifyUrl, 'https://ntfy.sh/rovik-abc');
  change(field(block(), 'Call this URL')!, 'ftp://x');
  await flush();
  assert.match(block().querySelector('[role="alert"]')!.textContent!, /http:\/\/ or https:\/\//);
  assert.equal(kept().autonomy!.notifyUrl, 'https://ntfy.sh/rovik-abc');
  // Back to keeping the result on the server.
  change(field(block(), 'Keep the result on the server')!);
  await flush();
  assert.deepEqual(kept().autonomy!.delivery, { kind: 'keepOnHost' });
  assert.equal(field(block(), 'Deploy key on the server'), undefined);
});

test('works on its own: “Test delivery” says what the push found, or why it failed (H05)', async () => {
  const push = {
    delivery: { kind: 'pushBranch' as const, deployKey: '~/.ssh/rovik_deploy', openPr: true },
  };
  let answer: () => unknown = () => ({
    remote: 'git@github.com:acme/app.git',
    branch: 'rovik/test-20261017-095301',
    pullRequests: '`gh` is logged in to github.com: pull requests open.',
  });
  const { root } = await keepingHostPage({ approvalPolicy: 'autonomous', autonomy: push });
  backend.hosts_test_delivery = () => answer();
  const run = () => button(root, 'Test delivery')!;
  assert.ok(run(), 'offered once the push is set up');
  run().click();
  await flush();
  assert.deepEqual(sent('hosts_test_delivery'), [{ id: 'h1' }]);
  const shown = () => root.querySelector('.host-autonomy-tested')!;
  assert.equal(
    shown().textContent,
    'Pushed rovik/test-20261017-095301 to git@github.com:acme/app.git and deleted it. `gh` is logged in to github.com: pull requests open.'
  );
  assert.ok(!shown().classList.contains('warning'));
  answer = () => ({
    remote: 'git@github.com:acme/app.git',
    branch: 'rovik/test-20261017-095302',
    problem: 'The push failed: Permission denied (publickey).',
  });
  run().click();
  await flush();
  assert.equal(shown().textContent, 'The push failed: Permission denied (publickey).');
  assert.ok(shown().classList.contains('warning'));
  answer = () => {
    throw 'Open a project whose repository has an `origin` remote: the test pushes there.';
  };
  run().click();
  await flush();
  assert.equal(root.querySelector('.host-autonomy-tested'), null);
  assert.match(root.querySelector('.host-autonomy [role="alert"]')!.textContent!, /Open a project/);
});

test('connected: the tasks of this computer’s chats there, each a line (H08)', async () => {
  let tasks: unknown[] = [
    {
      chat: 'c1',
      title: 'Make the test pass',
      computer: 'laptop',
      running: 'run-2',
      task: {
        id: 'u1',
        stage: 'implement',
        status: 'running',
        summary: 'Step 2 of 3',
        ms: 125_000,
        cost: 0.4,
        uncosted: 0,
      },
    },
    {
      chat: 'c2',
      title: 'Fix the parser',
      computer: 'laptop',
      outcome: 'done',
      task: {
        id: 'u9',
        stage: 'deliver',
        status: 'done',
        outcome: 'verified',
        summary: 'Delivered: pushed to `rovik/fix-the-parser-u9`',
        ms: 61_000,
        cost: 0,
        uncosted: 3,
      },
    },
    { chat: 'c3', title: '', computer: 'laptop', outcome: 'stopped' },
  ];
  const { root } = await hostPage(hostsList(DEPLOYED), {
    hosts_details: () => DETAILS,
    hosts_tasks: () => tasks,
  });
  assert.deepEqual(sent('hosts_tasks'), [{ id: 'h1' }]);
  const rows = () => [...root.querySelectorAll('.host-tasks-list li')];
  assert.equal(rows().length, 3);
  assert.equal(rows()[0].querySelector('.host-task-title')!.textContent, 'Make the test pass');
  assert.equal(
    rows()[0].querySelector('.host-task-line')!.textContent,
    'Implement: running · 2 min, 0.40'
  );
  assert.ok(rows()[0].classList.contains('running'));
  assert.equal(rows()[0].querySelector('.host-task-running')!.textContent, 'Running');
  assert.match(rows()[0].textContent!, /Step 2 of 3/);
  assert.equal(
    rows()[1].querySelector('.host-task-line')!.textContent,
    'Delivered (verified) · 1 min; no cost reported'
  );
  assert.equal(rows()[1].querySelector('.host-task-running'), null);
  assert.equal(
    rows()[1].querySelector('.host-task-summary code')?.textContent,
    'rovik/fix-the-parser-u9'
  );
  assert.equal(rows()[2].querySelector('.host-task-title')!.textContent, 'c3', 'by its id');
  assert.equal(
    rows()[2].querySelector('.host-task-line')!.textContent,
    'No task: its last run stopped'
  );
  // Refresh asks again.
  tasks = [];
  button(root, 'Refresh')!.click();
  await flush();
  assert.equal(sent('hosts_tasks').length, 2);
  assert.match(
    root.querySelector('.host-tasks')!.textContent!,
    /No chats of this computer’s there/
  );
});

test('a host’s task line names its stage as the strip does, and a partial delivery as one', () => {
  const line = (stage: string, status: string, outcome?: string) =>
    hostTaskLine({
      chat: 'c',
      title: '',
      computer: 'laptop',
      task: { id: 't', stage, status, outcome, ms: 0, cost: 0, uncosted: 0 },
    });
  assert.match(line('break', 'running'), /^Break: running · /);
  assert.match(line('pick', 'done'), /^Pick: done · /);
  assert.match(
    line('review', 'failed', 'review-open'),
    /^Change review: failed \(review-open\) · /
  );
  assert.match(line('deliver', 'done', 'verified'), /^Delivered \(verified\) · /);
  assert.match(line('deliver', 'done', 'unmet'), /^Delivered as partial \(unmet\) · /);
  assert.match(
    line('deliver', 'failed', 'plan-blocked'),
    /^Ended: not delivered \(plan-blocked\) · /
  );
});
