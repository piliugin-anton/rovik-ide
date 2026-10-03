// agentChanges.ts
//
// The agent's edits nobody reviewed (edits that didn't need approval, or
// were approved by the AI reviewer), shown in the editor until the user
// saves the file: added and changed lines green, removed ones marked red.
// Edits the user reviewed hunk by hunk aren't recorded here; they've been seen.
//
// Per file: the content before the agent's first edit since the last save,
// and the current one. Several edits in a row show as one set of changes.
import { createSignal } from 'solid-js';
import { diffLines, splitLines, type DiffHunk } from './lineDiff';

export interface AgentChange {
  /** The file as the agent left it; highlights apply only to this text. */
  current: string;
  /** Changes from the content before the agent's edits (lines of `current`: see the editor). */
  hunks: DiffHunk[];
}

interface Entry extends AgentChange {
  baseline: string;
}

const [changes, setChanges] = createSignal<Record<string, Entry>>({});
const listeners = new Set<(absPath: string) => void>();

/** Records an edit the agent made to `absPath` (`before` null: a new file). */
export function recordAgentEdit(absPath: string, before: string | null, after: string) {
  setChanges((all) => {
    const baseline = all[absPath]?.baseline ?? before ?? '';
    const hunks = diffLines(splitLines(baseline).lines, splitLines(after).lines);
    if (hunks.length === 0) {
      // Back to how it was: nothing to show.
      const { [absPath]: _gone, ...rest } = all;
      return rest;
    }
    return { ...all, [absPath]: { baseline, current: after, hunks } };
  });
  for (const listener of listeners) listener(absPath);
}

/** Unreviewed agent changes in a file (reactive). */
export function agentChangesFor(absPath: string): AgentChange | undefined {
  return changes()[absPath];
}

/** The user saved the file: they've seen it. */
export function clearAgentChanges(absPath: string) {
  if (!changes()[absPath]) return;
  setChanges(({ [absPath]: _gone, ...rest }) => rest);
}

/** App opens the file in a tab. */
export function onAgentEdit(listener: (absPath: string) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
