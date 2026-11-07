// Failed reads must stay distinct from empty settings, and recover in place.
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import type { JSX } from 'solid-js';
import AgentRunsSettings from './AgentRunsSettings';
import CloudHostsSettings from './CloudHostsSettings';
import HeavyCommandsSettings from './HeavyCommandsSettings';
import NetworkSettings from './NetworkSettings';
import ProjectChecksSettings from './ProjectChecksSettings';
import RestrictedFilesSettings from './RestrictedFilesSettings';
import VirtualFilesSettings from './VirtualFilesSettings';
import WslSettings from './WslSettings';
import { DEFAULT_PERMISSIONS } from '../lib/permissions';
import { DEFAULT_ROUTES } from '../lib/network';

let failures = new Set<string>();
let slots: number | null = null;
let slotSaves: number[] = [];
const answers: Record<string, () => unknown> = {
  workload_status: () => ({
    slots,
    autoSlots: 1,
    maxSlots: 8,
    busy: 0,
    waiting: 0,
    cores: 8,
    memoryGb: 16,
  }),
  vfs_status: () => ({
    driver: { driver: 'FUSE', installed: true, version: '3', detail: null, canInstall: true },
    mountpoint: '/mnt/rovik',
    defaultMountpoint: '/mnt/rovik',
    problem: null,
    capacity: 1024,
    used: 0,
    history: { revisions: 0, bytes: 0, limits: { maxBytes: 0, perFile: 0, maxFileSize: 0 } },
  }),
  wsl_status: () => ({
    available: true,
    unsupported: false,
    distros: ['Ubuntu'],
    deployed: false,
    outdated: false,
    appVersion: '0.2.1',
  }),
  hosts_list: () => ({ hosts: [], store: { kind: 'keychain' } }),
  hosts_summaries: () => [],
  network_settings: () => ({
    settings: { proxies: [], routes: { ...DEFAULT_ROUTES } },
    system: {},
    defaultTimeoutSecs: 30,
    minTimeoutSecs: 5,
    maxTimeoutSecs: 300,
  }),
  project_profile: () => ({
    stack: ['TypeScript'],
    checks: [{ kind: 'test', command: 'npm test', source: 'package.json' }],
    instructions: null,
  }),
  load_project_settings: () => null,
  restricted_files: () => [
    { path: '.env', blockRead: true, blockWrite: true, detected: true, listed: false },
  ],
};
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') return 1;
    if (cmd === 'plugin:event|unlisten') return null;
    if (failures.has(cmd)) throw new Error('temporarily unavailable');
    if (cmd === 'workload_set_slots') {
      slots = args.slots;
      slotSaves.push(args.slots);
      return;
    }
    if (!answers[cmd]) throw new Error(`Unexpected command: ${cmd}`);
    return answers[cmd]();
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

const flush = async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
const cases: {
  name: string;
  command: string;
  render: () => JSX.Element;
  loading: string;
  recovered: RegExp;
  falseEmpty?: string;
}[] = [
  {
    name: 'Heavy commands',
    command: 'workload_status',
    render: () => <HeavyCommandsSettings />,
    loading: 'Asking the machine…',
    recovered: /Nothing heavy is running/,
  },
  {
    name: 'Virtual files',
    command: 'vfs_status',
    render: () => <VirtualFilesSettings />,
    loading: 'Checking…',
    recovered: /FUSE is installed/,
  },
  {
    name: 'WSL',
    command: 'wsl_status',
    render: () => <WslSettings />,
    loading: 'Looking…',
    recovered: /User in the distribution/,
  },
  {
    name: 'Network',
    command: 'network_settings',
    render: () => <NetworkSettings />,
    loading: 'Reading the settings…',
    recovered: /Timeout \(seconds\)/,
  },
  {
    name: 'Cloud hosts',
    command: 'hosts_list',
    render: () => <CloudHostsSettings />,
    loading: 'Loading…',
    recovered: /No hosts yet/,
  },
  {
    name: 'Project checks',
    command: 'project_profile',
    render: () => (
      <ProjectChecksSettings
        root="/demo"
        permissions={DEFAULT_PERMISSIONS}
        onPermissionsChange={() => {}}
        onOpenPage={() => {}}
      />
    ),
    loading: 'Looking at the project…',
    recovered: /npm test|Detected: package.json/,
    falseEmpty: 'No checks. Add the commands',
  },
  {
    name: 'Restricted files',
    command: 'restricted_files',
    render: () => <RestrictedFilesSettings root="/demo" />,
    loading: 'Looking at the project…',
    recovered: /\.env/,
    falseEmpty: 'No files that look like credentials',
  },
  {
    name: 'Agent runs',
    command: 'project_profile',
    render: () => (
      <AgentRunsSettings
        rootPath="/demo"
        permissions={DEFAULT_PERMISSIONS}
        onChange={() => {}}
        onOpenPage={() => {}}
      />
    ),
    loading: 'Looking at demo’s checks…',
    recovered: /Nothing to run: no checks are set up for demo/,
  },
];

for (const scenario of cases) {
  test(`${scenario.name} reports a failed read and retries without leaving the page`, async () => {
    failures = new Set([scenario.command]);
    const root = container();
    const dispose = render(scenario.render, root);
    try {
      await flush();
      const alert = root.querySelector('[role="alert"]');
      assert.ok(alert, 'the failed read is announced');
      assert.match(alert.textContent!, /couldn.t|couldn’t/i);
      assert.ok(!root.textContent!.includes(scenario.loading), 'loading has ended');
      if (scenario.falseEmpty) {
        assert.ok(!root.textContent!.includes(scenario.falseEmpty), 'an unread list is not empty');
        assert.equal(
          root.querySelector('.project-check-add, .restricted-file-add'),
          null,
          'edits wait for a successful read'
        );
      }
      failures.clear();
      (alert.querySelector('button') as HTMLButtonElement).click();
      await flush();
      assert.equal(root.querySelector('[role="alert"]'), null, 'the failure clears on success');
      assert.match(root.textContent!, scenario.recovered);
    } finally {
      dispose();
      root.remove();
    }
  });
}

test('WSL setup and installation use stacked form fields', async () => {
  failures.clear();
  for (const available of [true, false]) {
    const before = answers.wsl_status;
    answers.wsl_status = () => ({ ...(before() as object), available });
    const root = container();
    const dispose = render(() => <WslSettings />, root);
    try {
      await flush();
      const fields = [...root.querySelectorAll('input')];
      assert.equal(fields.length, available ? 2 : 1);
      assert.ok(fields.every((input) => input.closest('.profile-form')));
    } finally {
      dispose();
      root.remove();
      answers.wsl_status = before;
    }
  }
});
