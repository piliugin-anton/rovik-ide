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

/**
 * A follow-up review's word on one earlier finding: a plan review's
 * (plan_review.rs, B13), or in Autopilot a change review's (S06).
 */
export interface EarlierCheck {
  /** Its number, as the agent got it. */
  finding: number;
  severity: FindingSeverity;
  location: string;
  title: string;
  /** The agent's answer (submit_plan's `responses`, answer_review's), when it gave one. */
  response: { action: 'fixed' | 'not_a_problem'; note: string } | null;
  resolved: boolean;
  /**
   * The reviewer's word (a verdict, or the finding reported again), not the
   * IDE's default: false when it gave no verdict even when asked (then
   * resolved: taken as settled) or the plan came back unchanged.
   */
  checked?: boolean;
  /** The reviewer's reason, or why the IDE decided. */
  reason: string;
}

/** A follow-up's word on an earlier finding, as its card says it. */
export function earlierVerdict(check: EarlierCheck): 'resolved' | 'still open' | 'not checked' {
  if (!check.resolved) return 'still open';
  return check.checked === false ? 'not checked' : 'resolved';
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
  /** Thorough (T07): the reviews go on while they leave fewer findings open, not for a count. */
  byProgress?: boolean;
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
  /**
   * plan: findings still open after the last round, saved at the end of the
   * plan; a blocked change review: the high ones still open.
   */
  open?: number;
  /** A follow-up review: each earlier finding, the agent's answer and the verdict. */
  earlier?: EarlierCheck[];
  /** change, Autopilot (S06): it blocks: its findings are answered and followed up. */
  blocking?: boolean;
  /**
   * The last review left a high finding open: an Autopilot plan isn't
   * saved (S02), an Autopilot change goes no further (S06).
   */
  blocked?: boolean;
  /** plan, a follow-up review: new findings below high about steps the revision didn't change; shown, not held. */
  notHeld?: ReviewFinding[];
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
      if (meta.reason) return `The reviewer reported nothing to fix: ${meta.reason}.`;
      // S06: a follow-up that found what it checked settled.
      if (meta.earlier?.length)
        return `The reviewer found the earlier ${meta.earlier.length === 1 ? 'finding' : 'findings'} resolved and nothing new to fix.`;
      return 'The reviewer found nothing to fix.';
    case 'findings': {
      const all = meta.findings ?? [];
      // P5.5: after green, only high findings go back to the agent.
      const serious = all.filter((f) =>
        meta.afterGreen ? f.severity === 'high' : f.severity !== 'low'
      ).length;
      const count = plural(all.length, 'finding');
      if (meta.blocked) {
        const high = meta.open ?? all.filter((f) => f.severity === 'high').length;
        return `The reviewer still finds ${plural(high, 'high problem')} after the run’s last review: the change goes no further, and the task ends here.`;
      }
      if (meta.blocking && serious)
        return `The reviewer found ${count}: ${serious} sent to the agent to fix or answer, and a follow-up review checks them.`;
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
    case 'clean': {
      if (meta.reason) return `The plan reviewer reported nothing to fix: ${meta.reason}.`;
      const earlier = meta.earlier?.length ?? 0;
      if (!earlier) return 'The plan reviewer found nothing to fix.';
      const notHeld = meta.notHeld?.length
        ? ` (${plural(meta.notHeld.length, 'finding')} about what the revision didn’t change, not held)`
        : '';
      const unchecked = meta.earlier!.filter((e) => earlierVerdict(e) === 'not checked').length;
      if (unchecked)
        return `The plan reviewer found nothing new to fix${notHeld}, but gave no verdict on ${plural(unchecked, 'earlier finding')}: taken as settled.`;
      return `The plan reviewer found the earlier ${earlier === 1 ? 'finding' : 'findings'} resolved and nothing new to fix${notHeld}.`;
    }
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
      : meta.byProgress
        ? `${meta.round}, going on while the findings get fewer`
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
  if (meta.earlier?.length) {
    const count = (verdict: ReturnType<typeof earlierVerdict>) =>
      meta.earlier!.filter((e) => earlierVerdict(e) === verdict).length;
    const unchecked = count('not checked');
    facts.push([
      'Earlier findings',
      `${count('resolved')} resolved, ${count('still open')} still open` +
        (unchecked ? `, ${unchecked} not checked` : ''),
    ]);
  }
  if (meta.notHeld?.length)
    facts.push(['Not held', `${meta.notHeld.length}, about what the revision didn’t change`]);
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
      : meta.byProgress
        ? `Review ${meta.round}`
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

/** answer_review's answers in a line (S06): "1 fixed, 2 not a problem". */
export function reviewAnswersSummary(args: Record<string, unknown>): string | undefined {
  const list = Array.isArray(args.responses) ? args.responses : [];
  const parts = list.flatMap((item) => {
    if (!item || typeof item !== 'object') return [];
    const r = item as Record<string, unknown>;
    const n = typeof r.finding === 'number' ? r.finding : Number(r.finding);
    const status = String(r.status ?? r.action ?? '')
      .toLowerCase()
      .replace(/[^a-z]/g, '');
    const word = status === 'fixed' ? 'fixed' : status.startsWith('not') ? 'not a problem' : '';
    return Number.isFinite(n) && word ? [`${n} ${word}`] : [];
  });
  return parts.length ? parts.join(', ') : undefined;
}
