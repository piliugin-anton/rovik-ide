// Component tests: the Git tab's branch menu, where a branch is switched
// to. It lists the local and remote branches, the current one marked,
// under a search as Rebase…'s list has: every word of it in a name, the
// arrow keys and Enter to pick a row, Escape to clear it before the menu
// closes. The app is answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { GitPanel } from './GitPanel';
import type { GitStatus } from '../types';

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
  ahead: 0,
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

function app() {
  calls = [];
  backend = {
    git_status: () => ON_FEATURE,
    git_branches: () => BRANCHES,
    git_stash_list: () => [],
    git_checkout: () => null,
  };
}

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
after(() => dispose());

async function panel() {
  dispose();
  dispose = render(() => <GitPanel projectPath="/work/app" />, container());
  await flush();
}

const all = <T extends Element = HTMLElement>(css: string) => [
  ...document.querySelectorAll<T>(css),
];
const text = (el: Element) => el.textContent?.trim() ?? '';
/** The branch menu (the "…" menu is a dropdown too). */
const menu = () =>
  document.querySelector<HTMLElement>('.git-branch-dropdown:not(.git-more-dropdown)');
const search = () => menu()!.querySelector<HTMLInputElement>('.git-history-search')!;
const labels = () => [...menu()!.querySelectorAll('.git-menu-section-label')].map(text);
const rows = () => [...menu()!.querySelectorAll('.context-menu-list .context-menu-item')].map(text);
const cursor = () => [...menu()!.querySelectorAll('.git-menu-cursor')].map(text);
const checkouts = () => calls.filter((c) => c.cmd === 'git_checkout').map((c) => c.args.branch);

async function openMenu() {
  all<HTMLButtonElement>('.git-branch-button')[0].click();
  await flush();
}

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

test('the branch menu lists local then remote branches under a search, the current one marked', async () => {
  app();
  await panel();
  await openMenu();
  assert.ok(menu(), 'the menu opened');
  assert.equal(document.activeElement, search(), 'the search takes the keyboard');
  assert.equal(search().placeholder, 'Search branches');
  assert.deepEqual(labels(), ['Local', 'Remote']);
  assert.deepEqual(rows(), ['feature', 'main', 'release-2', 'origin/feature', 'origin/main']);
  const current = [...menu()!.querySelectorAll('.git-branch-item-active')].map(text);
  assert.deepEqual(current, ['feature']);

  // Every word of the search, anywhere in the name, whatever the case.
  await type('orig ma');
  assert.deepEqual(labels(), ['Remote']);
  assert.deepEqual(rows(), ['origin/main']);
  await type('MAIN');
  assert.deepEqual(rows(), ['main', 'origin/main']);
  await type('nothing-like-it');
  assert.deepEqual(rows(), []);
  assert.ok(text(menu()!).includes('No branch matches.'));

  // Escape clears the search first, the menu staying; then it closes.
  await key('Escape');
  assert.equal(search().value, '');
  assert.equal(rows().length, 5);
  await key('Escape');
  assert.equal(menu(), null);
  assert.deepEqual(checkouts(), []);
});

test('a click on a row switches to that branch, and the menu closes', async () => {
  app();
  await panel();
  await openMenu();
  await type('main');
  [...menu()!.querySelectorAll<HTMLElement>('.context-menu-item')]
    .find((el) => text(el) === 'origin/main')!
    .click();
  await flush();
  assert.deepEqual(checkouts(), ['origin/main']);
  assert.equal(menu(), null);
});

test('the arrow keys and Enter pick a row, across both sections and in what the search left', async () => {
  app();
  await panel();
  await openMenu();
  assert.deepEqual(cursor(), ['feature']);
  for (let i = 0; i < 3; i++) await key('ArrowDown');
  assert.deepEqual(cursor(), ['origin/feature'], 'down into the remote branches');
  await key('ArrowUp');
  assert.deepEqual(cursor(), ['release-2']);
  // Typing starts the cursor on the first row that's left.
  await type('rel');
  assert.deepEqual(cursor(), ['release-2']);
  await key('Enter');
  assert.deepEqual(checkouts(), ['release-2']);
});

test('the menu opens again with no search', async () => {
  app();
  await panel();
  await openMenu();
  await type('orig');
  await openMenu();
  assert.equal(menu(), null, 'its button closes it');
  await openMenu();
  assert.equal(search().value, '');
  assert.equal(rows().length, 5);
});
