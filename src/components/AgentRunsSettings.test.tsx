// Component tests: Settings → Agent runs: what happens while the agent
// works and when it finishes, and what each setting says about whether it
// has an effect now. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import AgentRunsSettings from './AgentRunsSettings';
import { DEFAULT_PERMISSIONS, type AgentPermissions } from '../lib/permissions';
import { editorSettings, updateEditorSettings } from '../lib/editorSettings';
import type { SettingsPage } from '../lib/appSettings';

/** The backend's answers in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const DEMO = '/home/me/demo';

/**
 * A project with `n` checks found in its files, each ticked in Project
 * checks (the user's list then) unless `ticked` is false: found alone, a
 * check is off.
 */
const project = (n: number, ticked = true) => {
  const checks = Array.from({ length: n }, (_, i) => ({
    kind: 'test',
    command: `npm run check${i}`,
    source: 'package.json',
  }));
  return {
    project_profile: () => ({ stack: ['node'], checks, instructions: null }),
    load_project_settings: () =>
      ticked && n > 0 ? { checks: checks.map((c) => ({ ...c, enabled: true })) } : null,
  };
};

async function page(
  options: { permissions?: Partial<AgentPermissions>; rootPath?: string | null } = {}
) {
  const root = container();
  const [permissions, setPermissions] = createSignal<AgentPermissions>({
    ...DEFAULT_PERMISSIONS,
    ...options.permissions,
  });
  const opened: SettingsPage[] = [];
  render(
    () => (
      <AgentRunsSettings
        permissions={permissions()}
        onChange={setPermissions}
        rootPath={options.rootPath === undefined ? DEMO : options.rootPath}
        onOpenPage={(p) => opened.push(p)}
      />
    ),
    root
  );
  await flush();
  return { root, permissions, opened };
}

const statuses = (root: HTMLElement) =>
  [...root.querySelectorAll('.settings-status')].map((p) => ({
    text: p.textContent!.replace(/\s+/g, ' ').trim(),
    warning: p.classList.contains('warning'),
  }));

const select = (root: HTMLElement, label: string) =>
  root.querySelector(`select[aria-label="${label}"]`) as HTMLSelectElement;

function choose(select: HTMLSelectElement, value: string) {
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

test('agent runs: the sections, and each setting’s meaning as it is set', async () => {
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(2) };
  const { root, permissions } = await page();
  const titles = [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);
  assert.deepEqual(titles, [
    'While it works',
    'When it finishes',
    'Isolated chats',
    'In the editor',
  ]);
  assert.match(root.textContent!, /Autopilot’s tasks follow their own rules/);

  assert.equal(select(root, 'Pause every').value, '50');
  assert.match(root.textContent!, /After every 50 tool calls the run stops/);
  choose(select(root, 'Pause every'), 'never');
  assert.equal(permissions().toolCallCheckpoint, null);
  assert.match(root.textContent!, /never pauses for its length alone/);

  assert.equal(select(root, 'Checklist').value, 'auto');
  choose(select(root, 'Checklist'), 'always');
  assert.equal(permissions().checklist, 'always');
  assert.match(root.textContent!, /Every request starts with a checklist/);

  assert.equal(select(root, 'Conflicts').value, 'chat');
  choose(select(root, 'Conflicts'), 'me');
  assert.equal(permissions().chatResolvesConflicts, false);
});

test('agent runs: the checks after a run say what would run for the open project', async () => {
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(2) };
  const { root, permissions, opened } = await page();
  assert.deepEqual(statuses(root), [
    { text: '2 checks set up for demo run after a change. Project checks', warning: false },
  ]);
  const check = [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.includes('Check the changes'))!
    .querySelector('input') as HTMLInputElement;
  assert.equal(check.checked, true);
  check.click();
  assert.equal(permissions().verifyChanges, false);
  assert.deepEqual(statuses(root), [
    {
      text: 'Off: demo’s checks don’t run after the agent’s changes. Project checks',
      warning: true,
    },
  ]);
  [...root.querySelectorAll('button.link-button')]
    .find((b) => b.textContent === 'Project checks')!
    .click();
  assert.deepEqual(opened, ['project']);
});

test('agent runs: no checks set up, or no project open, is said', async () => {
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(0) };
  const none = await page();
  assert.deepEqual(statuses(none.root), [
    { text: 'Nothing to run: no checks are set up for demo. Project checks', warning: true },
  ]);
  // Found in the project's files, none ticked: none is set up.
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(2, false) };
  const unticked = await page();
  assert.deepEqual(statuses(unticked.root), [
    { text: 'Nothing to run: no checks are set up for demo. Project checks', warning: true },
  ]);
  const closed = await page({ rootPath: null });
  assert.deepEqual(statuses(closed.root), [
    {
      text: 'No project is open. The checks are set per project, in Project checks, once one is.',
      warning: false,
    },
  ]);
});

test('agent runs: the conflicts rule has no effect without virtual files, and says so', async () => {
  backend = { vfs_status: () => ({ mountpoint: null }), ...project(1) };
  const { root, opened } = await page();
  const status = statuses(root).find((s) => s.text.startsWith('No effect now'));
  assert.ok(status, 'a status says it has no effect');
  assert.equal(status!.warning, true);
  assert.match(status!.text, /virtual files aren’t mounted on this computer/);
  [...root.querySelectorAll('button.link-button')]
    .find((b) => b.textContent === 'Virtual files')!
    .click();
  assert.deepEqual(opened, ['vfs']);

  // Mounted, or not known: nothing is said.
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(1) };
  const mounted = await page();
  assert.equal(
    statuses(mounted.root).find((s) => s.text.startsWith('No effect now')),
    undefined
  );
  backend = { ...project(1) };
  const unknown = await page();
  assert.equal(
    statuses(unknown.root).find((s) => s.text.startsWith('No effect now')),
    undefined
  );
});

test('agent runs: opening the files the agent edits is the editor’s setting', async () => {
  backend = { vfs_status: () => ({ mountpoint: '/mnt/vfs' }), ...project(1) };
  updateEditorSettings({ openFilesAfterAgentEdits: false });
  const { root } = await page();
  const open = [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.includes('Open the files the agent edits'))!
    .querySelector('input') as HTMLInputElement;
  assert.equal(open.checked, false);
  open.click();
  assert.equal(editorSettings().openFilesAfterAgentEdits, true);
  assert.equal(open.checked, true);
});
