// Component tests: the Git panel when a git hook stops a commit, a push or
// a merge's commit (src-tauri/src/git/hooks.rs): the hook's output shows,
// with a button that tries once more without the hooks that check (git's
// --no-verify). And a workspace's panel, a section per repository (D10),
// whose one message commits each of them on its own branch (D11). The app
// is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import { GitPanel } from './GitPanel';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { GitOperation, GitRepos, GitStatus } from '../types';
import { oneRepository } from '../test/gitRepos';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
/** The commands the panel sent, in order. */
let calls: { cmd: string; args: any }[] = [];
const callbacks = new Map<number, (event: unknown) => void>();
/** The events the panel listens to, by their callback. */
let listeners: { event: string; handler: number }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, handler: args.handler });
      return args.handler;
    }
    if (cmd === 'plugin:event|unlisten') {
      listeners = listeners.filter((l) => l.handler !== args.eventId);
      return null;
    }
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

/** The watcher says git's state may have changed in `folder`, or that it
 *  became a repository of its own or stopped being one. */
function gitChanged(folder: string, repositories = false) {
  for (const l of listeners.filter((l) => l.event === 'git-changed')) {
    callbacks.get(l.handler)!({ event: 'git-changed', id: 1, payload: { folder, repositories } });
  }
}

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const CHANGED: GitStatus = {
  branch: 'main',
  ahead: 0,
  behind: 0,
  files: [
    { path: 'a.txt', status: 'M', staged: false, unstaged: true, additions: 1, deletions: 1 },
  ],
  workdir: '/p',
  upstream: 'origin/main',
  push_remote: 'origin',
};

/** Tauri turns a command's error down as its text. */
const refuse = (text: string) => Promise.reject(text);

function mount() {
  const el = container();
  const dispose = render(() => <GitPanel folders={[{ path: '/p', name: 'p' }]} session="/p" />, el);
  return { el, dispose };
}

function button(el: HTMLElement, label: string): HTMLButtonElement | undefined {
  return [...el.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as
    HTMLButtonElement | undefined;
}

async function typeAndCommit(el: HTMLElement, message: string) {
  const input = el.querySelector('.git-commit-input') as HTMLInputElement;
  input.value = message;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  (el.querySelector('.git-commit-button') as HTMLButtonElement).click();
  await flush();
}

const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

test('git panel: a pre-commit hook that stops the commit offers to commit without hooks', async () => {
  let status = CHANGED;
  calls = [];
  backend = {
    git_repos: () => oneRepository('/p'),
    git_status: () => status,
    git_commit: (args) => {
      if (!args.noVerify) {
        return refuse('Not committed: the pre-commit hook failed (exit code 1):\nlint: 2 errors');
      }
      status = { ...CHANGED, files: [], ahead: 1 };
      return null;
    },
    git_push: () => {
      status = { ...status, ahead: 0 };
      return { result: 'pushed', message: null };
    },
  };
  const { el, dispose } = mount();
  await flush();
  await typeAndCommit(el, 'wip');

  const error = el.querySelector('.git-inline-error-text');
  assert.equal(
    error?.textContent,
    'Not committed: the pre-commit hook failed (exit code 1):\nlint: 2 errors'
  );
  assert.deepEqual(
    sent('git_commit').map((a) => a.noVerify),
    [false]
  );
  const skip = button(el, 'Commit without hooks');
  assert.ok(skip, 'the skip is offered');
  assert.match(skip.title, /--no-verify/);
  // The message is kept for the retry.
  assert.equal((el.querySelector('.git-commit-input') as HTMLInputElement).value, 'wip');

  skip.click();
  await flush();
  assert.deepEqual(
    sent('git_commit').map((a) => [a.message, a.noVerify]),
    [
      ['wip', false],
      ['wip', true],
    ]
  );
  // The push that follows still runs pre-push.
  assert.deepEqual(
    sent('git_push').map((a) => a.noVerify),
    [false]
  );
  assert.equal(el.querySelector('.git-inline-error'), null);
  assert.equal(button(el, 'Commit without hooks'), undefined);
  dispose();
});

test('git panel: a pre-push hook that stops the push offers to push without hooks', async () => {
  let status: GitStatus = { ...CHANGED, files: [], ahead: 1 };
  calls = [];
  backend = {
    git_repos: () => oneRepository('/p'),
    git_status: () => status,
    git_push: (args) => {
      if (!args.noVerify) {
        return refuse('Not pushed: the pre-push hook failed (exit code 1):\ntests failed');
      }
      status = { ...status, ahead: 0 };
      return { result: 'pushed', message: null };
    },
  };
  const { el, dispose } = mount();
  await flush();
  // Nothing to commit and a commit to push: the button pushes.
  (el.querySelector('.git-commit-button') as HTMLButtonElement).click();
  await flush();
  assert.match(el.querySelector('.git-inline-error-text')!.textContent!, /tests failed$/);
  button(el, 'Push without hooks')!.click();
  await flush();
  assert.deepEqual(
    sent('git_push').map((a) => a.noVerify),
    [false, true]
  );
  assert.equal(el.querySelector('.git-inline-error'), null);
  dispose();
});

test('git panel: a merge its hook refused offers to commit the merge without hooks', async () => {
  const merging: GitOperation = {
    kind: 'merge',
    branch: 'main',
    incoming: 'origin/main',
    step: null,
    total: null,
    commit: null,
    conflicts: [],
    canContinue: true,
  };
  let status: GitStatus = { ...CHANGED, files: [], operation: merging };
  calls = [];
  backend = {
    git_repos: () => oneRepository('/p'),
    git_status: () => status,
    git_operation_continue: (args) => {
      if (!args.noVerify) {
        return refuse('Not committed: the pre-commit hook failed (exit code 1):\nnot yet');
      }
      status = { ...status, operation: null };
      return { result: 'merged', conflicts: [] };
    },
  };
  const { el, dispose } = mount();
  await flush();
  button(el, 'Commit merge')!.click();
  await flush();
  assert.match(el.querySelector('.git-inline-error-text')!.textContent!, /not yet$/);
  button(el, 'Commit merge without hooks')!.click();
  await flush();
  assert.deepEqual(
    sent('git_operation_continue').map((a) => a.noVerify),
    [false, true]
  );
  assert.equal(el.querySelector('.git-inline-error'), null);
  dispose();
});

test('git panel: an error no hook caused offers no skip', async () => {
  calls = [];
  backend = {
    git_repos: () => oneRepository('/p'),
    git_status: () => CHANGED,
    git_commit: () => refuse('No Git identity configured (user.name / user.email)'),
  };
  const { el, dispose } = mount();
  await flush();
  await typeAndCommit(el, 'wip');
  assert.match(el.querySelector('.git-inline-error-text')!.textContent!, /No Git identity/);
  assert.equal(el.querySelector('.git-skip-hooks'), null);
  dispose();
});

// ---------------------------------------------------------------------------
// A workspace: a section per repository, and one commit across them.
// ---------------------------------------------------------------------------

/** Each of `paths` a repository of its own, named after its folder. */
const reposOf = (paths: string[]): GitRepos => ({
  repos: paths.map((path) => ({
    key: `${path}/.git`,
    workdir: path,
    name: path.split('/').pop()!,
    scopes: [''],
    folders: [path],
  })),
  without: [],
});

/** Each repository's status, by the folder it's asked from. */
let statuses: Record<string, GitStatus> = {};
const changedIn = (path: string, branch = 'main'): GitStatus => ({
  ...CHANGED,
  branch,
  workdir: path,
});

/** The panel of a workspace with `paths` as its folders. */
function mountWorkspace(paths: string[]) {
  const el = container();
  const dispose = render(
    () => (
      <>
        <GitPanel
          folders={paths.map((path) => ({ path, name: path.split('/').pop()! }))}
          session="workspace:shop"
        />
        <ConfirmDialogHost />
      </>
    ),
    el
  );
  return { el, dispose };
}

/** The section named `name`. */
const section = (el: HTMLElement, name: string) =>
  [...el.querySelectorAll<HTMLElement>('.git-repo-section')].find(
    (s) => s.querySelector('.git-repo-name')?.textContent === name
  );
/** The section's box for the commit. */
const box = (el: HTMLElement, name: string) =>
  section(el, name)!.querySelector<HTMLInputElement>('.git-repo-include input')!;
/** What the panel says above its commit bar. */
const panelError = (el: HTMLElement) =>
  el.querySelector('.git-panel > .git-inline-error .git-inline-error-text')?.textContent ?? null;
const pushed = () => sent('git_push').map((a) => a.path);
const message = (el: HTMLElement) =>
  (el.querySelector('.git-commit-input') as HTMLInputElement).value;

/** A commit across repositories as git_commit_many makes it: `failing`
 *  fails after the others are made. */
function commitMany(failing: Set<string>) {
  return (args: { repos: { path: string }[] }) => {
    const committed: string[] = [];
    const failed: { path: string; error: string }[] = [];
    for (const { path } of args.repos) {
      if (failing.has(path)) {
        failed.push({ path, error: `failed to lock file '${path}/.git/refs/heads/main.lock'` });
        continue;
      }
      committed.push(path);
      statuses[path] = { ...statuses[path], files: [], ahead: statuses[path].ahead + 1 };
    }
    return { result: 'done', committed, failed };
  };
}

function workspaceApp(paths: string[], failing = new Set<string>()) {
  calls = [];
  backend = {
    git_repos: () => reposOf(paths),
    git_status: (args) => statuses[args.path],
    git_commit_many: commitMany(failing),
    git_push: (args) => {
      statuses[args.path] = { ...statuses[args.path], ahead: 0 };
      return { result: 'pushed', message: null };
    },
  };
}

test('git panel: one repository has no title of its own, and asks git from its folder as before', async () => {
  calls = [];
  backend = { git_repos: () => oneRepository('/p'), git_status: () => CHANGED };
  const { el, dispose } = mount();
  await flush();
  assert.equal(el.querySelector('.git-repo-title'), null, 'no name, no box');
  assert.ok(el.querySelector('.git-panel-header .git-branch-button'), 'its header, as before');
  assert.ok(el.querySelector('.git-commit-bar'));
  assert.deepEqual(sent('git_status')[0], { path: '/p', scopes: [''] });
  dispose();
});

test('git panel: two repositories show two sections with their names and branches', async () => {
  statuses = {
    '/w/api': changedIn('/w/api'),
    '/w/web': { ...changedIn('/w/web', 'feature/cart'), ahead: 2 },
  };
  workspaceApp(['/w/api', '/w/web']);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  const names = [...el.querySelectorAll('.git-repo-section .git-repo-name')].map(
    (n) => n.textContent
  );
  assert.deepEqual(names, ['api', 'web']);
  assert.equal(section(el, 'api')!.querySelector('.git-branch-name')!.textContent, 'main');
  assert.equal(section(el, 'web')!.querySelector('.git-branch-name')!.textContent, 'feature/cart');
  assert.equal(section(el, 'web')!.querySelector('.git-badge-ahead')!.textContent, '↑2');
  assert.equal(el.querySelectorAll('.git-commit-bar').length, 1, "one message box, the window's");
  dispose();
});

test('git panel: a repository in a merge is left out of the commit, saying why', async () => {
  const merging: GitOperation = {
    kind: 'merge',
    branch: 'main',
    incoming: 'origin/main',
    step: null,
    total: null,
    commit: null,
    conflicts: [],
    canContinue: true,
  };
  statuses = {
    '/w/api': { ...changedIn('/w/api'), operation: merging },
    '/w/web': changedIn('/w/web'),
  };
  workspaceApp(['/w/api', '/w/web']);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  const api = box(el, 'api');
  assert.equal(api.checked, false);
  assert.equal(api.disabled, true);
  assert.equal(api.closest('label')!.title, 'Finish the merge in api first');
  assert.equal(box(el, 'web').checked, true);
  await typeAndCommit(el, 'feat: cart');
  assert.deepEqual(
    sent('git_commit_many').map((a) => a.repos.map((r: { path: string }) => r.path)),
    [['/w/web']]
  );
  dispose();
});

test('git panel: a commit that fails in one repository keeps the message and pushes nothing, until a retry commits it and every repository of the message is pushed', async () => {
  statuses = {
    '/w/api': changedIn('/w/api'),
    '/w/web': changedIn('/w/web'),
    '/w/infra': changedIn('/w/infra'),
  };
  const failing = new Set(['/w/infra']);
  workspaceApp(['/w/api', '/w/web', '/w/infra'], failing);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web', '/w/infra']);
  await flush();
  await typeAndCommit(el, 'feat: across');
  assert.deepEqual(
    sent('git_commit_many').map((a) => [a.message, a.repos.map((r: { path: string }) => r.path)]),
    [['feat: across', ['/w/api', '/w/web', '/w/infra']]]
  );
  assert.equal(
    panelError(el),
    "Committed in api and web; nothing is pushed until infra is committed too.\ninfra: failed to lock file '/w/infra/.git/refs/heads/main.lock'"
  );
  assert.equal(message(el), 'feat: across', 'the message stays for the retry');
  assert.deepEqual(pushed(), [], 'nothing is pushed yet');

  // The lock is gone: the retry commits infra alone, then all three go.
  failing.clear();
  (el.querySelector('.git-commit-button') as HTMLButtonElement).click();
  await flush();
  assert.deepEqual(
    sent('git_commit_many').map((a) => a.repos.map((r: { path: string }) => r.path)),
    [['/w/api', '/w/web', '/w/infra'], ['/w/infra']]
  );
  assert.deepEqual(pushed(), ['/w/api', '/w/web', '/w/infra']);
  assert.equal(message(el), '');
  assert.equal(panelError(el), null);
  dispose();
});

test('git panel: a push that fails shows in its own section, and the others are pushed', async () => {
  statuses = { '/w/api': changedIn('/w/api'), '/w/web': changedIn('/w/web') };
  workspaceApp(['/w/api', '/w/web']);
  backend.git_push = (args) => {
    if (args.path === '/w/web') return refuse('Failed to push: could not reach github.com');
    statuses[args.path] = { ...statuses[args.path], ahead: 0 };
    return { result: 'pushed', message: null };
  };
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  await typeAndCommit(el, 'feat: both');
  assert.deepEqual(pushed(), ['/w/api', '/w/web']);
  assert.equal(
    section(el, 'web')!.querySelector('.git-inline-error-text')?.textContent,
    'Failed to push: could not reach github.com'
  );
  assert.equal(section(el, 'api')!.querySelector('.git-inline-error'), null);
  assert.equal(panelError(el), null, 'the commit itself went through');
  dispose();
});

test('git panel: a repository that was only ahead is not pushed by a commit', async () => {
  statuses = {
    '/w/api': { ...changedIn('/w/api'), files: [], ahead: 1 },
    '/w/web': changedIn('/w/web'),
  };
  workspaceApp(['/w/api', '/w/web']);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  await typeAndCommit(el, 'feat: web');
  assert.deepEqual(
    sent('git_commit_many').map((a) => a.repos.map((r: { path: string }) => r.path)),
    [['/w/web']]
  );
  assert.deepEqual(pushed(), ['/w/web']);
  // Nothing left to commit: the button offers api's push.
  assert.equal(button(el, 'Push')?.disabled, false);
  dispose();
});

test("git panel: a folder's change refreshes its own repository's section only", async () => {
  statuses = { '/w/api': changedIn('/w/api'), '/w/web': changedIn('/w/web') };
  workspaceApp(['/w/api', '/w/web']);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  const reads = (path: string) => sent('git_status').filter((a) => a.path === path).length;
  const before = { api: reads('/w/api'), web: reads('/w/web') };
  statuses['/w/web'] = { ...statuses['/w/web'], branch: 'feature/cart' };
  gitChanged('/w/web');
  await flush();
  assert.equal(reads('/w/web'), before.web + 1);
  assert.equal(reads('/w/api'), before.api, "api's section isn't read again");
  assert.equal(section(el, 'web')!.querySelector('.git-branch-name')!.textContent, 'feature/cart');
  assert.equal(sent('git_repos').length, 1, 'the list stays: no repository came or went');
  dispose();
});

test('git panel: git init in a folder without a repository gives it a section of its own', async () => {
  statuses = { '/w/api': changedIn('/w/api') };
  let notesIsRepository = false;
  workspaceApp(['/w/api', '/w/notes']);
  backend.git_repos = () =>
    notesIsRepository
      ? reposOf(['/w/api', '/w/notes'])
      : { ...reposOf(['/w/api']), without: [{ path: '/w/notes', name: 'notes' }] };
  backend.git_status = (args) =>
    statuses[args.path] ??
    refuse(`Not a git repository: could not find repository at '${args.path}'`);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/notes']);
  await flush();
  const notes = () => section(el, 'notes')!;
  assert.match(notes().textContent!, /This folder isn't a Git repository yet\./);
  assert.ok(button(notes(), 'Initialize repository'));

  // `git init` in a terminal: the watcher says the folder is a repository now.
  notesIsRepository = true;
  statuses['/w/notes'] = { ...changedIn('/w/notes'), files: [] };
  gitChanged('/w/notes', true);
  await flush();
  assert.equal(sent('git_repos').length, 2, 'the list is read again');
  assert.equal(notes().querySelector('.git-branch-name')!.textContent, 'main');
  assert.doesNotMatch(notes().textContent!, /isn't a Git repository/);
  dispose();
});

/** A command's answer that comes when the test says. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}

/** Types `text` in the commit message box. */
async function typeMessage(el: HTMLElement, text: string) {
  const input = el.querySelector('.git-commit-input') as HTMLInputElement;
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

const commitButton = (el: HTMLElement) =>
  el.querySelector('.git-commit-button') as HTMLButtonElement;

/** A window whose folders and session the test switches, as App does. */
function mountSwitchable(first: { folders: string[]; session: string }) {
  const [folders, setFolders] = createSignal(first.folders);
  const [session, setSession] = createSignal(first.session);
  const el = container();
  const dispose = render(
    () => (
      <>
        <GitPanel
          folders={folders().map((path) => ({ path, name: path.split('/').pop()! }))}
          session={session()}
        />
        <ConfirmDialogHost />
      </>
    ),
    el
  );
  const show = async (next: { folders: string[]; session: string }) => {
    setFolders(next.folders);
    setSession(next.session);
    await flush();
  };
  return { el, dispose, show };
}

test('git panel: a commit across repositories keeps to its own workspace when the window switches while it runs', async () => {
  // Sessions of their own: what a workspace's commit leaves waiting is kept
  // by session for as long as the app runs.
  const shop = { folders: ['/a/api', '/a/web'], session: 'workspace:market' };
  const blog = { folders: ['/b/site', '/b/posts'], session: 'workspace:journal' };
  statuses = Object.fromEntries(
    [...shop.folders, ...blog.folders].map((path) => [path, changedIn(path)])
  );
  calls = [];
  const answer = deferred<unknown>();
  backend = {
    git_repos: (args) => reposOf(args.folders.map((folder: { path: string }) => folder.path)),
    git_status: (args) => statuses[args.path],
    git_commit_many: () => answer.promise,
    git_push: (args) => {
      statuses[args.path] = { ...statuses[args.path], ahead: 0 };
      return { result: 'pushed', message: null };
    },
  };
  const { el, dispose, show } = mountSwitchable(blog);
  await flush();
  await typeMessage(el, 'post: draft');
  await show(shop);
  await typeAndCommit(el, 'feat: shop');
  // While shop's hooks run, the window shows the blog again.
  await show(blog);
  statuses['/a/api'] = { ...statuses['/a/api'], files: [], ahead: 1 };
  answer.resolve({
    result: 'done',
    committed: ['/a/api'],
    failed: [{ path: '/a/web', error: 'failed to lock file' }],
  });
  await flush();
  assert.equal(message(el), 'post: draft', "the blog's message is its own");
  assert.equal(panelError(el), null, "shop's failure isn't the blog's");
  assert.deepEqual(pushed(), []);

  // Back in shop: its message for the retry, and what stopped it.
  await show(shop);
  assert.equal(message(el), 'feat: shop');
  assert.equal(
    panelError(el),
    'Committed in api; nothing is pushed until web is committed too.\nweb: failed to lock file'
  );
  dispose();
});

test("git panel: a hook that stopped one workspace's commit offers no skip in another", async () => {
  // Sessions of their own: what a workspace's commit leaves waiting is kept
  // by session for as long as the app runs.
  const shop = { folders: ['/a/api', '/a/web'], session: 'workspace:store' };
  const blog = { folders: ['/b/site', '/b/posts'], session: 'workspace:notes' };
  statuses = Object.fromEntries(
    [...shop.folders, ...blog.folders].map((path) => [path, changedIn(path)])
  );
  calls = [];
  backend = {
    git_repos: (args) => reposOf(args.folders.map((folder: { path: string }) => folder.path)),
    git_status: (args) => statuses[args.path],
    git_commit_many: (args) =>
      args.noVerify
        ? { result: 'done', committed: [], failed: [] }
        : {
            result: 'stopped',
            problems: [
              {
                path: '/a/web',
                error: 'Not committed: the pre-commit hook failed (exit code 1):\nlint',
              },
            ],
          },
  };
  const { el, dispose, show } = mountSwitchable(shop);
  await flush();
  await typeAndCommit(el, 'feat: shop');
  assert.ok(button(el, 'Commit without hooks'), 'offered in shop');

  await show(blog);
  assert.equal(panelError(el), null);
  assert.equal(button(el, 'Commit without hooks'), undefined, 'not in the blog');

  await show(shop);
  button(el, 'Commit without hooks')!.click();
  await flush();
  assert.deepEqual(
    sent('git_commit_many').map((a) => [
      a.noVerify,
      a.message,
      a.repos.map((r: { path: string }) => r.path),
    ]),
    [
      [false, 'feat: shop', ['/a/api', '/a/web']],
      [true, 'feat: shop', ['/a/api', '/a/web']],
    ],
    "the retry is shop's own"
  );
  dispose();
});

test('git panel: a retry commits the message only where it is not yet, though a repository that has it changed since', async () => {
  statuses = { '/w/api': changedIn('/w/api'), '/w/web': changedIn('/w/web') };
  const failing = new Set(['/w/web']);
  workspaceApp(['/w/api', '/w/web'], failing);
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  await typeAndCommit(el, 'feat: both');
  // A formatter's fix the pre-commit hook left in api after its commit.
  statuses['/w/api'] = { ...statuses['/w/api'], files: CHANGED.files };
  gitChanged('/w/api');
  await flush();
  failing.clear();
  commitButton(el).click();
  await flush();
  assert.deepEqual(
    sent('git_commit_many').map((a) => a.repos.map((r: { path: string }) => r.path)),
    [['/w/api', '/w/web'], ['/w/web']],
    'api has the message already'
  );
  assert.deepEqual(pushed(), ['/w/api', '/w/web']);
  dispose();
});

test('git panel: a repository left out of a commit reads what changed in it while the commit ran', async () => {
  statuses = { '/w/api': changedIn('/w/api'), '/w/docs': changedIn('/w/docs') };
  workspaceApp(['/w/api', '/w/docs']);
  const answer = deferred<unknown>();
  backend.git_commit_many = () => answer.promise;
  const { el, dispose } = mountWorkspace(['/w/api', '/w/docs']);
  await flush();
  box(el, 'docs').click();
  await flush();
  assert.equal(box(el, 'docs').checked, false, 'docs is left out');
  await typeAndCommit(el, 'feat: api');
  // A terminal switches docs' branch while api's hooks run.
  statuses['/w/docs'] = { ...statuses['/w/docs'], branch: 'feature/docs' };
  gitChanged('/w/docs');
  await flush();
  const branch = () => section(el, 'docs')!.querySelector('.git-branch-name')!.textContent;
  assert.equal(branch(), 'main', 'the section waits for the commit');
  statuses['/w/api'] = { ...statuses['/w/api'], files: [], ahead: 1 };
  answer.resolve({ result: 'done', committed: ['/w/api'], failed: [] });
  await flush();
  assert.equal(branch(), 'feature/docs');
  dispose();
});

test('git panel: a push that fails keeps its repository waiting for the rest of its message', async () => {
  statuses = { '/w/api': changedIn('/w/api'), '/w/web': changedIn('/w/web') };
  const failing = new Set(['/w/web']);
  workspaceApp(['/w/api', '/w/web'], failing);
  let offline = true;
  backend.git_push = (args) => {
    if (offline) return refuse('Failed to push: could not reach github.com');
    statuses[args.path] = { ...statuses[args.path], ahead: 0 };
    return { result: 'pushed', message: null };
  };
  const { el, dispose } = mountWorkspace(['/w/api', '/w/web']);
  await flush();
  await typeAndCommit(el, 'feat: both');
  // web left out for now: the button pushes api, which fails.
  box(el, 'web').click();
  await flush();
  assert.equal(commitButton(el).textContent?.trim(), 'Push');
  commitButton(el).click();
  await flush();
  assert.deepEqual(pushed(), ['/w/api']);
  // web back in, committed: api, still waiting, goes with it.
  offline = false;
  failing.clear();
  box(el, 'web').click();
  await flush();
  commitButton(el).click();
  await flush();
  assert.deepEqual(
    sent('git_commit_many').map((a) => a.repos.map((r: { path: string }) => r.path)),
    [['/w/api', '/w/web'], ['/w/web']]
  );
  assert.deepEqual(pushed(), ['/w/api', '/w/api', '/w/web']);
  dispose();
});
