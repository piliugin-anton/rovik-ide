// changeReview.ts
//
// The change reviewer's records, as the chat shows them. When the agent
// finishes a change, a reviewer subagent reads it with fresh eyes before
// the project's checks run (src-tauri/src/agent_run/change_review.rs).
// Each review is a message with `meta.review`, never sent to the model.
import { modelLabel } from './subagents';
import { plural } from './textUtils';

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
  /** A plan review's finding: the step it's about. */
  step?: number;
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
  /** change: a finished change, before the checks; plan: a submitted plan (plan_review.rs). */
  kind: 'change' | 'plan';
  status: ReviewStatus;
  /** Absent when no review ran because the run's reviews were used. */
  round?: number;
  maxRounds: number;
  files?: string[];
  changedLines?: number;
  findings?: ReviewFinding[];
  /** Findings an earlier round made, repeated without new evidence. */
  dropped?: ReviewFinding[];
  /** skipped: why. */
  reason?: string;
  /** skipped for its format: the report as the reviewer wrote it. */
  report?: string;
  childDialogId?: string | null;
  model?: string | null;
  modelsTried?: string[];
  toolCalls?: number;
  ms?: number;
  /** Out of budget: what it found so far. */
  partial?: boolean;
  /** plan: its findings kept the plan from being saved, for the agent to fix. */
  held?: boolean;
  /** plan: findings still open after the last round, saved at the end of the plan. */
  open?: number;
  /** change: R08's review once the checks pass after a repair: only high findings go back. */
  afterGreen?: boolean;
  usage?: import('./subagents').SubagentUsage;
}

/** The record's first line. */
export function reviewHeadline(meta: ReviewMeta, interrupted: boolean): string {
  if (interrupted) return 'The review didn’t finish: the run ended while it was running.';
  if (meta.kind === 'plan') return planHeadline(meta);
  const files = meta.files?.length ?? 0;
  const what = `${plural(meta.changedLines ?? 0, 'changed line')} in ${plural(files, 'file')}`;
  switch (meta.status) {
    case 'running':
      return `Reviewing the change (${what})…`;
    case 'clean':
      return meta.reason
        ? `The reviewer reported nothing to fix: ${meta.reason}.`
        : 'The reviewer found nothing to fix.';
    case 'findings': {
      const all = meta.findings ?? [];
      // P5.5: after green, only high findings go back to the agent.
      const serious = all.filter((f) =>
        meta.afterGreen ? f.severity === 'high' : f.severity !== 'low'
      ).length;
      const count = plural(all.length, 'finding');
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

function planHeadline(meta: ReviewMeta): string {
  switch (meta.status) {
    case 'running':
      return 'Reviewing the plan against the code…';
    case 'clean':
      return meta.reason
        ? `The plan reviewer reported nothing to fix: ${meta.reason}.`
        : 'The plan reviewer found nothing to fix.';
    case 'findings': {
      const all = meta.findings ?? [];
      const count = plural(all.length, 'finding');
      if (meta.held)
        return `The plan reviewer found ${count}: the plan goes back to the agent before it’s saved.`;
      if (meta.open)
        return `The plan reviewer still found ${count} after its last round: saved at the end of the plan, under “Open review findings”.`;
      return `The plan reviewer found ${count}, none serious enough to hold the plan back.`;
    }
    case 'skipped':
      return `Plan review skipped: ${meta.reason ?? 'the reviewer couldn’t run'}. The plan was saved as it was.`;
    case 'stopped':
      return 'Plan review stopped.';
  }
}

/** The record's facts, for the card's "Details" expander: label and value. */
export function reviewFacts(meta: ReviewMeta): [string, string][] {
  const facts: [string, string][] = [
    [
      'What',
      meta.kind === 'plan' ? 'The plan, before it was saved' : 'The change, before the checks',
    ],
    ['Status', meta.status + (meta.partial ? ' (out of budget)' : '')],
  ];
  if (meta.reason) facts.push(['Reason', meta.reason]);
  facts.push([
    'Round',
    meta.round === undefined
      ? `none: all ${meta.maxRounds} used`
      : `${meta.round} of at most ${meta.maxRounds}`,
  ]);
  if (meta.model) facts.push(['Model', modelLabel(meta.model, meta.modelsTried)]);
  if (meta.toolCalls !== undefined) facts.push(['Tool calls', String(meta.toolCalls)]);
  if (meta.ms !== undefined) facts.push(['Time', `${Math.round(meta.ms / 1000)} s`]);
  if (meta.usage) {
    facts.push([
      'Tokens',
      `${meta.usage.inputTokens} in, ${meta.usage.outputTokens} out` +
        (meta.usage.cost !== undefined ? ` ($${meta.usage.cost.toFixed(4)})` : ''),
    ]);
  }
  if (meta.files?.length) facts.push(['Files', meta.files.join(', ')]);
  if (meta.changedLines !== undefined) facts.push(['Changed lines', String(meta.changedLines)]);
  if (meta.findings?.length) facts.push(['Findings', String(meta.findings.length)]);
  if (meta.dropped?.length) facts.push(['Repeated, not sent again', String(meta.dropped.length)]);
  if (meta.held) facts.push(['Plan', 'held back for the agent to fix']);
  if (meta.open) facts.push(['Open findings', `${meta.open}, saved at the end of the plan`]);
  if (meta.afterGreen) facts.push(['When', 'after the checks passed']);
  if (meta.childDialogId) facts.push(['Reviewer chat', meta.childDialogId]);
  return facts;
}

/** "Review 1 of at most 3 · 12 tool calls · 41 s · model" */
export function reviewDetails(meta: ReviewMeta): string {
  const parts = [
    meta.round === undefined
      ? `No review: all ${meta.maxRounds} used`
      : `Review ${meta.round} of at most ${meta.maxRounds}`,
  ];
  if (meta.toolCalls !== undefined) parts.push(`${meta.toolCalls} tool calls`);
  if (meta.ms !== undefined) parts.push(`${Math.round(meta.ms / 1000)} s`);
  if (meta.model) parts.push(modelLabel(meta.model, meta.modelsTried));
  if (meta.usage) {
    const t = meta.usage.inputTokens + meta.usage.outputTokens;
    parts.push(t >= 1000 ? `${(t / 1000).toFixed(1)}k tokens` : `${t} tokens`);
  }
  if (meta.partial) parts.push('out of budget');
  return parts.join(' · ');
}
