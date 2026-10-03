// verification.ts
//
// The verification gate's check runner. When the agent finishes a run in
// which it changed files, ChatPanel runs the project's checks (type check,
// lint, tests: lib/projectProfile.ts) and, if any fail, sends the failures
// back to the agent to fix. The environment, not the model, decides that
// the change works. This file runs the checks and words the report.
import { toolRunCommand } from './commands';
import { errorLocations, focusFailures, splitByChanged } from './commandOutput';
import { checkCommand, type AgentPermissions } from './permissions';
import { CHECK_LABEL, orderedChecks, type CheckKind, type ProjectCheck } from './projectProfile';

/** Fix-and-recheck rounds before the gate gives up and reports. */
export const MAX_VERIFY_ROUNDS = 3;
/** Per check. Test suites can be slow; this is the run_command maximum. */
const CHECK_TIMEOUT_MS = 600_000;

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

export function isFailed(result: CheckResult): boolean {
  return result.status === 'failed';
}

/**
 * Runs the enabled checks in order (type check, lint, tests, build). A check
 * the user's command permissions don't allow is skipped, not run: the gate
 * is held to the same rules as the agent. Tests are skipped when the type
 * check failed: its errors would only reappear as test failures.
 */
export async function runChecks(
  root: string,
  checks: ProjectCheck[],
  permissions: AgentPermissions,
  opts: {
    /** Files changed in the run: errors in them are told apart from others. */
    changedFiles: string[];
    shouldStop: () => boolean;
    onProgress: (results: CheckResult[]) => void;
  }
): Promise<CheckResult[]> {
  const results: CheckResult[] = orderedChecks(checks)
    .filter((c) => c.enabled && c.command.trim())
    .map((c) => ({ kind: c.kind, command: c.command.trim(), status: 'pending' }));
  opts.onProgress(results.map((r) => ({ ...r })));

  for (const result of results) {
    if (opts.shouldStop()) {
      result.status = 'skipped';
      result.reason = 'stopped';
      continue;
    }
    const verdict = checkCommand(result.command, permissions);
    if (!verdict.permitted) {
      result.status = 'skipped';
      result.reason = 'not in your allowed commands';
      opts.onProgress(results.map((r) => ({ ...r })));
      continue;
    }
    if (
      result.kind === 'test' &&
      results.some((r) => r.kind === 'typecheck' && r.status === 'failed')
    ) {
      result.status = 'skipped';
      result.reason = 'the type check failed';
      opts.onProgress(results.map((r) => ({ ...r })));
      continue;
    }

    result.status = 'running';
    opts.onProgress(results.map((r) => ({ ...r })));
    try {
      const out = await toolRunCommand(root, result.command, CHECK_TIMEOUT_MS);
      result.durationMs = out.durationMs;
      result.exitCode = out.exitCode;
      result.timedOut = out.timedOut;
      if (out.exitCode === 0 && !out.timedOut) {
        result.status = 'passed';
      } else {
        result.status = 'failed';
        // Compilers print to either stream; keep both, in order of usefulness.
        const combined = [out.stderr.trim(), out.stdout.trim()].filter(Boolean).join('\n');
        result.output = focusFailures(combined) || '(no output)';
        // From the focused output: the error blocks, not passing tests' paths.
        const files = splitByChanged(errorLocations(result.output), opts.changedFiles);
        if (files.changed.length || files.other.length) result.errorFiles = files;
      }
    } catch (err) {
      result.status = 'failed';
      result.output = `Could not run the command: ${String(err)}`;
    }
    opts.onProgress(results.map((r) => ({ ...r })));
  }
  return results;
}

/** "Errors point at …", for the report: changed files first, the rest flagged. */
export function describeErrorFiles(files?: CheckResult['errorFiles']): string | null {
  if (!files) return null;
  const list = (paths: string[]) =>
    paths.length > 6
      ? `${paths.slice(0, 6).join(', ')} and ${paths.length - 6} more`
      : paths.join(', ');
  const parts: string[] = [];
  if (files.changed.length)
    parts.push(`Errors point at files you changed: ${list(files.changed)}.`);
  if (files.other.length) {
    parts.push(
      files.changed.length
        ? `Also at files you didn't change: ${list(files.other)}.`
        : `Errors point only at files you didn't change: ${list(files.other)} ` +
            '(they may have failed before your change, or be affected by it).'
    );
  }
  return parts.join(' ');
}

/** The message the agent gets when checks fail (sent as a user-role message). */
export function failureReport(
  results: CheckResult[],
  changedFiles: string[],
  round: number
): string {
  const failed = results.filter(isFailed);
  const passed = results.filter((r) => r.status === 'passed');
  const lines: string[] = [
    '[Automatic check by the IDE, not written by the user]',
    `Your changes are not done yet: ${failed.length === 1 ? 'a check fails' : `${failed.length} checks fail`} ` +
      `(attempt ${round} of ${MAX_VERIFY_ROUNDS}).`,
    '',
    `Files you changed in this task: ${changedFiles.length ? changedFiles.join(', ') : '(none recorded)'}`,
  ];
  if (passed.length) {
    lines.push(
      `Passed: ${passed.map((r) => `${CHECK_LABEL[r.kind]} (\`${r.command}\`)`).join(', ')}`
    );
  }
  for (const r of failed) {
    const how = r.timedOut
      ? 'timed out'
      : r.exitCode === null || r.exitCode === undefined
        ? 'failed'
        : `exit code ${r.exitCode}`;
    lines.push('', `## ${CHECK_LABEL[r.kind]} failed: \`${r.command}\` (${how})`);
    const where = describeErrorFiles(r.errorFiles);
    if (where) lines.push(where);
    lines.push('', r.output ?? '');
  }
  const skipped = results.filter((r) => r.status === 'skipped');
  if (skipped.length) {
    lines.push('', `Not run: ${skipped.map((r) => `\`${r.command}\` (${r.reason})`).join(', ')}`);
  }
  lines.push(
    '',
    'Find the cause in your changes and fix it; the checks run again when you finish. ' +
      'If a failure has nothing to do with your changes (it fails on the original code too), ' +
      "don't change unrelated code: finish and say so."
  );
  return lines.join('\n');
}
