// autopilot.ts
//
// Autopilot's stage records, as the chat holds them
// (docs/design/autopilot-plan.md, 3.3): a task is a pipeline of stages,
// and each leaves an empty assistant message with `meta.stage`, which the
// backend writes and updates as the task goes on
// (src-tauri/crates/rovik-core/src/agent_run/autopilot.rs). The task's
// state is these records alone, so what the chat shows of it is right
// after a restart and in a remote chat's mirror, and nothing here keeps
// state of its own.
import type { ChatMessage } from '../types';

export type StageName =
  | 'plan'
  | 'planReview'
  | 'prepare'
  | 'tests'
  | 'implement'
  | 'review'
  | 'checks'
  | 'break'
  | 'pick'
  | 'acceptance'
  | 'deliver';

export type StageStatus = 'running' | 'done' | 'failed' | 'skipped';

/** How much a task spends on independence (AutopilotConfig.effort). */
export type AutopilotEffort = 'balanced' | 'thorough';

/** `meta.stage` (agent_run/autopilot.rs `StageRecord`). */
export interface StageMeta {
  /** The task: the id of the message that started it. */
  task: string;
  name: StageName;
  status: StageStatus;
  /** The stage's round, from 1. */
  round: number;
  effort: AutopilotEffort;
  /** Thorough: the working copy it ran in. */
  candidate?: string;
  /** A run stage's: how its run ended; a failed stage's: the task's outcome. */
  outcome?: string;
  /** The record it produced (a review, a check round, a reply), by message id. */
  ref?: string;
  /** One line for the strip. */
  summary?: string;
}

/** A stage record and the message that holds it. */
export interface StageRow {
  id: string;
  stage: StageMeta;
}

/** Every stage, in the order a task runs them. */
export const STAGES: StageName[] = [
  'plan',
  'planReview',
  'prepare',
  'tests',
  'implement',
  'review',
  'checks',
  'break',
  'pick',
  'acceptance',
  'deliver',
];

/** Only Thorough has these (3.2). */
const THOROUGH_ONLY: StageName[] = ['break', 'pick'];

/** The stages of a task of this effort, in order. */
export function pipeline(effort: AutopilotEffort): StageName[] {
  return STAGES.filter((s) => effort === 'thorough' || !THOROUGH_ONLY.includes(s));
}

const STATUSES: StageStatus[] = ['running', 'done', 'failed', 'skipped'];

/** A message's stage record, if it holds one that reads. */
export function stageOf(m: ChatMessage): StageMeta | null {
  const stage = m.role === 'assistant' ? m.meta?.stage : undefined;
  if (!stage || typeof stage.task !== 'string') return null;
  if (!STAGES.includes(stage.name) || !STATUSES.includes(stage.status)) return null;
  return stage;
}

/** The stage records among `messages`, oldest first. */
export function stageRows(messages: ChatMessage[]): StageRow[] {
  const rows: StageRow[] = [];
  for (const m of messages) {
    const stage = stageOf(m);
    if (stage) rows.push({ id: m.id, stage });
  }
  return rows;
}

/**
 * The chat's latest task, while it's the chat's current request: its
 * records. A message of the user's after it takes over from it.
 */
export function currentTask(messages: ChatMessage[]): StageRow[] | null {
  const rows = stageRows(messages);
  const last = rows[rows.length - 1];
  if (!last) return null;
  const task = rows.filter((r) => r.stage.task === last.stage.task);
  const from = messages.findIndex((m) => m.id === task[0].id);
  const later = messages.slice(from + 1);
  if (later.some((m) => m.role === 'user' && !m.meta?.harness && !m.failed)) return null;
  return task;
}

/** Whether a task is over: delivered, or stopped by the user. */
export function taskOver(rows: StageRow[]): boolean {
  const last = rows[rows.length - 1]?.stage;
  if (!last) return true;
  if (last.status === 'running') return false;
  return last.name === 'deliver' || (last.status === 'failed' && last.outcome === 'stopped');
}

/** A task the backend picks up again when asked to resume the chat. */
export function unfinishedTask(messages: ChatMessage[]): boolean {
  const task = currentTask(messages);
  return !!task && !taskOver(task);
}
