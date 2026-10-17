// verification.ts
//
// The verification gate's records, as the chat shows them. When the agent
// finishes a run in which it changed files, the backend runs the project's
// checks (type check, lint, tests) and, if any fail, sends the failures back
// to the agent to fix (src-tauri/src/agent_run/verify.rs). Each round is
// recorded in the chat as a message with `meta.verification`.
import type { CheckKind } from './projectProfile';
import type { SubagentUsage } from './subagents';
import { plural } from './textUtils';

export type CheckStatus = 'pending' | 'running' | 'passed' | 'failed' | 'skipped';

export interface CheckResult {
  kind: CheckKind;
  command: string;
  status: CheckStatus;
  exitCode?: number | null;
  timedOut?: boolean;
  durationMs?: number;
  /** skipped: why. */
  reason?: string;
  /** failed: the relevant part of the output (error blocks and summary). */
  output?: string;
  /** failed: files the errors point at, split by whether this run changed them. */
  errorFiles?: { changed: string[]; other: string[] };
  /** Waiting for a CPU slot while it waits; afterwards, a long wait. */
  note?: string;
  /** failed: a key for each failure, the same when only lines moved. */
  failures?: string[];
  /** The test check: how many tests ran, when its summary said. */
  testsRan?: number;
  /** failed, with more output than `output`: where the whole is saved (`@checks/…`). */
  savedOutput?: string;
}

/** What a failure triage found (src-tauri/src/agent_run/triage.rs). */
export interface TriageResult {
  check: CheckKind;
  rootCause: string;
  files: string[];
  nextStep: string;
  /** high, medium or low (as written otherwise). */
  confidence: string;
  confidenceNote: string;
  /** Independent causes besides the root cause. */
  otherCauses?: { files: string[]; confidence: string; cause: string }[];
}

/** A round's failure triage: a subagent that read a large or repeated failure. */
export interface TriageMeta {
  status: 'running' | 'done' | 'skipped' | 'stopped';
  /** The check it looked at. */
  check: CheckKind;
  /** Why it ran. */
  reason: 'large output' | 'failed again';
  /** skipped: why. */
  skipped?: string;
  result?: TriageResult;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
  /** Tokens its providers reported (P5.3: counted in the run's total). */
  usage?: SubagentUsage;
}

export type VerificationStatus =
  /** Checks are running (the record updates as each finishes). */
  | 'running'
  | 'passed'
  /** Sent to the agent to fix; another round follows. */
  | 'failed'
  /** Still failing after the last round: the run ended. */
  | 'gave-up'
  /** The agent finished after a failure without changing anything. */
  | 'unresolved'
  /** Command approval is on: the user starts the checks. */
  | 'needs-approval'
  /** It waited for approval, and a newer request came first: it won't run. */
  | 'superseded'
  /** Nothing could be checked (no checks, or none allowed to run). */
  | 'unverified'
  /** The fix broke something that passed before: it was undone. */
  | 'rolled-back'
  /** Checks pass, but tests were weakened or the check setup changed. */
  | 'flagged'
  | 'stopped';

/** How a round compares with the one before (src-tauri/src/agent_run/repair.rs). */
export interface RoundProgress {
  verdict: 'progress' | 'no-progress' | 'regression';
  /** Failing now, not before. */
  new: string[];
  /** The new failures in what passed before: they make it a regression. */
  broke?: string[];
  /** Failing before, not now. */
  fixed: string[];
  /** Back to the failures of two rounds ago. */
  backAndForth?: boolean;
}

/** A fix round that was undone. */
export interface RolledBack {
  restored: string[];
  /** Changed outside the chat meanwhile: left as they were. */
  skipped: string[];
  /** The failures the fix brought in. */
  broke: string[];
  diff: string;
}

/** Who fixes a cause (src-tauri/src/agent_run/causes.rs). */
export type CauseOwner =
  | { kind: 'worker'; label: string; step: number; describe: string }
  | {
      kind: 'lead';
      reason: 'contract' | 'acrossScopes' | 'unowned' | 'unlocated';
      describe: string;
    }
  | { kind: 'testRule'; file: string; describe: string };

/** One root cause of a failed round, and its fixer if one ran. */
export interface RepairCause {
  owner: CauseOwner;
  keys: string[];
  files: string[];
  summary: string;
  confidence?: string;
  fixer?: {
    /** done, partial, failed, not started, not landed, … */
    status: string;
    childDialogId?: string | null;
    changed: string[];
    said: string;
    parallel: boolean;
  };
}

/** The repair loop's round after a parallel group (R03–R07). */
export interface RepairMeta {
  group: string;
  round: number;
  /** fixing: workers are at it; recheck: they fixed it all, the checks run again; lead: the lead fixes (the rest). */
  status: 'fixing' | 'recheck' | 'lead' | 'stopped';
  /** Why the lead fixes everything this round (the escalation ladder). */
  leadOnly?: string | null;
  parallel: boolean;
  causes: RepairCause[];
}

/** Stored in a message's meta: shown in the chat by MessageList. */
export interface VerificationMeta {
  status: VerificationStatus;
  round: number;
  maxRounds: number;
  checks: CheckResult[];
  changedFiles: string[];
  /** unverified: why. */
  reason?: string;
  progress?: RoundProgress;
  failureCount?: number;
  testsRun?: number;
  /** The round before's count, to show a drop. */
  testsRunBefore?: number;
  /** Changes that weaken the checks. */
  integrity?: string[];
  rolledBack?: RolledBack;
  triage?: TriageMeta;
  /** Checks run in the middle of a run, after a parallel group was joined (F06). */
  midRun?: boolean;
  /** midRun: each error's location and the worker (or seam) it's put down to. */
  attribution?: { location: string; owner: string }[];
  /** After a parallel group: the round's causes and who fixed them. */
  repair?: RepairMeta;
  /** gave-up: the failures left, the fix attempts rolled back, the last triage's diagnosis. */
  remaining?: string[];
  attempts?: RolledBack[];
  triageSummary?: string | null;
  /** passed after a fix round: the change review that followed (R08). */
  reviewAfterGreen?: { status: 'findings' | 'done' | 'skipped'; reason?: string };
  /** False: started with none of the earlier rounds' state (P1.7). */
  stateRestored?: boolean;
  /** False: what the files were before the run was lost, so whether the
   * change weakens tests couldn't be checked (P3.9). */
  integrityChecked?: boolean;
  /** Autopilot: the checks are the plan's validation commands, the project
   * having none set up (V02). */
  fromPlan?: boolean;
}

/** The repair round's one-line summary. */
export function repairHeadline(r: RepairMeta): string {
  const n = r.causes.length;
  const causes = plural(n, 'cause');
  if (r.leadOnly) return `${causes}: the agent fixes them all (${r.leadOnly}).`;
  const fixers = r.causes.filter((c) => c.owner.kind === 'worker').length;
  switch (r.status) {
    case 'fixing':
      return `${causes}: the workers that own them are fixing them${r.parallel ? ' at the same time' : ', one at a time'}…`;
    case 'recheck':
      return `${causes}: the workers fixed them${r.parallel ? ' at the same time' : ''}; the checks run again.`;
    case 'stopped':
      return `${causes}: stopped while they were being fixed.`;
    case 'lead':
      return fixers && r.causes.some((c) => c.fixer)
        ? `${causes}: the workers fixed theirs; the agent fixes the rest.`
        : `${causes}: the agent fixes them.`;
  }
}

/** A cause's line: what, how many failures, who, and how the fix went. */
export function causeLine(c: RepairCause): string {
  const count = plural(c.keys.length, 'failure');
  let fix = '';
  if (c.fixer) {
    fix =
      c.fixer.status === 'done' && c.fixer.changed.length
        ? ` — fixed (${c.fixer.changed.join(', ')})`
        : ` — not fixed (${c.fixer.status}${c.fixer.said ? `: ${c.fixer.said}` : ''})`;
  }
  return `${c.summary} (${count}) · ${c.owner.describe}${fix}`;
}

/** The review after green, in a line; null when none ran or was due. */
export function reviewAfterGreenLine(meta: VerificationMeta): string | null {
  const r = meta.reviewAfterGreen;
  if (!r) return null;
  switch (r.status) {
    case 'findings':
      return 'A fresh review of the whole change found something serious: sent to the agent, and the checks run again after its fix.';
    case 'done':
      return 'A fresh review of the whole change found nothing serious.';
    case 'skipped':
      return `No review after the fix: ${r.reason ?? 'it couldn’t run'}.`;
  }
}

/** The triage's one-line summary in the record. */
export function triageHeadline(t: TriageMeta, checkLabel: string, interrupted = false): string {
  const what = `${checkLabel.toLowerCase()} failure`;
  const why = t.reason === 'failed again' ? 'it failed again' : 'its output is large';
  if (interrupted) return `Triage of the ${what} didn’t finish.`;
  switch (t.status) {
    case 'running':
      return `Triaging the ${what} (${why})…`;
    case 'done':
      return `Triage of the ${what}: ${t.result?.confidence ?? 'unknown'} confidence.`;
    case 'stopped':
      return `Triage of the ${what} stopped.`;
    case 'skipped':
      return `Triage of the ${what} skipped: ${t.skipped ?? 'it couldn’t run'}. The agent got the usual report.`;
  }
}

/** A failure key without its check: "src/a.ts|TS2322|Type …" → readable. */
export function describeFailure(key: string): string {
  const parts = key.split('|').slice(1).filter(Boolean);
  if (parts[0] === 'test') return `test ${parts.slice(1).join(' ')}`;
  return parts.join(' · ');
}

/** One line on how the round went against the one before; null for none. */
export function progressLine(meta: VerificationMeta): string | null {
  const parts: string[] = [];
  const p = meta.progress;
  if (p) {
    if (p.verdict === 'no-progress') {
      parts.push(
        p.backAndForth
          ? 'No progress: back to the failures of two rounds ago'
          : 'No progress: the same failures as the round before'
      );
    } else {
      if (p.fixed.length) parts.push(`${p.fixed.length} fixed`);
      if (p.new.length) parts.push(`${p.new.length} new`);
    }
  }
  if (meta.failureCount && meta.status !== 'rolled-back')
    parts.push(`${meta.failureCount} failing`);
  if (meta.testsRun !== undefined) {
    const before =
      meta.testsRunBefore !== undefined && meta.testsRunBefore !== meta.testsRun
        ? ` (${meta.testsRunBefore} before)`
        : '';
    parts.push(`${plural(meta.testsRun, 'test')} ran${before}`);
  }
  return parts.length ? parts.join(' · ') : null;
}
