// verification.ts
//
// The verification gate's records, as the chat shows them. When the agent
// finishes a run in which it changed files, the backend runs the project's
// checks (type check, lint, tests) and, if any fail, sends the failures back
// to the agent to fix (src-tauri/src/agent_run/verify.rs). Each round is
// recorded in the chat as a message with `meta.verification`.
import type { CheckKind } from './projectProfile';

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
    parts.push(`${meta.testsRun} test${meta.testsRun === 1 ? '' : 's'} ran${before}`);
  }
  return parts.length ? parts.join(' · ') : null;
}
