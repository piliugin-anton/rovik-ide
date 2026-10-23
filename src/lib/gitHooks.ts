// gitHooks.ts
//
// A git hook that stopped a commit, a push or a merge's commit in the Git
// panel (src-tauri/src/git/hooks.rs) can be skipped for one more try, as
// git's --no-verify skips it. Only the hooks that check what's committed or
// pushed are skipped (CHECKS in hooks.rs): prepare-commit-msg and the hooks
// that come after still run. The panel tells which hook it was from the
// error, whose start hooks.rs writes.

/** The hooks git's --no-verify skips (hooks.rs CHECKS). */
const CHECKS = ['pre-commit', 'commit-msg', 'pre-merge-commit', 'pre-push'];

/** The hook that stopped the action `error` is about, when skipping it is
 *  a way past: a check that failed, or couldn't start (on Windows, without
 *  Git for Windows' sh). Null for any other error. */
export function skippableHook(error: string): string | null {
  const m = /^Not (?:committed|pushed): the ([a-z-]+) hook (?:failed|couldn't start)/.exec(error);
  return m && CHECKS.includes(m[1]) ? m[1] : null;
}

/** What a skip offer's button does, for its tooltip. */
export const SKIP_HINTS = {
  commit: 'Commits once more without the pre-commit and commit-msg hooks (git commit --no-verify)',
  merge:
    'Commits the merge without the pre-merge-commit, pre-commit and commit-msg hooks (--no-verify)',
  push: 'Pushes once more without the pre-push hook (git push --no-verify)',
} as const;

/** The repository's hooks, for the Git hooks view (git_hooks in
 *  src-tauri/src/git/hook_files.rs). */
export interface GitHooks {
  /** The folder they're in (it may not exist yet). */
  dir: string;
  /** The folder as shown: from the working folder when it's in it. */
  shownDir: string;
  /** .git/hooks, core.hooksPath's folder, or husky's `.husky/`. */
  source: 'default' | 'hooksPath' | 'husky';
  /** A hook must be executable to run (macOS, Linux; not husky's). */
  needsExecutable: boolean;
  hooks: GitHook[];
}

export interface GitHook {
  name: string;
  /** When it runs, and what it's given. */
  when: string;
  /** It runs before, and failing stops the action. */
  blocks: boolean;
  /** not-executable: git skips it. sample: only git's example, which doesn't run. */
  state: 'active' | 'not-executable' | 'sample' | 'missing';
  path: string | null;
}

/** A hook's state, as its row says it (nothing for one that isn't there). */
export const HOOK_STATE_LABELS: Record<GitHook['state'], string> = {
  active: 'on',
  'not-executable': 'not executable',
  sample: 'example only',
  missing: '',
};

/** Where the hooks are, as the view's heading note says it. */
export function hooksPlace(h: GitHooks): string {
  switch (h.source) {
    case 'husky':
      return `In ${h.shownDir}/, run by husky`;
    case 'hooksPath':
      return `In ${h.shownDir}/ (set by core.hooksPath)`;
    default:
      return `In ${h.shownDir}/`;
  }
}

/** A hook's row tooltip: when it runs, and what its state means. */
export function hookTitle(hook: GitHook): string {
  const state = {
    active: '',
    'not-executable':
      '\nNot executable, so git and the Git panel skip it: Enable makes it executable.',
    sample: `\nOnly git’s example (${hook.name}.sample) is there, which doesn’t run.`,
    missing: '',
  }[hook.state];
  return `${hook.name}: ${hook.when}${state}`;
}
