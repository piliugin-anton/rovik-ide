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
  | 'stopped';

/** Stored in a message's meta: shown in the chat by MessageList. */
export interface VerificationMeta {
  status: VerificationStatus;
  round: number;
  maxRounds: number;
  checks: CheckResult[];
  changedFiles: string[];
  /** unverified: why. */
  reason?: string;
}
