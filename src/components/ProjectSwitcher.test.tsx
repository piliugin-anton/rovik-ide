// Component test: the project switcher's menu (docs/design/workspaces-
// plan.md, 3.4, D3) — its items in their order, Add folder only while a
// workspace is open, workspaces listed among the projects and found by their
// folders, no Delete on what the window has open — and its trigger, which
// in a workspace shows the workspace's name and lists its folders. The app
// is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectSwitcher from './ProjectSwitcher';
import type { ProjectSummary, WorkspaceSummary } from '../lib/chatApi';
import type { WindowTarget } from '../lib/openFolders';
import { listedId, type Listed } from './ProjectList';

let projects: ProjectSummary[] = [];
let workspaces: WorkspaceSummary[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  invoke: async (cmd: string) => {
    if (cmd === 'chat_list_projects') return projects;
    if (cmd === 'workspace_list') return workspaces;
    throw new Error(`no app: ${cmd}`);
  },
};

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const project = (id: string, name: string): ProjectSummary => ({
  id,
  name,
  path: `/work/${name}`,
  chatCount: 1,
  lastOpenedAt: 1,
  missing: false,
});
const SHOP: WorkspaceSummary = {
  id: 'w-shop',
  name: 'Shop',
  folders: [
    { name: 'api', path: '/work/api', missing: false },
    { name: 'web', path: '/work/web', missing: false },
  ],
  chatCount: 2,
  lastOpenedAt: 2,
};
const SHOP_TARGET: WindowTarget = {
  kind: 'workspace',
  id: 'w-shop',
  name: 'Shop',
  folders: SHOP.folders.map(({ name, path }) => ({ name, path })),
};

/** The switcher with `target` open, its menu open; what its items did. */
async function switcher(target: WindowTarget | null, openIds?: ReadonlySet<string>) {
  const root = container();
  const did: string[] = [];
  const opened: Listed[] = [];
  const dispose = render(
    () => (
      <ProjectSwitcher
        target={target}
        openIds={openIds}
        onOpenFolder={() => did.push('open')}
        onNewProject={() => did.push('new project')}
        onNewWorkspace={() => did.push('new workspace')}
        onAddFolder={() => did.push('add folder')}
        onNewWindow={() => did.push('new window')}
        onOpen={(entry) => opened.push(entry)}
        onRelink={() => undefined}
        onDelete={() => undefined}
      />
    ),
    root
  );
  root.querySelector<HTMLButtonElement>('.project-switcher-trigger')!.click();
  await flush();
  const items = () =>
    [...root.querySelectorAll<HTMLButtonElement>('.project-switcher-open')].map((b) =>
      b.textContent?.replace(/⌘O|Ctrl\+O/, '').trim()
    );
  const button = (label: string) =>
    [...root.querySelectorAll<HTMLButtonElement>('.project-switcher-open')].find((b) =>
      b.textContent?.startsWith(label)
    )!;
  return { root, did, opened, items, button, dispose };
}

test('the menu: open, new project, create a workspace, and a new window, in that order', async () => {
  projects = [project('p-api', 'api')];
  workspaces = [];
  const { items, button, did, dispose } = await switcher({ kind: 'folder', path: '/work/api' });
  assert.deepEqual(items(), ['Open project…', 'New project…', 'Create a workspace…', 'New window']);
  button('Create a workspace…').click();
  assert.deepEqual(did, ['new workspace']);
  dispose();
});

test('in a workspace, Add folder to workspace… comes after Create a workspace…', async () => {
  projects = [];
  workspaces = [SHOP];
  const { items, button, did, root, dispose } = await switcher(SHOP_TARGET);
  assert.deepEqual(items(), [
    'Open project…',
    'New project…',
    'Create a workspace…',
    'Add folder to workspace…',
    'New window',
  ]);
  button('Add folder to workspace…').click();
  assert.deepEqual(did, ['add folder']);
  assert.equal(root.querySelector('.project-switcher-menu'), null, 'the menu closes');
  dispose();
});

test('in a workspace, the trigger shows its name with stacked folders, and its tooltip lists them', async () => {
  workspaces = [SHOP];
  const { root, dispose } = await switcher(SHOP_TARGET);
  const trigger = root.querySelector('.project-switcher-trigger')!;
  assert.equal(trigger.querySelector('.project-switcher-name')?.textContent, 'Shop');
  assert.ok(trigger.querySelector('.project-switcher-stack'), 'the stacked folders icon');
  assert.equal(trigger.getAttribute('title'), '/work/api\n/work/web');
  dispose();
});

test("workspaces are listed with the projects, found by a folder's path, and open from the list", async () => {
  projects = [project('p-blog', 'blog')];
  workspaces = [SHOP];
  const { root, opened, dispose } = await switcher(null);
  const rows = () => [...root.querySelectorAll('.project-row-name')].map((e) => e.textContent);
  assert.deepEqual(rows(), ['Shop', 'blog'], 'the most recently opened first');

  const filter = root.querySelector<HTMLInputElement>('.project-switcher-filter')!;
  filter.value = '/work/web';
  filter.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  assert.deepEqual(rows(), ['Shop']);
  filter.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.deepEqual(opened.map(listedId), ['w-shop'], 'Enter opens the first one found');
  dispose();
});

test('no Delete on what the window has open: its workspace, and its folders’ projects', async () => {
  projects = [project('p-api', 'api'), project('p-blog', 'blog')];
  workspaces = [SHOP];
  const { root, dispose } = await switcher(SHOP_TARGET, new Set(['w-shop', 'p-api']));
  const deletable = [...root.querySelectorAll('.project-row')]
    .filter((row) => row.querySelector('.project-row-delete'))
    .map((row) => row.querySelector('.project-row-name')?.textContent);
  assert.deepEqual(deletable, ['blog']);
  dispose();
});
