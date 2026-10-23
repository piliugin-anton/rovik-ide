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
