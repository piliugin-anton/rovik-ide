// changeReview.ts
//
// The change reviewer's records, as the chat shows them. When the agent
// finishes a change, a reviewer subagent reads it with fresh eyes before
// the project's checks run (src-tauri/src/agent_run/change_review.rs).
// Each review is a message with `meta.review`, never sent to the model.

export type FindingSeverity = 'high' | 'medium' | 'low';

export interface ReviewFinding {
  severity: FindingSeverity;
  /** As written: "src/a.ts:42". */
  location: string;
  path: string | null;
  line: number | null;
  title: string;
  scenario: string;
  fix: string;
  confidence: 'confirmed' | 'plausible';
  newEvidence?: string;
}

export type ReviewStatus =
  /** The reviewer is reading the change. */
  | 'running'
  /** It found something (high and medium ones went to the agent). */
  | 'findings'
  /** Nothing to report. */
  | 'clean'
  /** It couldn't review: failed, or its report couldn't be read. Never "no findings". */
  | 'skipped'
  | 'stopped';

export interface ReviewMeta {
  kind: 'change';
  status: ReviewStatus;
  round: number;
  maxRounds: number;
  files?: string[];
  changedLines?: number;
  findings?: ReviewFinding[];
  /** Findings an earlier round made, repeated without new evidence. */
  dropped?: ReviewFinding[];
  /** skipped: why. */
  reason?: string;
  childDialogId?: string | null;
  model?: string | null;
  modelsTried?: string[];
  toolCalls?: number;
  ms?: number;
  /** Out of budget: what it found so far. */
  partial?: boolean;
}

/** The record's first line. */
export function reviewHeadline(meta: ReviewMeta, interrupted: boolean): string {
  if (interrupted) return 'The review didn’t finish: the run ended while it was running.';
  const files = meta.files?.length ?? 0;
  const what = `${meta.changedLines ?? 0} changed line${meta.changedLines === 1 ? '' : 's'} in ${files} file${files === 1 ? '' : 's'}`;
  switch (meta.status) {
    case 'running':
      return `Reviewing the change (${what})…`;
    case 'clean':
      return 'The reviewer found nothing to fix.';
    case 'findings': {
      const all = meta.findings ?? [];
      const serious = all.filter((f) => f.severity !== 'low').length;
      const count = `${all.length} finding${all.length === 1 ? '' : 's'}`;
      return serious
        ? `The reviewer found ${count}: ${serious} sent to the agent to fix or answer.`
        : `The reviewer found ${count}, none serious enough to send to the agent.`;
    }
    case 'skipped':
      return `Review skipped: ${meta.reason ?? 'the reviewer couldn’t run'}. The checks decide.`;
    case 'stopped':
      return 'Review stopped.';
  }
}

/** "Review 1 of at most 3 · 12 tool calls · 41 s · model" */
export function reviewDetails(meta: ReviewMeta): string {
  const parts = [`Review ${meta.round} of at most ${meta.maxRounds}`];
  if (meta.toolCalls !== undefined) parts.push(`${meta.toolCalls} tool calls`);
  if (meta.ms !== undefined) parts.push(`${Math.round(meta.ms / 1000)} s`);
  if (meta.model) {
    const failed = (meta.modelsTried?.length ?? 1) - 1;
    parts.push(failed > 0 ? `${meta.model} (after ${failed} failed)` : meta.model);
  }
  if (meta.partial) parts.push('out of budget');
  return parts.join(' · ');
}
