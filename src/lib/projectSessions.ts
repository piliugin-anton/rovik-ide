// projectSessions.ts
//
// Each project's tabs and open folders, for coming back to it: switching
// projects closed them all, and the project came back with none (only the
// one open at quit is restored, from last-project.json). Saved as they
// change, for the most recent projects, and shared by the windows
// (lib/sharedState.ts): a project closed with its window comes back with
// its tabs in another, which last-project.json, one project's, can't do.
// A convenience: losing it loses nothing but tabs.
import type { OpenFile } from '../types';
import { sharedMap } from './sharedState';

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

type Stored = ProjectSession & { at: number };

const sessions = sharedMap('project-sessions', { key: KEY });

/** The project at `root` has these tabs now: kept for coming back to it. */
export function rememberProjectSession(
  root: string,
  session: ProjectSession,
  now = Date.now()
): Promise<void> {
  const all: Record<string, Partial<Stored>> = {
    ...(sessions.all() as Record<string, Partial<Stored>>),
    [root]: { ...session, at: now },
  };
  const older = Object.entries(all)
    .sort(([, a], [, b]) => (b.at ?? 0) - (a.at ?? 0))
    .slice(LIMIT)
    .map(([r]) => [r, null]);
  return sessions.set({ [root]: { ...session, at: now }, ...Object.fromEntries(older) });
}

/** The tabs the project at `root` had when it was last left, if kept. */
export function projectSession(root: string): ProjectSession | null {
  const s = sessions.all()[root] as Partial<Stored> | undefined;
  if (!s || typeof s !== 'object' || !Array.isArray(s.files)) return null;
  return {
    files: s.files.filter((f) => f && typeof f.path === 'string'),
    active: typeof s.active === 'string' ? s.active : null,
    expanded: Array.isArray(s.expanded) ? s.expanded.filter((p) => typeof p === 'string') : [],
  };
}

/** `projectSession`, as saved now: another window may have had the project since. */
export async function savedProjectSession(root: string): Promise<ProjectSession | null> {
  await sessions.refresh();
  return projectSession(root);
}
