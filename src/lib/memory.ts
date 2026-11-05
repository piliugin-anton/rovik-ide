// memory.ts
//
// The project's long-term memory (the verified-code-memory crate): what
// earlier runs of the project's chats learned, kept in one SQLite file per
// project in the app's data folder. A run recalls it when it starts and
// reports what became of it as `memoryRecalled` events (rovik-protocol),
// which the chat's "Relevant prior work" panel shows; Settings → Long-term
// memory shows how much it holds and turns it off for the project.
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
