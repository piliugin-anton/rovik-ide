// viewStates.ts
//
// Where each file's editor was: its scroll position, cursor, selections
// and folded regions. The editor is created anew each time its tab is
// shown (App.tsx mounts one per active tab), so without this, switching
// tabs put every file back at its first line with nothing selected.
// Kept by path for the most recently left files; a file left long ago
// opens at its top.

/** How many files' places are kept, the most recently left first. */
const LIMIT = 200;

const states = new Map<string, unknown>();

/** The editor of `path` is going away: keep where it was. */
export function rememberViewState<T>(path: string, state: T | null | undefined) {
  states.delete(path);
  if (state == null) return;
  states.set(path, state);
  while (states.size > LIMIT) {
    const oldest = states.keys().next().value;
    if (oldest === undefined) break;
    states.delete(oldest);
  }
}

/** Where the editor of `path` was when it was last left, if it's kept. */
export function recallViewState<T>(path: string): T | undefined {
  return states.get(path) as T | undefined;
}
