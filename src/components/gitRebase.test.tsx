// Component tests: the Git tab's "…" menu, Rebase…. It sits in the Branch
// group and opens a list of local and remote branches, the current one left
// out, with a search over it. Picking one rebases the current branch onto
// it; the list says so when there was nothing to do, and asks first when
// the rebase would rewrite commits already pushed. The app is answered
// here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { GitPanel } from './GitPanel';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { GitOperation, GitPullOutcome, GitStatus } from '../types';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') return 0;
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

const ON_FEATURE: GitStatus = {
  branch: 'feature',
  behind: 0,
  ahead: 2,
  files: [],
  workdir: '/work/app',
  operation: null,
  upstream: 'origin/feature',
  push_remote: 'origin',
};
const BRANCHES = [
  { name: 'feature', is_remote: false },
  { name: 'main', is_remote: false },
  { name: 'release-2', is_remote: false },
  { name: 'origin/feature', is_remote: true },
  { name: 'origin/main', is_remote: true },
];
const outcome = (result: GitPullOutcome['result']): GitPullOutcome => ({ result, conflicts: [] });

/** The repository is in `status`; each rebase answers the next of `rebases`. */
function app(status: GitStatus, rebases: GitPullOutcome[] = []) {
  calls = [];
  backend = {
    git_status: () => status,
    git_branches: () => BRANCHES,
    git_stash_list: () => [],
    git_rebase: () => {
      const next = rebases.shift();
      if (!next) throw new Error('an unexpected rebase');
      return next;
    },
  };
}

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
after(() => dispose());

async function panel() {
  dispose();
  dispose = render(
    () => (
      <>
        <GitPanel projectPath="/work/app" />
        <ConfirmDialogHost />
      </>
    ),
    container()
  );
  await flush();
}

const all = <T extends Element = HTMLElement>(css: string) => [
  ...document.querySelectorAll<T>(css),
];
const text = (el: Element) => el.textContent?.trim() ?? '';
const menu = () => document.querySelector<HTMLElement>('.git-more-dropdown');

async function openMenu() {
  all<HTMLButtonElement>('.git-more-trigger')[0].click();
  await flush();
}

/** The open menu's item that reads `label`. */
function item(label: string): HTMLElement {
  const found = all('.git-more-dropdown .context-menu-item').find((el) => text(el) === label);
  assert.ok(found, `no item ${label}`);
  return found;
}

async function openRebase() {
  await openMenu();
  item('Rebase…').click();
  await flush();
}

const search = () => menu()!.querySelector<HTMLInputElement>('.git-history-search')!;
const labels = () => all('.git-more-dropdown .git-menu-section-label').map(text);
const rows = () => all('.git-more-dropdown .context-menu-list .context-menu-item').map(text);
const rebases = () => calls.filter((c) => c.cmd === 'git_rebase').map((c) => c.args);

async function type(value: string) {
  const input = search();
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

async function key(name: string) {
  search().dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true }));
  await flush();
}

test('Rebase… is in the Branch group, between New branch and Delete branch', async () => {
  app(ON_FEATURE);
  await panel();
  await openMenu();
  const branchGroup = all('.git-more-dropdown .git-menu-section-label').find(
    (l) => text(l) === 'Branch'
  )!;
  const list = branchGroup.nextElementSibling!;
  assert.deepEqual([...list.querySelectorAll('.context-menu-item')].map(text), [
    'New branch…',
    'Rebase…',
    'Delete branch…',
  ]);
});

test('it lists local then remote branches, the current one left out, under a search', async () => {
  app(ON_FEATURE);
  await panel();
  await openRebase();
  assert.ok(text(menu()!).startsWith('Rebase feature onto'));
  assert.equal(document.activeElement, search());
  assert.deepEqual(labels(), ['Local', 'Remote']);
  assert.deepEqual(rows(), ['main', 'release-2', 'origin/feature', 'origin/main']);

  // Every word of the search, anywhere in the name.
  await type('orig ma');
  assert.deepEqual(labels(), ['Remote']);
  assert.deepEqual(rows(), ['origin/main']);
  await type('MAIN');
  assert.deepEqual(rows(), ['main', 'origin/main']);
  await type('nothing-like-it');
  assert.deepEqual(rows(), []);
  assert.ok(text(menu()!).includes('No branch matches.'));

  // Escape clears the search first; the menu stays.
  await key('Escape');
  assert.equal(search().value, '');
  assert.equal(rows().length, 4);
  assert.deepEqual(rebases(), []);
});

test('a pick rebases onto that branch, and the menu closes', async () => {
  app(ON_FEATURE, [outcome('rebased')]);
  await panel();
  await openRebase();
  const before = calls.filter((c) => c.cmd === 'git_status').length;
  all('.git-more-dropdown .context-menu-item')
    .find((el) => text(el) === 'origin/main')!
    .click();
  await flush();
  assert.deepEqual(rebases(), [{ path: '/work/app', onto: 'origin/main', remote: true }]);
  assert.equal(menu(), null);
  assert.ok(calls.filter((c) => c.cmd === 'git_status').length > before, 'the status refreshed');
});

test('the arrow keys and Enter pick a row', async () => {
  app(ON_FEATURE, [outcome('rebased')]);
  await panel();
  await openRebase();
  const cursor = () => all('.git-more-dropdown .git-menu-cursor').map(text);
  assert.deepEqual(cursor(), ['main']);
  await key('ArrowDown');
  await key('ArrowDown');
  await key('ArrowUp');
  assert.deepEqual(cursor(), ['release-2']);
  await key('Enter');
  assert.deepEqual(rebases(), [{ path: '/work/app', onto: 'release-2', remote: false }]);
});

test('with nothing to do, the list says so and stays', async () => {
  app(ON_FEATURE, [outcome('up-to-date')]);
  await panel();
  await openRebase();
  all('.git-more-dropdown .context-menu-item')
    .find((el) => text(el) === 'main')!
    .click();
  await flush();
  assert.ok(menu());
  assert.equal(
    text(menu()!.querySelector('.git-rebase-note')!),
    'feature already has everything on main: nothing to rebase.'
  );
});

test('commits already pushed: it asks first, and rebases only when told to', async () => {
  app(ON_FEATURE, [outcome('rewrites-pushed'), outcome('rewrites-pushed'), outcome('rebased')]);
  await panel();
  await openRebase();
  const pick = async () => {
    all('.git-more-dropdown .context-menu-item')
      .find((el) => text(el) === 'main')!
      .click();
    await flush();
  };
  const dialogButton = (label: string) =>
    all<HTMLButtonElement>('.confirm-dialog button').find((b) => text(b) === label)!;

  await pick();
  assert.ok(text(document.querySelector('.confirm-dialog')!).includes('origin/feature'));
  dialogButton('Cancel').click();
  await flush();
  assert.equal(rebases().length, 1);
  assert.ok(menu(), 'the list stays');

  await pick();
  dialogButton('Rebase anyway').click();
  await flush();
  assert.deepEqual(rebases().slice(1), [
    { path: '/work/app', onto: 'main', remote: false },
    { path: '/work/app', onto: 'main', remote: false, rewritePushed: true },
  ]);
  assert.equal(menu(), null);
});

test('during a merge, Rebase… is greyed out and says why', async () => {
  const merging: GitOperation = {
    kind: 'merge',
    branch: 'feature',
    incoming: 'main',
    step: null,
    total: null,
    commit: null,
    conflicts: ['a.txt'],
    canContinue: true,
  };
  app({ ...ON_FEATURE, operation: merging });
  await panel();
  await openMenu();
  const rebase = item('Rebase…');
  assert.ok(rebase.classList.contains('git-menu-disabled'));
  assert.equal(rebase.title, 'A merge is in progress: finish or abort it first');
  rebase.click();
  await flush();
  assert.equal(menu()!.querySelector('.git-history-search'), null);
});
