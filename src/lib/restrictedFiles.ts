// restrictedFiles.ts
//
// A project's restricted files: the files the agent may not read or change
// (src-tauri/crates/rovik-core/src/restricted_files.rs decides what's
// restricted; agent_run/restricted.rs refuses the agent's calls, and tells
// it why). Settings → Restricted files (RestrictedFilesSettings.tsx) shows
// the backend's table: the files found whose names say they hold
// credentials (.env files, keys, logins), and the user's own entries,
// which are kept with the project's settings and win over the names.
import { invoke } from '@tauri-apps/api/core';
import { updateProjectSettings } from './projectProfile';
import { plural } from './textUtils';

/** One entry of the user's list: what the agent may not do with a file. */
export interface RestrictedFile {
  /** Relative to the project, with `/`. A folder's entry is for its files too. */
  path: string;
  /** The agent may not read it. */
  blockRead: boolean;
  /** The agent may not change it. */
  blockWrite: boolean;
}

/** One line of the settings page's table, as the backend lists it. */
export interface RestrictedFileRow extends RestrictedFile {
  /** Its name says it holds credentials, and it's in the project. */
  detected: boolean;
  /** The user's list has an entry for it. */
  listed: boolean;
}

/** The table for the project in `root`: what's found, and the user's list. */
export function loadRestrictedFiles(root: string): Promise<RestrictedFileRow[]> {
  return invoke<RestrictedFileRow[]>('restricted_files', { root });
}

/**
 * Changes the user's list for the project: `change` is given the list as
 * it's saved, so changes made one after another each keep the one before
 * (a table shown before a change was saved doesn't undo it). An empty
 * list is removed.
 */
export function updateRestrictedFiles(
  root: string,
  change: (entries: RestrictedFile[]) => RestrictedFile[]
): Promise<void> {
  return updateProjectSettings(root, (saved) => {
    const next = change(savedEntries(saved.restrictedFiles));
    return { restrictedFiles: next.length > 0 ? next : undefined };
  });
}

/** The user's list as saved: an entry without a box blocks it, as the backend reads it. */
export function savedEntries(raw: unknown): RestrictedFile[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((e): e is Record<string, unknown> => !!e && typeof e === 'object')
    .filter((e) => typeof e.path === 'string' && e.path !== '')
    .map((e) => ({
      path: e.path as string,
      blockRead: e.blockRead !== false,
      blockWrite: e.blockWrite !== false,
    }));
}

/** The list with `entry` set: added, or in place of the one for its path. */
export function withEntry(entries: RestrictedFile[], entry: RestrictedFile): RestrictedFile[] {
  return [...entries.filter((e) => e.path !== entry.path), entry];
}

/** The list without the entry for `path`. */
export function withoutEntry(entries: RestrictedFile[], path: string): RestrictedFile[] {
  return entries.filter((e) => e.path !== path);
}

/**
 * Whether a typed path is a pattern (`*.pem`), which the list doesn't
 * match. Brackets are a name's (Next.js's `app/[id]/`).
 */
export function isPattern(input: string): boolean {
  return /[*?]/.test(input);
}

/**
 * A path the user typed, as the list keeps it: relative to the project in
 * `root`, with `/`, without `.` parts. null when it isn't inside the
 * project (`..`, another folder) or names the project itself.
 */
export function projectPath(input: string, root: string): string | null {
  let path = input.trim().replace(/\\/g, '/');
  const base = root.replace(/\\/g, '/').replace(/\/+$/, '');
  if (path === base) return null;
  if (path.startsWith(`${base}/`)) path = path.slice(base.length + 1);
  if (path.startsWith('/') || /^[a-zA-Z]:/.test(path)) return null;
  const parts: string[] = [];
  for (const part of path.split('/')) {
    if (part === '..') return null;
    if (part !== '' && part !== '.') parts.push(part);
  }
  return parts.length > 0 ? parts.join('/') : null;
}

/** The Settings menu's line for the page: what's kept from the agent. */
export function restrictedSummary(rows: RestrictedFileRow[]): string {
  if (rows.length === 0) return 'No files with credentials found';
  const kept = rows.filter((r) => r.blockRead || r.blockWrite).length;
  if (kept === 0) return 'Every file listed is open to the agent';
  return `${plural(kept, 'file')} kept from the agent`;
}
