// projects.ts tests (node:test): which project each open folder is, asked
// once per folder. A folder that looks like a copy, or like a moved project,
// is asked about in a notification of its own, naming it, so two folders'
// questions are both asked (docs/design/workspaces-plan.md, 3.3, D5). The
// app is answered here, command by command.
import { test } from 'node:test';
import assert from 'node:assert/strict';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  setTimeout: (run: () => void, ms: number) => {
    const timer = setTimeout(run, ms);
    timer.unref();
    return timer;
  },
  clearTimeout: (timer: ReturnType<typeof setTimeout>) => clearTimeout(timer),
  __TAURI_INTERNALS__: {
    transformCallback: () => 0,
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      const answer = backend[cmd];
      if (!answer) throw new Error(`no app: ${cmd}`);
      return answer(args);
    },
  },
};

const {
  currentProject,
  newFoldersGeneration,
  projectList,
  refreshProjects,
  resolveProject,
  workspaceList,
} = await import('./projects');
const { notifications, dismiss } = await import('./notifications');

const project = (root: string) => ({ id: `project-${root}`, name: root, path: root, idFile: true });
const summary = (id: string, name: string) => ({
  id,
  name,
  path: `/gone/${name}`,
  chatCount: 2,
  lastOpenedAt: 1,
  missing: true,
});

function app(answers: Record<string, (args: any) => unknown>) {
  calls = [];
  backend = { workspace_list: () => [], ...answers };
  for (const n of notifications()) dismiss(n.id);
}

const shown = () =>
  notifications().map((n) => ({
    id: n.id,
    title: n.title,
    message: n.message,
    actions: (n.actions ?? []).map((a) => a.label),
  }));

test('two folders that look like copies are both asked, each naming its folder', async () => {
  app({
    chat_open_project: ({ root }: { root: string }) => ({
      project: project(root),
      suggestion: { kind: 'copy', original: summary('original-1', 'app') },
    }),
    chat_list_projects: () => [],
  });
  await resolveProject('/work/api', []);
  await resolveProject('/work/web', []);
  const asked = shown();
  assert.deepEqual(
    asked.map((n) => n.id),
    ['project-copy:/work/api', 'project-copy:/work/web'],
    'one question each'
  );
  const [api, web] = asked;
  assert.ok(api.message?.includes('/work/api'), String(api.message));
  assert.ok(web.message?.includes('/work/web'), String(web.message));
  assert.deepEqual(api.actions, ['Separate history', 'Share chats']);
});

test('a copy whose original is another folder of the window keeps its own history', async () => {
  // `api` is a copy of `web`, which is open here: sharing would make one
  // project two of the workspace's folders (D5).
  app({
    chat_open_project: ({ root }: { root: string }) => ({
      project: project(root),
      suggestion: { kind: 'copy', original: summary('project-/work/web', 'web') },
    }),
    chat_list_projects: () => [],
  });
  const openHere = [
    { path: '/work/web', name: 'web', missing: false, project: project('/work/web') },
    { path: '/work/api', name: 'api', missing: false },
  ];
  await resolveProject('/work/api', openHere);
  const [asked] = shown();
  assert.deepEqual(asked.actions, ['Separate history'], 'sharing is not offered');
  assert.ok(asked.message?.includes('/work/api'), String(asked.message));
});

test('a moved project is asked about here, in the folder it is', async () => {
  app({
    chat_open_project: ({ root }: { root: string }) => ({
      project: project(root),
      suggestion: { kind: 'moved', candidates: [summary('old-1', 'app')] },
    }),
    chat_list_projects: () => [],
  });
  await resolveProject('/work/api', []);
  const [asked] = shown();
  assert.equal(asked.id, 'project-moved:/work/api');
  assert.ok(asked.message?.includes('/work/api'), String(asked.message));
  assert.deepEqual(asked.actions, ['No', 'Use its chats']);
});

test('an answer for a folder the window has left is dropped', async () => {
  // The window switches while api's answer is out: web is what it shows, and
  // api's answer is for a folder it has left (3.3).
  const answering: ((opened: unknown) => void)[] = [];
  app({
    chat_open_project: () =>
      new Promise((resolve) => answering.push(resolve as (opened: unknown) => void)),
    chat_list_projects: () => [],
  });
  const leaving = resolveProject('/work/api', []);
  newFoldersGeneration();
  const here = resolveProject('/work/web', []);
  answering[0]?.({
    project: project('/work/api'),
    suggestion: { kind: 'copy', original: summary('original-1', 'app') },
  });
  await leaving;
  assert.equal(currentProject(), undefined, 'the folder left is not the one shown');
  assert.deepEqual(shown(), [], 'and nothing is asked about it');

  answering[1]?.({ project: project('/work/web'), suggestion: null });
  await here;
  assert.equal(currentProject()?.path, '/work/web');
});

test('an answer for a folder resolved again since is dropped', async () => {
  // The app answers in the test's order: the first answer comes back after
  // the same folder was resolved again, so its question isn't asked.
  const answering: ((opened: unknown) => void)[] = [];
  app({
    chat_open_project: () =>
      new Promise((resolve) => answering.push(resolve as (opened: unknown) => void)),
    chat_list_projects: () => [],
  });
  const first = resolveProject('/work/api', []);
  const second = resolveProject('/work/api', []);
  assert.equal(answering.length, 2, 'each asked once');
  answering[0]?.({
    project: project('/work/api'),
    suggestion: { kind: 'copy', original: summary('original-1', 'app') },
  });
  await first;
  answering[1]?.({ project: project('/work/api'), suggestion: null });
  await second;
  assert.deepEqual(shown(), [], 'the first answer, for the folder it left, asks nothing');
});

test('the workspaces are listed beside the projects, each keeping its row while it is unchanged', async () => {
  const shop = (missing: boolean) => ({
    id: 'w-shop',
    name: 'Shop',
    folders: [
      { name: 'api', path: '/work/api', missing: false },
      { name: 'web', path: '/work/web', missing },
    ],
    chatCount: 3,
    lastOpenedAt: 5,
  });
  const blog = { id: 'w-blog', name: 'Blog', folders: [], chatCount: 0, lastOpenedAt: 1 };
  let listed = [shop(false), blog];
  app({ chat_list_projects: () => [summary('p-1', 'app')], workspace_list: () => listed });
  await refreshProjects();
  const first = workspaceList();
  assert.deepEqual(
    first.map((w) => w.id),
    ['w-shop', 'w-blog']
  );
  assert.equal(projectList().length, 1);

  // Read again: an unchanged workspace is the same row; one whose folder went missing is new.
  listed = [shop(true), { ...blog }];
  await refreshProjects();
  assert.notEqual(workspaceList()[0], first[0], 'web went missing: its row changes');
  assert.equal(workspaceList()[1], first[1], 'Blog is as it was: the same row');
});

test("the projects are listed when the workspaces can't be read", async () => {
  app({
    chat_list_projects: () => [summary('p-1', 'app'), summary('p-2', 'web')],
    workspace_list: () => Promise.reject('database is locked'),
  });
  await refreshProjects();
  assert.equal(projectList().length, 2);
});
