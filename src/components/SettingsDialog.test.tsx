// Component tests: the Settings dialog's menu (each page with what it's
// for and its state now), its navigation, and that a change on a page is
// saved at once. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import SettingsDialog from './SettingsDialog';
import { DEFAULT_PERMISSIONS } from '../lib/permissions';
import { DEFAULT_SUBAGENT_SETTINGS } from '../lib/subagentSettings';
import type { PersistedAiSettings } from '../lib/aiSettings';

/** The saved AI settings, as the backend keeps them. */
let saved: PersistedAiSettings = {
  version: 2,
  providers: [
    {
      id: 'a',
      name: 'Alpha',
      baseUrl: 'http://a',
      apiKey: '',
      stream: true,
      models: [
        { id: 'm1', enabled: true },
        { id: 'm2', enabled: true },
        { id: 'm3', enabled: false },
      ],
    },
  ],
  active: { providerId: 'a', modelId: 'm1' },
  permissions: { ...DEFAULT_PERMISSIONS, verifyChanges: false },
  subagents: DEFAULT_SUBAGENT_SETTINGS,
};
let calls: { cmd: string; args: any }[] = [];
let backend: Record<string, (args: any) => unknown> = {};
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    if (cmd === 'load_ai_settings') return JSON.stringify(saved);
    if (cmd === 'save_ai_settings') {
      saved = JSON.parse(args.json);
      return;
    }
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const COMPUTER: typeof backend = {
  workload_status: () => ({
    slots: null,
    autoSlots: 1,
    maxSlots: 8,
    busy: 0,
    waiting: 0,
    cores: 8,
    memoryGb: 16,
  }),
  vfs_status: () => ({
    driver: { driver: 'FUSE', installed: false, version: null, detail: null, canInstall: true },
    mountpoint: null,
    defaultMountpoint: '/run/user/1000/rovik',
    problem: null,
    capacity: 0,
    used: 0,
    history: { revisions: 0, bytes: 0, limits: { maxBytes: 0, perFile: 0, maxFileSize: 0 } },
  }),
  hosts_list: () => ({
    hosts: [
      {
        id: 'h1',
        name: 'Build server',
        host: 'build.example.com',
        port: 22,
        user: 'dev',
        auth: 'key',
        approvalPolicy: 'waitForMe',
        deploy: { state: 'deployed', version: '0.2.0' },
        state: 'ready',
        chats: 2,
        passwordSaved: false,
        passphraseSaved: false,
      },
      {
        id: 'h2',
        name: 'Spare',
        host: 'spare.example.com',
        port: 22,
        user: 'dev',
        auth: 'key',
        approvalPolicy: 'waitForMe',
        deploy: { state: 'notDeployed' },
        chats: 0,
        passwordSaved: false,
        passphraseSaved: false,
      },
    ],
    store: { kind: 'keychain' },
  }),
  network_settings: () => ({
    settings: {
      proxies: [],
      routes: {
        models: 'auto',
        ssh: 'direct',
        browser: 'own',
        hostModels: 'own',
        hostBrowser: 'own',
      },
    },
    system: { https: null, http: null },
    defaultTimeoutSecs: 30,
    minTimeoutSecs: 5,
    maxTimeoutSecs: 300,
  }),
  project_profile: () => ({ stack: [], checks: [], instructions: null }),
  load_project_settings: () => null,
  restricted_files: () => [
    { path: '.env', blockRead: true, blockWrite: true, detected: true, listed: false },
  ],
};

let dispose = () => {};

async function dialog(options: { rootPath?: string | null; answers?: typeof backend } = {}) {
  dispose();
  calls = [];
  backend = { ...COMPUTER, ...options.answers };
  const root = container();
  let closed = 0;
  dispose = render(
    () => (
      <SettingsDialog
        rootPath={options.rootPath === undefined ? '/home/me/demo' : options.rootPath}
        onClose={() => closed++}
      />
    ),
    root
  );
  await flush();
  return { root, closed: () => closed };
}

const items = (root: HTMLElement) =>
  [...root.querySelectorAll('.settings-menu-item')].map((b) => ({
    label: b.querySelector('.settings-menu-label')!.textContent,
    state: b.querySelector('.settings-menu-state')?.textContent ?? null,
    warning: b.querySelector('.settings-menu-state')?.classList.contains('warning') ?? false,
  }));

const item = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll('.settings-menu-item')].find(
    (b) => b.querySelector('.settings-menu-label')!.textContent === label
  ) as HTMLButtonElement;

const title = (root: HTMLElement) => root.querySelector('.modal-title')!.textContent;

test('settings menu: the pages in groups, each with what it’s for and its state now', async () => {
  const { root } = await dialog();
  assert.deepEqual(
    [...root.querySelectorAll('.settings-menu-group-title')].map((t) => t.textContent),
    ['Editor', 'AI chats', 'This project · demo', 'This computer']
  );
  assert.deepEqual(items(root), [
    { label: 'Editor', state: 'Dark Modern · 14 px · spaces (2)', warning: false },
    { label: 'Model providers', state: '1 provider, 2 models enabled', warning: false },
    {
      label: 'Connectors',
      state: 'None yet: connect Claude Code or Codex to use one in place of a model',
      warning: false,
    },
    {
      label: 'Permissions & approval',
      state: 'Commands: 20 listed, no approval · Edits: no approval · Other tools: no approval',
      warning: false,
    },
    {
      label: 'Agent runs',
      state: 'Pauses every 50 tool calls · checklist when needed · no checks after a run',
      warning: false,
    },
    { label: 'Subagents', state: 'Off: the agent works alone', warning: false },
    {
      label: 'Autopilot',
      state:
        'Balanced · a task stops after 2 h or once it cost 5 · plan: up to 8 reviews, 30 min · 10 commands never run',
      warning: false,
    },
    { label: 'Project checks', state: 'Not run: checking is off in Agent runs', warning: true },
    { label: 'Restricted files', state: '1 file kept from the agent', warning: false },
    { label: 'Heavy commands', state: 'Auto: 1 slot on this machine', warning: false },
    {
      label: 'Virtual files',
      state: 'FUSE isn’t installed: chats work in the project folder directly',
      warning: true,
    },
    { label: 'Network', state: 'No proxy: connections go directly', warning: false },
    { label: 'Cloud hosts', state: '2 hosts, 1 connected, 2 chats there', warning: false },
    // Its version once the window's updater has asked (App starts it).
    { label: 'Updates', state: 'Looks for updates every 30 minutes', warning: false },
  ]);
  // Every item says what its page affects.
  for (const b of root.querySelectorAll('.settings-menu-item')) {
    assert.ok((b.querySelector('.settings-menu-sub')!.textContent?.length ?? 0) > 20);
  }
  assert.match(root.querySelector('.settings-menu-note')!.textContent!, /Chosen per chat/);
});

test('settings menu: without a project there is no project group; what can’t be read has no state', async () => {
  const { root } = await dialog({
    rootPath: null,
    answers: {
      workload_status: () => {
        throw new Error('no backend');
      },
    },
  });
  assert.deepEqual(
    [...root.querySelectorAll('.settings-menu-group-title')].map((t) => t.textContent),
    ['Editor', 'AI chats', 'This computer']
  );
  assert.equal(item(root, 'Project checks'), undefined);
  assert.equal(item(root, 'Restricted files'), undefined);
  const heavy = items(root).find((i) => i.label === 'Heavy commands')!;
  assert.equal(heavy.state, null);
});

test('settings dialog: a page opens with a Back bar; Back and Escape return to the menu, Escape there closes', async () => {
  const { root, closed } = await dialog();
  assert.equal(title(root), 'Settings');
  item(root, 'Agent runs').click();
  await flush();
  assert.equal(title(root), 'Agent runs');
  assert.match(root.querySelector('.modal-back-button')!.textContent!, /Settings/);
  assert.ok(root.querySelector('select[aria-label="Pause every"]'));
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
  await flush();
  assert.equal(title(root), 'Settings');
  assert.equal(closed(), 0);
  item(root, 'Heavy commands').click();
  await flush();
  assert.equal(title(root), 'Heavy commands');
  (root.querySelector('.modal-back-button') as HTMLButtonElement).click();
  await flush();
  assert.equal(title(root), 'Settings');
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
  assert.equal(closed(), 1);
});

test('settings dialog: a change on a page is saved at once, and the menu shows it', async () => {
  const { root } = await dialog();
  item(root, 'Agent runs').click();
  await flush();
  const check = [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.includes('Check the changes'))!
    .querySelector('input') as HTMLInputElement;
  assert.equal(check.checked, false, 'as saved');
  check.click();
  await flush();
  const saves = calls.filter((c) => c.cmd === 'save_ai_settings');
  assert.equal(saves.length, 1);
  assert.equal(saved.permissions.verifyChanges, true);
  assert.deepEqual(saved.active, { providerId: 'a', modelId: 'm1' }, 'the chat’s model kept');
  assert.equal(saved.providers.length, 1, 'the providers kept');
  (root.querySelector('.modal-back-button') as HTMLButtonElement).click();
  await flush();
  // Checking is on now, but the project has no check to run.
  const project = items(root).find((i) => i.label === 'Project checks')!;
  assert.equal(project.state, 'Nothing to run: no check is turned on');
  assert.equal(project.warning, true);
});

test('settings menu: Project checks counts the checks that run, none until the user ticks one', async () => {
  saved = { ...saved, permissions: { ...saved.permissions, verifyChanges: true } };
  let projectSettings: unknown = null;
  const { root } = await dialog({
    answers: {
      project_profile: () => ({
        stack: ['TypeScript'],
        checks: [
          { kind: 'typecheck', command: 'npx tsc --noEmit', source: 'tsconfig.json' },
          { kind: 'test', command: 'npm test', source: 'package.json scripts.test' },
        ],
        instructions: null,
      }),
      load_project_settings: () => projectSettings,
      save_project_settings: ({ settings }: { settings: unknown }) => {
        projectSettings = settings;
      },
    },
  });
  const project = () => items(root).find((i) => i.label === 'Project checks');
  assert.deepEqual(project(), {
    label: 'Project checks',
    state: 'Nothing to run: no check is turned on',
    warning: true,
  });
  item(root, 'Project checks').click();
  await flush();
  const boxes = [
    ...root.querySelectorAll('.project-check-row input[type="checkbox"]'),
  ] as HTMLInputElement[];
  assert.deepEqual(
    boxes.map((b) => b.checked),
    [false, false],
    'found, none ticked'
  );
  boxes[1].click();
  await flush();
  (root.querySelector('.modal-back-button') as HTMLButtonElement).click();
  await flush();
  assert.deepEqual(project(), {
    label: 'Project checks',
    state: '1 check runs when the agent finishes a change',
    warning: false,
  });
});

test('settings dialog: Restricted files opens on the project’s table, and a change shows in the menu', async () => {
  let projectSettings: any = null;
  const { root } = await dialog({
    answers: {
      load_project_settings: () => projectSettings,
      save_project_settings: ({ settings }: { settings: unknown }) => {
        projectSettings = settings;
      },
      restricted_files: () => [
        {
          path: '.env',
          blockRead: projectSettings?.restrictedFiles?.[0]?.blockRead ?? true,
          blockWrite: projectSettings?.restrictedFiles?.[0]?.blockWrite ?? true,
          detected: true,
          listed: !!projectSettings,
        },
      ],
    },
  });
  item(root, 'Restricted files').click();
  await flush();
  assert.equal(title(root), 'Restricted files');
  const boxes = [...root.querySelectorAll('tbody input[type="checkbox"]')] as HTMLInputElement[];
  assert.deepEqual(
    boxes.map((b) => b.checked),
    [true, true]
  );
  boxes[0].click();
  boxes[1].click();
  await flush();
  assert.deepEqual(projectSettings, {
    restrictedFiles: [{ path: '.env', blockRead: false, blockWrite: false }],
  });
  (root.querySelector('.modal-back-button') as HTMLButtonElement).click();
  await flush();
  assert.equal(
    items(root).find((i) => i.label === 'Restricted files')!.state,
    'Every file listed is open to the agent'
  );
});

test('settings dialog: the pages open on the saved settings, cross-links between them work', async () => {
  const { root } = await dialog();
  item(root, 'Permissions & approval').click();
  await flush();
  assert.equal(title(root), 'Permissions & approval');
  // Nothing of the runs' settings here any more: a link to their page.
  assert.equal(root.querySelector('select[aria-label="Pause every"]'), null);
  [...root.querySelectorAll('button.link-button')]
    .find((b) => b.textContent === 'Agent runs')!
    .click();
  await flush();
  assert.equal(title(root), 'Agent runs');
  (root.querySelector('.modal-back-button') as HTMLButtonElement).click();
  await flush();
  item(root, 'Editor').click();
  await flush();
  // The editor page is the editor's own: the agent's edits are on Agent runs.
  assert.equal(
    [...root.querySelectorAll('.settings-section-title')].find((t) => t.textContent === 'Agent'),
    undefined
  );
  assert.equal(
    [...root.querySelectorAll('label')].find((l) =>
      l.textContent?.includes('Open the files the agent edits')
    ),
    undefined
  );
});
