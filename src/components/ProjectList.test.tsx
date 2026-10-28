// Component test: the project list hides Delete for every project the
// window has open, not only for the one it shows (docs/design/workspaces-
// plan.md, 3.4, B06), and lists workspaces among the projects: by when each
// was last opened, with their folders, those missing, and a delete question
// of their own (H01). Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectList, {
  deleteQuestion,
  listed,
  listedId,
  listedMatches,
  type Listed,
} from './ProjectList';
import type { ProjectSummary, WorkspaceSummary } from '../lib/chatApi';

const summary = (id: string, name: string, missing = false, at = Date.now()): ProjectSummary => ({
  id,
  name,
  path: `/work/${name}`,
  chatCount: 2,
  lastOpenedAt: at,
  missing,
});

const workspace = (
  id: string,
  name: string,
  folders: { name: string; missing?: boolean }[],
  at = Date.now(),
  chatCount = 4
): WorkspaceSummary => ({
  id,
  name,
  folders: folders.map((f) => ({ name: f.name, path: `/work/${f.name}`, missing: !!f.missing })),
  chatCount,
  lastOpenedAt: at,
});

function list(entries: Listed[], openIds?: ReadonlySet<string>) {
  const root = container();
  const deleted: string[] = [];
  const opened: string[] = [];
  const dispose = render(
    () => (
      <ProjectList
        entries={entries}
        openIds={openIds}
        onOpen={(entry) => opened.push(listedId(entry))}
        onRelink={() => undefined}
        onDelete={(entry) => deleted.push(listedId(entry))}
      />
    ),
    root
  );
  const rows = [...root.querySelectorAll<HTMLElement>('.project-row')];
  const deleteOf = (i: number) => rows[i]?.querySelector('.project-row-delete') ?? null;
  return { root, rows, deleteOf, deleted, opened, dispose };
}

const projects = () =>
  listed([summary('p-api', 'api'), summary('p-web', 'web'), summary('p-old', 'old', true)], []);

test('no Delete on any project this window has open', () => {
  const { rows, deleteOf, deleted, dispose } = list(projects(), new Set(['p-api', 'p-web']));
  assert.equal(rows.length, 3);
  assert.equal(deleteOf(0), null, 'open');
  assert.equal(deleteOf(1), null, 'open too, though another one may be shown');
  assert.ok(deleteOf(2), 'not open: it can be deleted');
  (deleteOf(2) as HTMLButtonElement).click();
  assert.deepEqual(deleted, ['p-old']);
  dispose();
});

test('with nothing open, every project can be deleted', () => {
  const { deleteOf, dispose } = list(projects());
  assert.ok(deleteOf(0) && deleteOf(1) && deleteOf(2));
  dispose();
});

test('a missing project is offered Relink, and Delete unless the window has it', () => {
  const { rows, root, dispose } = list(projects(), new Set(['p-old']));
  const missing = rows[2];
  assert.ok(missing?.querySelector('.project-row-actions .link-button'));
  assert.equal(
    missing?.querySelector('.project-row-delete'),
    null,
    'missing, but the window has it: no Delete'
  );
  assert.ok(root.textContent?.includes('folder missing'));
  dispose();
});

test("a workspace's row: its name, that it's one of 3 folders, the folders, and how many are missing", () => {
  const shop = workspace('w-shop', 'Shop', [
    { name: 'api' },
    { name: 'web', missing: true },
    { name: 'infra' },
  ]);
  const { rows, opened, dispose } = list(listed([], [shop]));
  const row = rows[0]!;
  assert.equal(row.querySelector('.project-row-name')?.textContent, 'Shop1 folder missing');
  assert.equal(row.querySelector('.project-row-path')?.textContent, 'api, web, infra');
  assert.match(
    row.querySelector('.project-row-meta')?.textContent ?? '',
    /^Workspace · 3 folders · /
  );
  assert.equal(
    row.querySelector('.project-row-main')?.getAttribute('title'),
    '/work/api\n/work/web\n/work/infra',
    'its tooltip lists the folders'
  );
  // The rest of it opens, with one folder missing (D20).
  const open = row.querySelector<HTMLButtonElement>('.project-row-main')!;
  assert.equal(open.disabled, false);
  open.click();
  assert.deepEqual(opened, ['w-shop']);
  dispose();
});

test('no Delete on the workspace the window has open; another one can go', () => {
  const entries = listed(
    [],
    [
      workspace('w-shop', 'Shop', [{ name: 'api' }]),
      workspace('w-blog', 'Blog', [{ name: 'site' }], 0),
    ]
  );
  const { deleteOf, deleted, dispose } = list(entries, new Set(['w-shop']));
  assert.equal(deleteOf(0), null, 'open here');
  (deleteOf(1) as HTMLButtonElement).click();
  assert.deepEqual(deleted, ['w-blog']);
  dispose();
});

test('projects and workspaces are listed together by when each was last opened', () => {
  const entries = listed(
    [summary('p-api', 'api', false, 300), summary('p-web', 'web', false, 100)],
    [workspace('w-shop', 'Shop', [{ name: 'api' }], 200)]
  );
  assert.deepEqual(entries.map(listedId), ['p-api', 'w-shop', 'p-web']);
});

test('a row is the same while its project or workspace is: the list read again keeps it', () => {
  const api = summary('p-api', 'api');
  const shop = workspace('w-shop', 'Shop', [{ name: 'api' }]);
  const [first, second] = [listed([api], [shop]), listed([api], [shop])];
  assert.equal(first[0], second[0]);
  assert.equal(first[1], second[1]);
  assert.notEqual(
    listed([{ ...api }], [])[0],
    first.find((e) => e.kind === 'project')
  );
});

test('the delete question of a workspace names its own chats, and keeps its folders and theirs', () => {
  const question = deleteQuestion({
    kind: 'workspace',
    workspace: workspace('w-shop', 'Shop', [{ name: 'api' }]),
  });
  assert.deepEqual(question, {
    title: 'Delete the workspace Shop?',
    message:
      "Its 4 workspace chats are deleted for good. Its folders, and their own chats, aren't touched.",
    confirmLabel: 'Delete workspace',
  });
  const one = deleteQuestion({
    kind: 'workspace',
    workspace: workspace('w-shop', 'Shop', [{ name: 'api' }], 0, 1),
  });
  assert.match(one.message, /^Its 1 workspace chat is deleted for good\./);
  // A project's question is as it was.
  assert.equal(
    deleteQuestion({ kind: 'project', project: summary('p-api', 'api') }).title,
    "Delete api's chat history?"
  );
});

test("the filter finds a workspace by its name or by one of its folders' paths", () => {
  const shop: Listed = {
    kind: 'workspace',
    workspace: workspace('w-shop', 'Shop', [{ name: 'api' }, { name: 'web' }]),
  };
  assert.equal(listedMatches(shop, 'sho'), true);
  assert.equal(listedMatches(shop, '/work/web'), true);
  assert.equal(listedMatches(shop, 'blog'), false);
  assert.equal(listedMatches({ kind: 'project', project: summary('p-api', 'api') }, 'API'), true);
});
