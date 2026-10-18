// gitPublish.ts
//
// A branch with no remote branch yet (new, never pushed, or its remote
// branch deleted): the Git panel's commit can't push it until it has one.
// Publishing makes it, on the remote the branch pushes to: the panel's
// push of such a branch creates its remote branch and tracks it
// (src-tauri/src/git.rs `git_push`), so later commits push on their own.
// After a commit the panel asks first; with nothing to commit, its button
// is Publish.
import type { GitStatus } from '../types';

/** Whether `s` is on a branch with no remote branch yet that a push can
 *  make: no upstream, a remote to push to, no merge or rebase under way. */
export function unpublished(s: GitStatus | null | undefined): s is GitStatus {
  return !!s && !s.operation && !s.upstream && !!s.push_remote;
}

/** What the commit button does: push the commits the remote branch lacks,
 *  commit what's changed, or publish the branch. */
export type CommitAction = 'push' | 'commit' | 'publish';

/** The commit button's action, with `message` written in its box. */
export function commitAction(s: GitStatus, message: string): CommitAction {
  if (s.ahead > 0) return 'push';
  const committing = !!message.trim() && s.files.length > 0;
  return !committing && unpublished(s) ? 'publish' : 'commit';
}

/** The commit button's label for `action`. */
export const COMMIT_LABELS: Record<CommitAction, string> = {
  push: 'Push',
  commit: 'Commit',
  publish: 'Publish',
};

/** What publishing `s`'s branch does, in a line (the Publish button's title). */
export function publishHint(s: GitStatus): string {
  return `Create ${s.branch} on ${s.push_remote} and push it`;
}

/** The question after a commit on a branch with no remote branch yet. */
export function publishQuestion(s: GitStatus): {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
} {
  const remote = s.push_remote ?? 'the remote';
  return {
    title: `Publish ${s.branch}?`,
    message:
      `${remote} has no branch ${s.branch} yet, so the commit wasn’t pushed.\n\n` +
      `Publishing creates ${s.branch} on ${remote} and pushes it there. Commits on it after that are pushed as they’re made.`,
    confirmLabel: 'Publish branch',
    cancelLabel: 'Not now',
  };
}
