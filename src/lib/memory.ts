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
  /** `provisional`, `verified` or `deprecated`: its trust state. */
  verification: MemoryState;
  /** What promoted or invalidated it; absent: nothing did. */
  evidence?: string;
  /** Whether the run's outcome says it helped; absent: it never got a verdict. */
  helped?: boolean;
}

/** A memory's trust state (rovik-protocol sends it as `verification`). */
export type MemoryState = 'provisional' | 'verified' | 'deprecated';

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
  /** Its trust state. */
  verification: MemoryState;
  /** What promoted or invalidated it; absent: nothing did. */
  evidence?: string;
  /** When it last changed, in milliseconds since the Unix epoch. */
  updatedAt: number;
}

/** How a trust state reads to the user: the panel and the manager say it alike. */
export function memoryStateLabel(state: string): string {
  return state === 'deprecated' ? 'invalidated' : state;
}

/**
 * The memories of the project in `root`, newest first: those whose title,
 * text, tags or files mention `query`, or every one of them when it's empty.
 */
export function memoryList(root: string, query: string): Promise<MemoryEntry[]> {
  return invoke<MemoryEntry[]>('memory_list', { root, query });
}

/**
 * Replaces a memory's title and text, the two its user can correct. Its trust
 * state and the evidence for it, its tags and its files stay as they are.
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
