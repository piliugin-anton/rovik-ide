// Component tests: the Git panel's Git hooks view (GitHooksView.tsx): a
// row per hook with its state, and what each row offers (open, create,
// enable, delete). The app is answered here, command by command. Run with:
// npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { GitHooksView } from './GitHooksView';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { GitHook, GitHooks } from '../lib/gitHooks';

let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
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

const hook = (name: string, state: GitHook['state'], path: string | null = null): GitHook => ({
  name,
  when: `when ${name} runs`,
  blocks: name.startsWith('pre'),
  state,
  path,
});

/** The repository's hooks, as the app keeps them in this test. */
function repository(): GitHooks {
  return {
    dir: '/p/.git/hooks',
    shownDir: '.git/hooks',
    source: 'default',
    needsExecutable: true,
    hooks: [
      hook('pre-commit', 'active', '/p/.git/hooks/pre-commit'),
      hook('commit-msg', 'not-executable', '/p/.git/hooks/commit-msg'),
      hook('pre-push', 'sample'),
      hook('post-checkout', 'missing'),
    ],
  };
}

function mount() {
  const opened: string[] = [];
  const el = container();
  const dispose = render(
    () => (
      <>
        <GitHooksView projectPath="/p" onOpen={(path) => opened.push(path)} />
        <ConfirmDialogHost />
      </>
    ),
    el
  );
  return { el, opened, dispose };
}

const row = (el: HTMLElement, name: string) =>
  [...el.querySelectorAll('.git-hook-row')].find((r) =>
    r.querySelector('.git-more-row-label')?.textContent?.startsWith(name)
  ) as HTMLElement;

const buttons = (r: HTMLElement) =>
  [...r.querySelectorAll('button')].map(
    (b) => b.textContent?.trim() || b.getAttribute('aria-label')
  );

test('git hooks view: each hook with its state, and what it offers', async () => {
  let hooks = repository();
  calls = [];
  backend = { git_hooks: () => hooks };
  const { el, dispose } = mount();
  await flush();

  assert.equal(el.querySelector('.git-more-note')?.textContent, 'In .git/hooks/');
  assert.equal(
    row(el, 'pre-commit').querySelector('.git-more-row-label')?.textContent,
    'pre-commit · on'
  );
  assert.deepEqual(buttons(row(el, 'pre-commit')), ['Delete the pre-commit hook']);
  assert.deepEqual(buttons(row(el, 'commit-msg')), ['Enable', 'Delete the commit-msg hook']);
  assert.match(row(el, 'commit-msg').title, /Not executable, so git and the Git panel skip it/);
  assert.equal(
    row(el, 'pre-push').querySelector('.git-more-row-label')?.textContent,
    'pre-push · example only'
  );
  assert.deepEqual(buttons(row(el, 'pre-push')), ['Create']);
  assert.equal(
    row(el, 'post-checkout').querySelector('.git-more-row-label')?.textContent,
    'post-checkout'
  );
  assert.deepEqual(buttons(row(el, 'post-checkout')), ['Create']);
  dispose();
});

test('git hooks view: a hook opens, is made and opened, and is made executable', async () => {
  let hooks = repository();
  calls = [];
  backend = {
    git_hooks: () => hooks,
    git_hook_create: ({ name }) => {
      const path = `/p/.git/hooks/${name}`;
      hooks = {
        ...hooks,
        hooks: hooks.hooks.map((h) => (h.name === name ? { ...h, state: 'active', path } : h)),
      };
      return path;
    },
    git_hook_make_executable: ({ name }) => {
      hooks = {
        ...hooks,
        hooks: hooks.hooks.map((h) => (h.name === name ? { ...h, state: 'active' } : h)),
      };
      return null;
    },
  };
  const { el, opened, dispose } = mount();
  await flush();

  row(el, 'pre-commit').click();
  assert.deepEqual(opened, ['/p/.git/hooks/pre-commit']);
  // A row with no hook opens nothing.
  row(el, 'post-checkout').click();
  assert.equal(opened.length, 1);

  (row(el, 'post-checkout').querySelector('button') as HTMLButtonElement).click();
  await flush();
  assert.deepEqual(calls.find((c) => c.cmd === 'git_hook_create')?.args, {
    path: '/p',
    name: 'post-checkout',
  });
  assert.deepEqual(opened, ['/p/.git/hooks/pre-commit', '/p/.git/hooks/post-checkout']);
  assert.equal(
    row(el, 'post-checkout').querySelector('.git-more-row-label')?.textContent,
    'post-checkout · on'
  );

  (row(el, 'commit-msg').querySelector('button') as HTMLButtonElement).click();
  await flush();
  assert.ok(
    calls.some((c) => c.cmd === 'git_hook_make_executable' && c.args.name === 'commit-msg')
  );
  assert.deepEqual(buttons(row(el, 'commit-msg')), ['Delete the commit-msg hook']);
  dispose();
});

test('git hooks view: deleting asks first, and an error shows', async () => {
  let hooks = repository();
  calls = [];
  backend = {
    git_hooks: () => hooks,
    git_hook_delete: () => Promise.reject('pre-commit: couldn’t move it to the trash: denied'),
  };
  const { el, dispose } = mount();
  await flush();

  const trash = () =>
    row(el, 'pre-commit').querySelector('[aria-label="Delete the pre-commit hook"]') as HTMLElement;
  trash().click();
  await flush();
  const cancel = [...document.querySelectorAll('.confirm-dialog button')].find(
    (b) => b.textContent === 'Cancel'
  ) as HTMLElement;
  cancel.click();
  await flush();
  assert.ok(!calls.some((c) => c.cmd === 'git_hook_delete'), 'nothing deleted without a yes');

  trash().click();
  await flush();
  const yes = [...document.querySelectorAll('.confirm-dialog button')].find(
    (b) => b.textContent === 'Delete'
  ) as HTMLElement;
  yes.click();
  await flush();
  assert.ok(calls.some((c) => c.cmd === 'git_hook_delete' && c.args.name === 'pre-commit'));
  assert.equal(
    el.querySelector('.git-more-note.error')?.textContent,
    'pre-commit: couldn’t move it to the trash: denied'
  );
  dispose();
});

test('git hooks view: where husky’s hooks are', async () => {
  calls = [];
  backend = {
    git_hooks: () => ({
      ...repository(),
      source: 'husky',
      shownDir: '.husky',
      needsExecutable: false,
    }),
  };
  const { el, dispose } = mount();
  await flush();
  assert.equal(el.querySelector('.git-more-note')?.textContent, 'In .husky/, run by husky');
  dispose();
});
