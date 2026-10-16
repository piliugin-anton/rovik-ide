// projectSessions.ts
//
// Each project's tabs and open folders, for coming back to it after
// another project was open: switching closed them all, and the project
// came back with none (only the one open at quit is restored, from
// last-project.json). Kept for the most recently left projects, in this
// webview's storage: a convenience, losing it loses nothing but tabs.
import type { OpenFile } from '../types';

const KEY = 'rovik.projectSessions';
/** How many projects' tabs are kept, the most recently left first. */
const LIMIT = 30;

export type SessionFile = Pick<OpenFile, 'id' | 'path' | 'name' | 'language'>;

export interface ProjectSession {
  files: SessionFile[];
  /** The tab that was shown. */
  active: string | null;
  /** Folders open in the tree. */
  expanded: string[];
}

type Stored = Record<string, ProjectSession & { at: number }>;

function read(): Stored {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? '{}');
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? (parsed as Stored) : {};
  } catch {
    return {};
  }
}

/** The project at `root` is being left: keep its tabs. */
export function rememberProjectSession(root: string, session: ProjectSession, now = Date.now()) {
  const all = read();
  all[root] = { ...session, at: now };
  const kept = Object.entries(all)
    .sort(([, a], [, b]) => (b.at ?? 0) - (a.at ?? 0))
    .slice(0, LIMIT);
  try {
    localStorage.setItem(KEY, JSON.stringify(Object.fromEntries(kept)));
  } catch {
    // Storage full or unavailable: the project just comes back without tabs.
  }
}

/** The tabs the project at `root` had when it was last left, if kept. */
export function projectSession(root: string): ProjectSession | null {
  const s = read()[root];
  if (!s || !Array.isArray(s.files)) return null;
  return {
    files: s.files.filter((f) => f && typeof f.path === 'string'),
    active: typeof s.active === 'string' ? s.active : null,
    expanded: Array.isArray(s.expanded) ? s.expanded.filter((p) => typeof p === 'string') : [],
  };
}
