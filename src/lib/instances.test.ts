// instances.ts tests (node:test): what a window does with a project another
// window has open (src-tauri/src/instances.rs): it isn't opened here, and
// the user is told why, the other window brought to the front; the chats
// without a project are one window's too, and the chats another window is
// at work on are its own to judge. The app is answered here, command by
// command.
import { test } from 'node:test';
import assert from 'node:assert/strict';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  // Notices close by themselves later: not something to wait for here.
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

const { chatsAtWorkElsewhere, checkProject, claimNoProject, claimProject, openElsewhereNotice } =
  await import('./instances');
const { notifications, dismiss } = await import('./notifications');
const { chatsBlocked, currentProject, resolveProject } = await import('./projects');

const free = {
  claimed: true,
  openElsewhere: false,
  openAs: null,
  copy: false,
  atWork: false,
  focused: false,
};
const folder = (path: string) => ({ kind: 'folder' as const, path });
const workspace = (id: string) => ({ kind: 'workspace' as const, id });
const theirs = (over: Record<string, unknown> = {}) => ({
  claimed: false,
  openElsewhere: true,
  openAs: '/work/app',
  copy: false,
  atWork: false,
  focused: true,
  ...over,
});

function app(answers: Record<string, (args: any) => unknown>) {
  calls = [];
  backend = answers;
  dismiss('open-elsewhere');
}

const shown = () => notifications().map((n) => ({ title: n.title, message: n.message }));

test('a project another window has stays there: that window comes to the front, and this one says so', async () => {
  app({ project_claim: () => theirs() });
  assert.equal(await claimProject(folder('/work/app')), false);
  assert.deepEqual(calls, [
    { cmd: 'project_claim', args: { target: folder('/work/app'), focusOther: true } },
  ]);
  assert.deepEqual(shown(), [
    {
      title: 'app is open in another window',
      message: 'That window is in front now. Each window has a project of its own.',
    },
  ]);
});

test('a free project is claimed, and nothing is said', async () => {
  app({ project_claim: () => free });
  assert.equal(await claimProject(folder('/work/other')), true);
  assert.deepEqual(shown(), []);
});

test('checking asks without claiming; at start, a window still closing is waited for', async () => {
  app({ project_check: () => theirs({ focused: false }) });
  assert.equal(await checkProject(folder('/work/app'), { focusOther: false }), false);
  assert.deepEqual(calls, [
    { cmd: 'project_check', args: { target: folder('/work/app'), focusOther: false } },
  ]);

  // The last project, restored right after Rovik was quit: free a moment later.
  let asked = 0;
  app({ project_claim: () => (++asked < 2 ? theirs({ focused: false }) : free) });
  assert.equal(await claimProject(folder('/work/app'), { focusOther: false, retries: 2 }), true);
  assert.equal(asked, 2);
  assert.deepEqual(shown(), []);
});

test('the app failing to answer opens the project anyway', async () => {
  app({});
  const quiet = console.error;
  console.error = () => {};
  try {
    assert.equal(await claimProject(folder('/work/app')), true);
  } finally {
    console.error = quiet;
  }
});

test('what the user is told: a copy sharing the chats, a project whose chats are at work', () => {
  assert.deepEqual(openElsewhereNotice(folder('/work/app-copy'), theirs({ copy: true })), {
    title: 'app-copy’s chats are open in another window',
    message:
      '/work/app shares its chats, and it’s open there. Each window has a project of its own.',
  });
  assert.deepEqual(openElsewhereNotice(folder('C:\\work\\app'), theirs({ atWork: true })), {
    title: 'app’s chats are at work in another window',
    message:
      'That window went on to another project while they work. Go back to app there, or open it here once they’re done.',
  });
  assert.equal(
    openElsewhereNotice(folder('/work/app'), theirs({ focused: false })).message,
    'Each window has a project of its own: open another one here.'
  );
});

test('a workspace says its own name, and a folder of one says which', () => {
  // A workspace open in another window: it has no folder to name.
  assert.deepEqual(
    openElsewhereNotice(workspace('w1'), theirs({ openAs: null, workspace: 'Shop' })),
    {
      title: 'Shop is open in another window',
      message: 'That window is in front now. Each window has a project of its own.',
    }
  );
  // A folder of another window's workspace names both.
  assert.deepEqual(openElsewhereNotice(folder('/work/api'), theirs({ workspace: 'Shop' })), {
    title: 'api is open in another window, in Shop',
    message: 'That window is in front now. Each window has a project of its own.',
  });
});

test('a workspace is claimed by its ID: its folders are the backend’s to read', async () => {
  app({ project_claim: () => free });
  assert.equal(await claimProject(workspace('w1')), true);
  assert.deepEqual(calls, [
    { cmd: 'project_claim', args: { target: { kind: 'workspace', id: 'w1' }, focusOther: true } },
  ]);
});

test('the chats without a project are claimed quietly, and the chat panel says why it has none', async () => {
  app({
    project_claim: () => theirs({ openAs: null, focused: false }),
    chat_list_projects: () => [],
  });
  assert.equal(await claimNoProject(), false);
  assert.deepEqual(calls[0], { cmd: 'project_claim', args: { target: null, focusOther: false } });
  assert.deepEqual(shown(), []);

  await resolveProject(null);
  assert.deepEqual(chatsBlocked(), { kind: 'no-project-elsewhere' });
  assert.equal(currentProject(), null);

  // A folder opened here: its own chats.
  const project = { id: 'p1', name: 'app', path: '/work/app' };
  app({ chat_open_project: () => ({ project, suggestion: null }), chat_list_projects: () => [] });
  await resolveProject('/work/app');
  assert.equal(chatsBlocked(), null);
  assert.deepEqual(currentProject(), project);

  // Free again: this window has them.
  app({ project_claim: () => free, chat_list_projects: () => [] });
  await resolveProject(null);
  assert.equal(chatsBlocked(), null);
});

test('a folder whose chats can’t be opened shows that, not the chats without a project', async () => {
  app({
    chat_open_project: () => {
      throw new Error('database is locked');
    },
    chat_list_projects: () => [],
  });
  const quiet = console.error;
  console.error = () => {};
  try {
    await resolveProject('/work/app');
  } finally {
    console.error = quiet;
  }
  assert.equal(chatsBlocked()?.kind, 'failed');
  assert.ok(
    calls.every((c) => c.cmd !== 'project_claim'),
    'not claimed for this window'
  );
});

test('the chats another window is at work on are asked about, only when there are some', async () => {
  app({
    chats_at_work_elsewhere: ({ dialogIds }) => dialogIds.filter((id: string) => id !== 'mine'),
  });
  assert.deepEqual([...(await chatsAtWorkElsewhere(['mine', 'theirs']))], ['theirs']);
  calls = [];
  assert.equal((await chatsAtWorkElsewhere([])).size, 0);
  assert.deepEqual(calls, []);
});
