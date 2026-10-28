// Component test: the Git tab's line counts follow a file that changes
// again while it's listed. A save makes the watcher send "git-changed",
// and the panel reads the status again but keeps the rows it has (so a
// click landing just then isn't lost): a row's `+N −N` must still show its
// file as it is now. They kept the counts of the save that listed the
// file, so the next save looked as if it never happened. The app is
// answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { GitPanel } from './GitPanel';
import type { GitStatus } from '../types';
import { oneRepository } from '../test/gitRepos';

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
/** What `git_status` answers now. */
let status: GitStatus;
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
    if (cmd === 'plugin:event|unlisten') return null;
    if (cmd === 'git_repos') return oneRepository('/p');
    if (cmd === 'git_status') return status;
    throw new Error(`no app: ${cmd}`);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

// A size for the changes list to show its rows in: happy-dom lays nothing
// out, and the virtualized list draws only the rows that fit.
for (const key of ['clientHeight', 'clientWidth', 'offsetHeight', 'offsetWidth'] as const) {
  const own = Object.getOwnPropertyDescriptor(HTMLElement.prototype, key);
  const base = own ?? Object.getOwnPropertyDescriptor(Element.prototype, key);
  Object.defineProperty(HTMLElement.prototype, key, {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains('git-files') ? 400 : (base?.get?.call(this) ?? 0);
    },
  });
}

/** The watcher says git's status may have changed (here: a save). */
function gitChanged() {
  for (const l of listeners.filter((l) => l.event === 'git-changed')) {
    callbacks.get(l.handler)!({
      event: 'git-changed',
      id: 1,
      payload: { folder: '/p', repositories: false },
    });
  }
}

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** The branch's status with `src/util.ts` changed by these many lines. */
const changed = (additions: number, deletions: number): GitStatus => ({
  branch: 'feature',
  ahead: 1,
  behind: 0,
  files: [
    { path: 'src/util.ts', status: 'M', staged: false, unstaged: true, additions, deletions },
  ],
  workdir: '/p',
  operation: null,
  upstream: null,
  push_remote: 'origin',
});

/** Each row of the changes list: its name and its counts. */
const rows = () =>
  [...document.querySelectorAll('.git-files .tree-row')].map((row) => [
    row.querySelector('.tree-row-name')?.textContent,
    row.querySelector('.git-diff-stats')?.textContent ?? '',
  ]);

let dispose = () => {};
after(() => dispose());

test("a listed file's line counts follow its next save", async () => {
  status = changed(2, 0);
  dispose = render(
    () => <GitPanel folders={[{ path: '/p', name: 'p' }]} session="/p" />,
    container()
  );
  await flush();
  assert.deepEqual(rows(), [
    ['src', '+2'],
    ['util.ts', '+2'],
  ]);

  // The file saved again: a line more, and one of git's gone.
  status = changed(3, 1);
  gitChanged();
  await flush();
  assert.deepEqual(rows(), [
    ['src', '+3−1'],
    ['util.ts', '+3−1'],
  ]);
  const stats = document.querySelector('.git-file-row .git-diff-stats') as HTMLElement;
  assert.equal(stats.title, '3 added lines, 1 removed');
});
