// memory.ts
//
// The project's long-term memory (the verified-code-memory crate): what
// earlier runs of the project's chats learned, kept in one SQLite file per
// project in the app's data folder. A run recalls it when it starts and
// reports what became of it as `memoryRecalled` events (rovik-protocol),
// which the chat's "Relevant prior work" panel shows; Settings → Long-term
// memory shows how much it holds, turns it off for the project, and opens
// the manager that lists, corrects and removes what's there.
import { invoke } from '@tauri-apps/api/core';
import { plural } from './textUtils';

/** A memory a run recalled, as the chat's panel shows it. */
export interface RecalledMemory {
  id: string;
  /** `episode`, `project fact` or `procedure`. */
  kind: string;
  title: string;
  content: string;
  /** The project files it's about; absent: none. */
  files?: string[];
  /** `provisional`, `verified` or `deprecated`: what evidence supports it. */
  verification: MemoryState;
  /** `current`, `needs_revalidation` or `unknown`: whether that still holds. */
  applicability?: Applicability;
  /** What promoted or invalidated it; absent: nothing did. */
  evidence?: string;
  /** How the run that carried it ended; absent: the run checked nothing. */
  association?: Association;
}

/** A memory's trust state (rovik-protocol sends it as `verification`). */
export type MemoryState = 'provisional' | 'verified' | 'deprecated';

/** Whether a memory's claim is expected to hold for the code as it is now. */
export type Applicability = 'current' | 'needs_revalidation' | 'unknown';

/** How the run that had a memory in hand ended: an association, not a verdict. */
export type Association = 'checksPassed' | 'checksFailed';

/** How many memories one project has, by trust state. */
export interface MemoryStats {
  total: number;
  verified: number;
  provisional: number;
  deprecated: number;
}

/** The memory of the project in `root`: what Settings counts and shows. */
export function memoryStats(root: string): Promise<MemoryStats> {
  return invoke<MemoryStats>('memory_stats', { root });
}

/** The Settings menu's line for Long-term memory. */
export function memorySummary(stats: MemoryStats): string {
  if (stats.total === 0) return 'No memories yet: the agent adds them as it works';
  const states: [number, string][] = [
    [stats.verified, 'verified'],
    [stats.provisional, 'provisional'],
    [stats.deprecated, 'invalidated'],
  ];
  const parts = states.filter(([count]) => count > 0).map(([count, word]) => `${count} ${word}`);
  return `${plural(stats.total, 'memory', 'memories')}: ${parts.join(', ')}`;
}

/** A memory as Settings' manager lists it (rovik-core's `MemoryEntry`). */
export interface MemoryEntry {
  id: string;
  /** `episode`, `project fact` or `procedure`. */
  kind: string;
  /** One line: what it's about. */
  title: string;
  /** What it says. */
  content: string;
  /** The project files it's about. */
  files: string[];
  /** The tags retrieval matches it by. */
  tags: string[];
  /** What evidence supports it. */
  verification: MemoryState;
  /** Whether that still holds for the code as it is now. */
  applicability: Applicability;
  /** What kind of source asserted it: `user assertion`, `test result`, … */
  sourceKind: string;
  /** What promoted or invalidated it; absent: nothing did. */
  evidence?: string;
  /** When evidence last confirmed the claim, in milliseconds; absent: never. */
  lastValidatedAt?: number;
  /** When its title or text last changed, in milliseconds since the epoch. */
  updatedAt: number;
}

/** How a trust state reads to the user: the panel and the manager say it alike. */
export function memoryStateLabel(state: string): string {
  return state === 'deprecated' ? 'invalidated' : state;
}

/**
 * Whether a memory still applies, in the words the panel and the manager use.
 * Nothing for `current`: it is the ordinary case, and saying so on every row
 * would bury the ones that need attention.
 */
export function memoryApplicabilityLabel(state: string | undefined): string | undefined {
  if (state === 'needs_revalidation') return 'check again';
  if (state === 'unknown') return 'unchecked';
  return undefined;
}

/**
 * The memories of the project in `root`, newest first: those whose title,
 * text, tags or files mention `query`, or every one of them when it's empty.
 */
export function memoryList(root: string, query: string): Promise<MemoryEntry[]> {
  return invoke<MemoryEntry[]>('memory_list', { root, query });
}

/**
 * Replaces a memory's title and text, the two its user can correct. The
 * correction is the user's own assertion and carries no evidence, so the memory
 * is marked to be checked again and stops being one a run may rely on; its
 * tags, its files and its kind stay as they are.
 */
export function memoryEdit(
  root: string,
  id: string,
  title: string,
  content: string
): Promise<void> {
  return invoke<void>('memory_edit', { root, id, title, content });
}

/** Removes one memory of the project in `root` for good. */
export function memoryDelete(root: string, id: string): Promise<void> {
  return invoke<void>('memory_delete', { root, id });
}

/** Removes every memory of the project in `root`, and resolves how many went. */
export function memoryClear(root: string): Promise<number> {
  return invoke<number>('memory_clear', { root });
}
