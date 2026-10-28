// Component test: Create a workspace…'s dialog (docs/design/workspaces-
// plan.md, 3.4, D4): the open folder listed first, the name following the
// folders until one is typed, folders added by the picker (several at once)
// or by a typed path, each checked as it's listed — against those before it
// (D5) and the other windows' (D7) — with why one can't join on its row and
// Create off while one can't, and Create making the workspace. The app is
// answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import NewWorkspaceDialog, { defaultWorkspaceName } from './NewWorkspaceDialog';
import type { Workspace } from '../lib/chatApi';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** The folders the test's file system has, and the one another window has open. */
const FOLDERS = ['/work/api', '/work/web', '/work/docs', '/work/blog', '/work/api/src'];
const free = {
  claimed: true,
  openElsewhere: false,
  openAs: null,
  copy: false,
  atWork: false,
  focused: false,
};

/** The app: D5 as the backend checks it, by path, and blog open in another window, in Journal. */
function app() {
  calls = [];
  backend = {
    workspace_check_folder: ({ folder, others }: { folder: string; others: string[] }) => {
      if (!FOLDERS.includes(folder)) return { kind: 'notAFolder' };
      if (others.includes(folder)) return { kind: 'duplicate', other: folder };
      const holder = others.find((other) => folder.startsWith(`${other}/`));
      return holder ? { kind: 'inside', other: holder } : null;
    },
    project_check: ({ target }: { target: { path: string } }) =>
      target.path === '/work/blog'
        ? {
            ...free,
            claimed: false,
            openElsewhere: true,
            openAs: '/work/blog',
            workspace: 'Journal',
          }
        : free,
    workspace_create: ({ name, folders }: { name: string; folders: string[] }): Workspace => ({
      id: 'w-new',
      name,
      folders: folders.map((path) => ({ name: path.split('/').pop()!, path, missing: false })),
    }),
  };
}

/** The dialog, open in a window that has `openFolder` open; what it made. */
async function dialog(openFolder: string | null) {
  const root = container();
  const created: Workspace[] = [];
  let cancelled = false;
  const dispose = render(
    () => (
      <NewWorkspaceDialog
        openFolder={openFolder}
        onCreated={(workspace) => created.push(workspace)}
        onCancel={() => (cancelled = true)}
      />
    ),
    root
  );
  await flush();
  const el = document.querySelector<HTMLElement>('.new-workspace-dialog')!;
  const rows = () =>
    [...el.querySelectorAll('.new-workspace-folder')].map((row) => ({
      path: row.querySelector('.new-workspace-folder-path')?.textContent,
      why: row.querySelector('.new-project-note.error')?.textContent ?? null,
    }));
  const name = () => el.querySelector<HTMLInputElement>('#new-workspace-name')!;
  const create = () =>
    [...el.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent === 'Create')!;
  const type = async (path: string) => {
    const field = el.querySelector<HTMLInputElement>('.new-workspace-path')!;
    field.value = path;
    field.dispatchEvent(new Event('input', { bubbles: true }));
    field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await flush();
  };
  const leaveOut = async (index: number) => {
    el.querySelectorAll<HTMLButtonElement>('.new-workspace-folder-remove')[index].click();
    await flush();
  };
  return {
    el,
    rows,
    name,
    create,
    type,
    leaveOut,
    created,
    cancelled: () => cancelled,
    dispose,
  };
}

test('the open folder is listed first, and the name follows the folders until one is typed', async () => {
  app();
  const d = await dialog('/work/api');
  assert.deepEqual(d.rows(), [{ path: '/work/api', why: null }]);
  assert.equal(d.name().value, 'api');
  await d.type('/work/web');
  assert.deepEqual(
    d.rows().map((r) => r.path),
    ['/work/api', '/work/web']
  );
  assert.equal(d.name().value, 'api + web');

  d.name().value = 'Shop';
  d.name().dispatchEvent(new Event('input', { bubbles: true }));
  await d.type('/work/docs');
  assert.equal(d.name().value, 'Shop', 'a typed name stays');
  d.dispose();
});

test('a typed or pasted path is added, out of the quotes a copied path comes in', async () => {
  app();
  const d = await dialog(null);
  assert.equal(d.rows().length, 0);
  assert.equal(d.create().disabled, true, 'no folders yet');
  await d.type('  "/work/web"  ');
  assert.deepEqual(d.rows(), [{ path: '/work/web', why: null }]);
  assert.equal(d.create().disabled, false);
  d.dispose();
});

test('each refusal shows on its row and keeps Create off', async () => {
  app();
  const d = await dialog('/work/api');
  await d.type('/work/api/src');
  await d.type('/work/blog');
  await d.type('/work/nowhere');
  await d.type('web');
  assert.deepEqual(d.rows(), [
    { path: '/work/api', why: null },
    {
      path: '/work/api/src',
      why: "It's inside /work/api, another folder of this workspace: a workspace holds folders side by side.",
    },
    { path: '/work/blog', why: 'blog is open in another window, in Journal' },
    { path: '/work/nowhere', why: "That isn't a folder." },
    { path: 'web', why: 'Type the folder’s full path.' },
  ]);
  assert.equal(d.create().disabled, true);
  // The other window was asked quietly: it stays where it is.
  assert.ok(
    calls.filter((c) => c.cmd === 'project_check').every((c) => c.args.focusOther === false)
  );

  // Each left out, Create is on again.
  for (let i = 0; i < 4; i++) await d.leaveOut(1);
  assert.deepEqual(d.rows(), [{ path: '/work/api', why: null }]);
  assert.equal(d.create().disabled, false);
  d.dispose();
});

test('a folder listed twice is refused the second time; the first one stays', async () => {
  app();
  const d = await dialog('/work/api');
  await d.type('/work/api');
  assert.deepEqual(d.rows(), [
    { path: '/work/api', why: null },
    { path: '/work/api', why: "It's a folder of this workspace already, as /work/api." },
  ]);
  d.dispose();
});

test('Add folder… takes several folders at once', async () => {
  app();
  backend['plugin:dialog|open'] = ({ options }: { options: { multiple: boolean } }) =>
    options.multiple ? ['/work/web', '/work/docs'] : '/work/web';
  const d = await dialog('/work/api');
  [...d.el.querySelectorAll<HTMLButtonElement>('button')]
    .find((b) => b.textContent === 'Add folder…')!
    .click();
  await flush();
  assert.deepEqual(
    d.rows().map((r) => r.path),
    ['/work/api', '/work/web', '/work/docs']
  );
  d.dispose();
});

test('Create makes the workspace with its name and its folders, in order', async () => {
  app();
  const d = await dialog('/work/api');
  await d.type('/work/web');
  d.create().click();
  await flush();
  const made = calls.filter((c) => c.cmd === 'workspace_create').map((c) => c.args);
  assert.deepEqual(made, [{ name: 'api + web', folders: ['/work/api', '/work/web'] }]);
  assert.deepEqual(
    d.created.map((w) => w.id),
    ['w-new']
  );
  d.dispose();
});

test('a create the app refuses says why, and the dialog stays', async () => {
  app();
  backend.workspace_create = () => Promise.reject('web is inside api, another folder');
  const d = await dialog('/work/api');
  await d.type('/work/web');
  d.create().click();
  await flush();
  assert.equal(d.created.length, 0);
  assert.match(d.el.textContent ?? '', /Couldn’t create the workspace: web is inside api/);
  d.dispose();
});

test("a workspace's name from many folders is cut to the longest a name can be", () => {
  const paths = Array.from({ length: 20 }, (_, i) => `/work/folder-${i}`);
  const name = defaultWorkspaceName(paths);
  assert.equal(name.length, 80);
  assert.ok(name.endsWith('…'));
  assert.equal(defaultWorkspaceName(['/work/api', 'C:\\work\\web']), 'api + web');
});
