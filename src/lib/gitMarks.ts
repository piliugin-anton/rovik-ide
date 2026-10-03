// gitMarks.ts
//
// Change marks against git, in the editor's gutter: a green bar on added
// lines, a blue one on changed lines, a red marker where lines were
// removed. For files opened from the Git tab (GitPanel), compared with the
// version a revert would restore (src-tauri/src/git.rs, git_base_content).
// They follow the text as it's edited, and the Git tab refreshes the base
// after commits, reverts and branch switches.
//
// Distinct from the agent's highlights (lib/agentChanges.ts), which tint
// whole lines until saved: these are git's view and stay as long as the
// file differs from it.
import { createSignal } from 'solid-js';

interface Base {
  /** Relative to the repository's working folder, for refreshing. */
  file: string;
  /** The earlier version; '' for a new file (everything is added). */
  content: string;
}

const [bases, setBases] = createSignal<Record<string, Base>>({});

export function setGitBase(absPath: string, file: string, content: string | null) {
  setBases((all) => ({ ...all, [absPath]: { file, content: content ?? '' } }));
}

export function clearGitBase(absPath: string) {
  if (!bases()[absPath]) return;
  setBases(({ [absPath]: _gone, ...rest }) => rest);
}

/** The base the editor compares a file with (reactive). */
export function gitBaseFor(absPath: string): string | undefined {
  return bases()[absPath]?.content;
}

/** Files with marks, to refresh their bases. */
export function markedFiles(): { absPath: string; file: string }[] {
  return Object.entries(bases()).map(([absPath, b]) => ({ absPath, file: b.file }));
}
