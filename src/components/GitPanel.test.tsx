// Component tests: the Git panel when a git hook stops a commit, a push or
// a merge's commit (src-tauri/src/git/hooks.rs): the hook's output shows,
// with a button that tries once more without the hooks that check (git's
// --no-verify). The app is answered here, command by command. Run with:
// npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { GitPanel } from './GitPanel';
import type { GitOperation, GitStatus } from '../types';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
/** The commands the panel sent, in order. */
let calls: { cmd: string; args: any }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') return 1;
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

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
  const dispose = render(() => <GitPanel projectPath="/p" />, el);
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
