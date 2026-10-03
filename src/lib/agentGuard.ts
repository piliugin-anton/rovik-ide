// agentGuard.ts
//
// Keeps one agent run (a user message and every tool call that follows)
// from going on forever, without capping legitimate long tasks.
//
// Two mechanisms:
//   - Checkpoints: every N tool calls (a setting; "never" allowed) the run
//     pauses and the user decides whether it goes on. Pausing is not an
//     error: the chat shows Continue, and the run picks up where it was.
//   - Loop detection, independent of the count: the same call returning the
//     same thing again and again, a streak of failures, the same error over
//     and over, or an edit putting a file back the way it was. The first time
//     one fires the model gets a note in the tool result (models often recover
//     when told); the next time the run pauses with the reason.
//
// Pure bookkeeping, no I/O: ChatPanel feeds it each finished tool call.

/** Default for the "pause every N tool calls" setting. */
export const DEFAULT_TOOL_CALL_CHECKPOINT = 50;
/** The setting's choices; null = never pause on count alone. */
export const TOOL_CALL_CHECKPOINT_CHOICES: (number | null)[] = [25, 50, 100, 200, null];

/** Same call, same arguments, same result: this many times is a loop. */
const IDENTICAL_LIMIT = 3;
/** ...within this many most recent calls. */
const IDENTICAL_WINDOW = 30;
/** Failed calls in a row. */
const ERROR_STREAK_LIMIT = 5;
/** The same error text this many times... */
const SAME_ERROR_LIMIT = 3;
/** ...within this many most recent calls. */
const SAME_ERROR_WINDOW = 15;
/** Earlier states remembered per edited file. */
const FILE_STATES_KEPT = 50;

export interface FinishedCall {
  name: string;
  argsJson: string;
  /** The result text the model gets. */
  result: string;
  /**
   * A successful file write (write_file, edit_file): the file before (null
   * when it didn't exist) and after.
   */
  write?: { path: string; before: string | null; after: string };
}

export type GuardVerdict =
  | { kind: 'ok' }
  /** Add `note` to this call's result: the model is warned. */
  | { kind: 'nudge'; note: string }
  /** Add `note` to the result; the run pauses after this turn's calls. */
  | { kind: 'stop'; note: string; reason: string };

export interface PauseInfo {
  kind: 'checkpoint' | 'loop';
  /** Tool calls run so far in this run, Continues included. */
  toolCalls: number;
  /** Loop pauses: what looked stuck. */
  reason?: string;
}

export class AgentRunGuard {
  /** Tool calls in this run so far, counting those before a Continue. */
  toolCalls: number;
  private nextCheckpoint: number | null;
  private recentCalls: string[] = [];
  private recentErrors: string[] = [];
  private errorStreak = 0;
  private fileStates = new Map<string, number[]>();
  private nudged = false;
  private stopReason: string | null = null;

  /**
   * @param checkpointEvery pause after every this many tool calls; null never.
   * @param startAt tool calls already made (continuing a paused run).
   */
  constructor(checkpointEvery: number | null, startAt = 0) {
    this.toolCalls = startAt;
    this.nextCheckpoint =
      checkpointEvery && checkpointEvery > 0
        ? (Math.floor(startAt / checkpointEvery) + 1) * checkpointEvery
        : null;
  }

  /** Records one finished tool call. */
  record(call: FinishedCall): GuardVerdict {
    this.toolCalls++;
    const reason = this.detect(call);
    if (!reason) return { kind: 'ok' };

    if (!this.nudged) {
      this.nudged = true;
      // Fresh evidence is needed for the next verdict, so a model that does
      // change course isn't paused by what it did before the note.
      this.resetEvidence();
      return {
        kind: 'nudge',
        note:
          `\n\n[Note from the IDE: ${reason}. This looks like a loop. Step back before going on: ` +
          're-read the relevant code, try a different approach, or stop and ask the user. ' +
          'If it happens again, the run is paused for the user.]',
      };
    }
    this.stopReason = reason;
    return {
      kind: 'stop',
      reason,
      note: `\n\n[Note from the IDE: the run was paused here for the user, because ${reason}.]`,
    };
  }

  /** Whether to pause before the next request to the model, and why. */
  pause(): PauseInfo | null {
    if (this.stopReason) {
      return { kind: 'loop', toolCalls: this.toolCalls, reason: this.stopReason };
    }
    if (this.nextCheckpoint !== null && this.toolCalls >= this.nextCheckpoint) {
      return { kind: 'checkpoint', toolCalls: this.toolCalls };
    }
    return null;
  }

  private detect(call: FinishedCall): string | null {
    const failed = call.result.startsWith('Error:');

    // An edit that went through changes what reads and commands return:
    // repeating them afterwards is expected, not a loop.
    if (call.write) {
      this.recentCalls = [];
      const revert = this.recordWrite(call.write);
      if (revert) return revert;
    }

    // 1. The same call returning the same result again.
    const key = `${call.name}\u0000${canonicalArgs(call.argsJson)}\u0000${hash(call.result)}`;
    this.recentCalls.push(key);
    if (this.recentCalls.length > IDENTICAL_WINDOW) this.recentCalls.shift();
    const repeats = this.recentCalls.filter((k) => k === key).length;
    if (!call.write && repeats >= IDENTICAL_LIMIT) {
      return (
        `${describeCall(call)} was called ${repeats} times with the same arguments and ` +
        'returned the same result each time'
      );
    }

    // 2. Failures in a row.
    this.errorStreak = failed ? this.errorStreak + 1 : 0;
    if (this.errorStreak >= ERROR_STREAK_LIMIT) {
      return `the last ${this.errorStreak} tool calls all failed`;
    }

    // 3. The same failure again, whatever the arguments.
    const error = failed ? call.result.trim() : '';
    this.recentErrors.push(error);
    if (this.recentErrors.length > SAME_ERROR_WINDOW) this.recentErrors.shift();
    if (failed) {
      const same = this.recentErrors.filter((e) => e === error).length;
      if (same >= SAME_ERROR_LIMIT) {
        return `the same error came back ${same} times: "${firstLine(error, 160).replace(/\.$/, '')}"`;
      }
    }
    return null;
  }

  /** A write that puts the file back into a state it had earlier in the run. */
  private recordWrite(write: NonNullable<FinishedCall['write']>): string | null {
    const states = this.fileStates.get(write.path) ?? [];
    if (states.length === 0 && write.before !== null) states.push(hash(write.before));
    const after = hash(write.after);
    const last = states[states.length - 1];
    const backToEarlier = after !== last && states.includes(after);
    states.push(after);
    if (states.length > FILE_STATES_KEPT) states.shift();
    this.fileStates.set(write.path, states);
    return backToEarlier
      ? `this change put ${write.path} back exactly the way it was earlier in this run, undoing the changes in between`
      : null;
  }

  private resetEvidence() {
    this.recentCalls = [];
    this.recentErrors = [];
    this.errorStreak = 0;
    this.fileStates.clear();
  }
}

/** Arguments with keys sorted, so {"a":1,"b":2} and {"b":2,"a":1} are the same call. */
function canonicalArgs(argsJson: string): string {
  try {
    return JSON.stringify(sortKeys(JSON.parse(argsJson)));
  } catch {
    return argsJson.trim();
  }
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value as Record<string, unknown>)
        .sort()
        .map((k) => [k, sortKeys((value as Record<string, unknown>)[k])])
    );
  }
  return value;
}

/** "read_file src/App.tsx", "run_command `npm test`": for the notes. */
function describeCall(call: FinishedCall): string {
  let args: Record<string, unknown> = {};
  try {
    args = JSON.parse(call.argsJson) ?? {};
  } catch {
    // name only
  }
  const detail = [args.path, args.command, args.query, args.url].find(
    (v): v is string => typeof v === 'string' && v.trim() !== ''
  );
  return detail ? `${call.name} (${firstLine(detail, 80)})` : call.name;
}

function firstLine(text: string, max: number): string {
  const line = text.split('\n')[0];
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** cyrb53: a fast 53-bit string hash; results and file contents aren't kept whole. */
function hash(text: string): number {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < text.length; i++) {
    const ch = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return 4294967296 * (2097151 & h2) + (h1 >>> 0);
}
