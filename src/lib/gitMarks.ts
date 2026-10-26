// gitMarks.ts
//
// Change marks against git in the editor (Editor.tsx, createGitMarks):
// added lines green, changed lines yellow, and the lines git has that the
// file no longer does shown in red where they were. For files opened from
// the Git tab (GitPanel) or a chat link to a file the agent wrote, compared
// with the version a revert would restore (src-tauri/src/git/changes.rs,
// git_base_content). They follow the text as it's edited, and the Git tab
// refreshes the base after commits, reverts and branch switches.
//
// Git takes a file's path relative to its repository, which isn't the
// project's folder when the project is inside one (`repo/app`'s
// `package.json` is `app/package.json`). A base keeps the repository and the
// path so a refresh asks the right repository, and a tab of a version of a
// file (`disk:`, `chat:`, a stash's, a commit's) is never refreshed at all.
//
// Distinct from the agent's highlights (lib/agentChanges.ts), which tint
// whole lines until saved: these are git's view and stay as long as the
// file differs from it.
import { createSignal } from 'solid-js';
import { normalizePath, toRelativePath } from './fileTools';
import { innermostFolder } from './openFolders';

interface Base {
  /** The repository's working folder: what a refresh asks git from. */
  workdir: string;
  /** Relative to the repository's working folder, for refreshing. */
  file: string;
  /** The earlier version; '' for a new file (everything is added). */
  content: string;
}

const [bases, setBases] = createSignal<Record<string, Base>>({});

export function setGitBase(absPath: string, workdir: string, file: string, content: string | null) {
  const key = normalizePath(absPath);
  setBases((all) => ({ ...all, [key]: { workdir, file, content: content ?? '' } }));
}

export function clearGitBase(absPath: string) {
  const key = normalizePath(absPath);
  if (!bases()[key]) return;
  setBases(({ [key]: _gone, ...rest }) => rest);
}

/** The base the editor compares a file with (reactive). */
export function gitBaseFor(absPath: string): string | undefined {
  return bases()[normalizePath(absPath)]?.content;
}

/** Files with marks, to refresh their bases. */
export function markedFiles(): { absPath: string; workdir: string; file: string }[] {
  return Object.entries(bases()).map(([absPath, base]) => ({
    absPath,
    workdir: base.workdir,
    file: base.file,
  }));
}

/**
 * The files with marks that are files on disk: a tab holding a version of a
 * file (`disk:`, `chat:`, a `remote-edit:` or `plan:` from a chat's host, a
 * stash's `stash@`, a commit's `commit@`, a context file) is compared with
 * what it holds, and git has nothing newer to say about it (section 12,
 * item 8). Nor is a base with no repository asked for: it would be asked with
 * an empty path, fail, and take its marks with it.
 */
export function refreshableFiles(): { absPath: string; workdir: string; file: string }[] {
  return markedFiles().filter((marked) => marked.workdir !== '' && onDisk(marked.absPath));
}

/** Whether a tab's text is a file on disk, rather than a version of one. */
export function onDisk(absPath: string): boolean {
  return !/^(?:disk|chat|context|remote-edit|plan):|^(?:stash|commit)@/.test(absPath);
}

/** A read of the window's repositories, kept for the folders it was made for. */
export interface WorkdirReads<T> {
  /** What `read` answered for `key`: the read in flight, or a new one. */
  of(key: string): Promise<T>;
  /** Git's state changed (a `git init`, a repository made): read again. */
  forget(): void;
}

/**
 * The working folders of the window's repositories, read once per set of
 * folders: `key` names that set (their paths). A read takes a moment, and a
 * tab can be opened before it answers — a file's marks wait for `of` rather
 * than finding no repository, which would leave the tab unmarked for good.
 */
export function createWorkdirReads<T>(read: () => Promise<T>): WorkdirReads<T> {
  let current: { key: string; done: Promise<T> } | null = null;
  return {
    of(key: string): Promise<T> {
      if (!current || current.key !== key) current = { key, done: read() };
      return current.done;
    },
    forget(): void {
      current = null;
    },
  };
}

/**
 * The file's path in the repository that holds it, among the window's (the
 * deepest working folder): a project at `repo/app` asks for its
 * `package.json` as `app/package.json`. Null for a path in none of them.
 */
export function repoFileOf(
  absPath: string,
  workdirs: readonly string[]
): { workdir: string; file: string } | null {
  const workdir = innermostFolder(workdirs, absPath);
  if (!workdir) return null;
  const file = toRelativePath(absPath, workdir);
  if (file === absPath || file === '') return null;
  return { workdir, file };
}
