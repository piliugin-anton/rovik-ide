// agentChanges.ts
//
// The agent's edits nobody reviewed (edits that didn't need approval, or
// were approved by the AI reviewer), shown in the editor until the user
// saves the file: added and changed lines green, removed ones marked red.
// Edits the user reviewed hunk by hunk aren't recorded here; they've been seen.
//
// Per file: the content before the agent's first edit since the last save,
// and the current one. Several edits in a row show as one set of changes.
// The editor marks its text against the baseline (Editor.tsx), so the
// marks hold when the text isn't quite what the agent wrote (a formatter
// ran on it, the user typed).
import { createSignal } from 'solid-js';
import { normalizePath } from './fileTools';
import { diffLines, splitLines, type DiffHunk } from './lineDiff';

export interface AgentChange {
  /** The file before the agent's first edit since the last save. */
  baseline: string;
  /** The file as the agent left it. */
  current: string;
  /** Changes from `baseline` to `current`. */
  hunks: DiffHunk[];
}

const [changes, setChanges] = createSignal<Record<string, AgentChange>>({});
const listeners = new Set<(absPath: string) => void>();

/** Records an edit the agent made to `absPath` (`before` null: a new file). */
export function recordAgentEdit(absPath: string, before: string | null, after: string) {
  const key = normalizePath(absPath);
  setChanges((all) => {
    const baseline = all[key]?.baseline ?? before ?? '';
    const hunks = diffLines(splitLines(baseline).lines, splitLines(after).lines);
    if (hunks.length === 0) {
      // Back to how it was: nothing to show.
      const { [key]: _gone, ...rest } = all;
      return rest;
    }
    return { ...all, [key]: { baseline, current: after, hunks } };
  });
  for (const listener of listeners) listener(absPath);
}

/** Unreviewed agent changes in a file (reactive). */
export function agentChangesFor(absPath: string): AgentChange | undefined {
  return changes()[normalizePath(absPath)];
}

/** The user saved the file: they've seen it. */
export function clearAgentChanges(absPath: string) {
  const key = normalizePath(absPath);
  if (!changes()[key]) return;
  setChanges(({ [key]: _gone, ...rest }) => rest);
}

/** App opens the file in a tab. */
export function onAgentEdit(listener: (absPath: string) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
