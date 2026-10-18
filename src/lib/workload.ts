// workload.ts
//
// Heavy commands' slots (src-tauri/crates/rovik-core/src/workload): how
// many CPU-heavy commands (builds, tests, type checks) run at once on this
// computer, from all chats and the terminal together. Set in Settings →
// Heavy commands and kept by the backend for the next start.
import { invoke } from '@tauri-apps/api/core';
import { plural } from './textUtils';

export interface WorkloadStatus {
  /** As set; null: auto. */
  slots: number | null;
  /** What auto gives on this machine now. */
  autoSlots: number;
  maxSlots: number;
  /** Commands holding a slot, and ones waiting for one. */
  busy: number;
  waiting: number;
  cores: number;
  memoryGb: number;
}

export function workloadStatus(): Promise<WorkloadStatus> {
  return invoke<WorkloadStatus>('workload_status');
}

/** `null`: auto. A number: that many slots, kept for the next start. */
export function setWorkloadSlots(slots: number | null): Promise<void> {
  return invoke<void>('workload_set_slots', { slots });
}

/** The machine as auto sees it: "8 cores, 16 GB". */
export function describeMachine(status: Pick<WorkloadStatus, 'cores' | 'memoryGb'>): string {
  return `${plural(status.cores, 'core')}, ${status.memoryGb} GB`;
}

/** One line for the Settings menu: "Auto: 1 slot now" or "Fixed: 2 slots". */
export function workloadSummary(status: WorkloadStatus): string {
  const now =
    status.busy === 0 && status.waiting === 0
      ? ''
      : ` · ${plural(status.busy, 'heavy command')} running${status.waiting ? `, ${status.waiting} waiting` : ''}`;
  return status.slots === null
    ? `Auto: ${plural(status.autoSlots, 'slot')} on this machine${now}`
    : `Fixed: ${plural(status.slots, 'slot')}${now}`;
}
