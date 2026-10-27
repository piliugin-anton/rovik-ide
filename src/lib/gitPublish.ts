// gitPublish.ts
//
// The Git panel's commit button: what it does, and publishing a branch.
//
// A branch with no remote branch yet (new, never pushed, or its remote
// branch deleted): the Git panel's commit can't push it until it has one.
// Publishing makes it, on the remote the branch pushes to: the panel's
// push of such a branch creates its remote branch and tracks it
// (src-tauri/src/git.rs `git_push`), so later commits push on their own.
// After a commit the panel asks first; with nothing to commit, its button
// is Publish.
//
// With several repositories (a workspace's, D10) the button works over the
// ones included in the commit, and one commit message goes to each of them
// (D11, src-tauri/src/git/changes.rs `git_commit_many`); after it, one
// question publishes every branch it left with no remote branch.
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

/** Whether `s` has anything for the commit button: changes, commits to push,
 *  or a branch to publish. */
export function hasWork(s: GitStatus): boolean {
  return s.files.length > 0 || s.ahead > 0 || unpublished(s);
}

const conflicted = (s: GitStatus) => s.files.some((file) => file.status === 'U');

/** Why `name`'s repository is out of a commit across repositories now; null
 *  when nothing keeps it out. */
export function includeBlocked(s: GitStatus, name: string): string | null {
  if (s.operation) return `Finish the ${s.operation.kind} in ${name} first`;
  if (conflicted(s)) return `Resolve the conflicts in ${name} first`;
  return null;
}

/** Whether `s` can be in the next commit across repositories: nothing in the
 *  way, and something to commit or push. */
export function includable(s: GitStatus): boolean {
  return !s.operation && !conflicted(s) && hasWork(s);
}

/** The commit button's action over the repositories included in the commit,
 *  with several: commit when one has changes and there's a message, else
 *  push when one is ahead, else publish when one can be published. */
export function commitActionAcross(statuses: readonly GitStatus[], message: string): CommitAction {
  if (message.trim() && statuses.some((s) => s.files.length > 0)) return 'commit';
  if (statuses.some((s) => s.ahead > 0)) return 'push';
  if (statuses.some((s) => unpublished(s))) return 'publish';
  return 'commit';
}

/** A repository's status, with the name the panel shows it by. */
export interface NamedStatus {
  name: string;
  status: GitStatus;
}

/** `a`, `a and b`, `a, b and c`. */
function listed(items: readonly string[]): string {
  if (items.length <= 1) return items.join('');
  return `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`;
}

/** A branch by its repository: `api’s feature-x`. */
const branchOf = ({ name, status }: NamedStatus) => `${name}’s ${status.branch}`;

/** What publishing these repositories' branches does, in a line (the Publish
 *  button's title, with several). */
export function publishHintAcross(list: readonly NamedStatus[]): string {
  const each = list.map((one) => `${branchOf(one)} on ${one.status.push_remote}`);
  return `Create ${listed(each)} and push ${list.length === 1 ? 'it' : 'them'}`;
}

/** The one question after a commit across repositories, for every branch it
 *  left with no remote branch yet. */
export function publishQuestionAcross(list: readonly NamedStatus[]): {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
} {
  const one = list.length === 1;
  const each = list.map((named) => `${branchOf(named)} (${named.status.push_remote})`);
  return {
    title: `Publish ${listed(list.map(branchOf))}?`,
    message:
      `${one ? 'This branch has' : 'These branches have'} no remote branch yet, so ` +
      `${one ? 'its commit wasn’t' : 'their commits weren’t'} pushed: ${listed(each)}.\n\n` +
      `Publishing creates ${one ? 'it' : 'each one'} on its remote and pushes it there. ` +
      `Commits on ${one ? 'it' : 'them'} after that are pushed as they’re made.`,
    confirmLabel: one ? 'Publish branch' : 'Publish branches',
    cancelLabel: 'Not now',
  };
}

/** A repository's problem in a commit across repositories, by its name. */
export interface NamedProblem {
  name: string;
  error: string;
}

/** What the panel says when a check or a hook stopped a commit across
 *  repositories before any was made: each problem, by its repository. */
export function stoppedText(problems: readonly NamedProblem[]): string {
  const lines = problems.map(
    ({ name, error }) => `${name}: ${error.replace(/^Not committed: /, '')}`
  );
  return ['Nothing was committed.', ...lines].join('\n');
}

/** What the panel says when a commit across repositories failed in some
 *  after it was made in others (`committed`: every repository this message is
 *  in so far): nothing is pushed until the rest are committed too. */
export function partialText(committed: readonly string[], failed: readonly NamedProblem[]): string {
  const errors = failed.map(({ name, error }) => `${name}: ${error}`);
  if (committed.length === 0) return ['Nothing was committed.', ...errors].join('\n');
  const rest = listed(failed.map((problem) => problem.name));
  return [
    `Committed in ${listed(committed)}; nothing is pushed until ${rest} ${failed.length === 1 ? 'is' : 'are'} committed too.`,
    ...errors,
  ].join('\n');
}
