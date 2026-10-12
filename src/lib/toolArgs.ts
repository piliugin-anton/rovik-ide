// toolArgs.ts
//
// A tool call's arguments and result, as the chat shows them: shared by a
// call's own block (ToolCallBlock) and a group of calls (ToolCallGroup,
// lib/toolGroups.ts).
import { parseTodo, progressLabel } from './todo';

export type Args = Record<string, unknown>;

export const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
export const num = (v: unknown) => (typeof v === 'number' ? v : undefined);

/** A line-number argument; models sometimes send it as a string ("12"). */
export function lineArg(v: unknown): number | undefined {
  const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : num(v);
  return n !== undefined && Number.isFinite(n) && n >= 1 ? Math.floor(n) : undefined;
}

/** A write that replaces one quoted piece of a file (old_text) rather than all of it. */
export function isPartialWrite(name: string, args: Args): boolean {
  return name === 'write' && args.old_text !== undefined && args.old_text !== null;
}

/** A plan's title: its first heading, as written. */
export function planTitle(plan: string | undefined): string | undefined {
  return plan?.match(/^#\s+(.+)$/m)?.[1];
}

/**
 * A checklist update's line: "1 of 3 done · <the step in progress>", or
 * undefined for an empty checklist. `title` shortens the step's title.
 */
export function todoSummary(args: Args, title = (t: string) => t): string | undefined {
  const items = parseTodo(args);
  const current = items.find((i) => i.status === 'in_progress');
  if (!items.length) return undefined;
  return `${progressLabel(items)}${current ? ` · ${title(current.title)}` : ''}`;
}

/**
 * Arguments stream in as partial JSON, so a strict parse fails until the
 * last chunk lands. Fall back to pulling complete string/number fields out
 * with a regex so the header can say "Reading src/foo.ts" mid-stream.
 */
export function parseArgsLenient(raw: string): Args {
  const trimmed = raw.trim();
  if (!trimmed) return {};

  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    const out: Args = {};
    const fieldRe = /"(\w+)"\s*:\s*(?:"((?:[^"\\]|\\.)*)"|(-?\d+))/g;
    let m: RegExpExecArray | null;
    while ((m = fieldRe.exec(trimmed)) !== null) {
      const [, key, str, num] = m;
      if (str !== undefined) {
        try {
          out[key] = JSON.parse(`"${str}"`);
        } catch {
          out[key] = str;
        }
      } else if (num !== undefined) {
        out[key] = Number(num);
      }
    }
    // A string still arriving (a file's content as it's written): what
    // there is so far, so the header can count its lines while it streams.
    const open = /"(\w+)"\s*:\s*"((?:[^"\\]|\\.)*\\?)$/.exec(trimmed);
    if (open && !(open[1] in out)) {
      // Drop a half-received escape at the end ("\", "\u00").
      const raw = open[2].replace(/\\(u[0-9a-fA-F]{0,3})?$/, '');
      try {
        out[open[1]] = JSON.parse(`"${raw}"`);
      } catch {
        out[open[1]] = raw;
      }
    }
    return out;
  }
}

/** A tool result reporting a failure. */
export function isErrorResult(result?: string) {
  return result !== undefined && /^Error:/.test(result.trimStart());
}

export type CallStatus = 'running' | 'done' | 'error' | 'idle';

/**
 * Where a call is, from its result (the role:"tool" message, once it has
 * arrived) and whether the run still goes. A superseded call (an earlier
 * `todo` snapshot) never runs: its "in progress" step is history, not work
 * happening now, even while a later run goes.
 */
export function callStatus(
  result: { content: string } | undefined,
  running: boolean | undefined,
  superseded?: boolean
): CallStatus {
  if (!result) return running && !superseded ? 'running' : 'idle';
  return isErrorResult(result.content) ? 'error' : 'done';
}

/**
 * A folder a listing named: `listed` is the one it actually listed, relative
 * to the project ("" for the project itself), as the backend resolved it
 * (agent_tools::project_relative). Without it (still running, or listed
 * before results said), the path as the model wrote it, without a leading
 * "./" or a trailing separator.
 */
export function dirName(path: string | undefined, listed?: string): string {
  const dir =
    listed ?? (path?.trim() ?? '').replace(/(.)[\\/]+$/, '$1').replace(/^(\.[\\/]+)+/, '');
  return dir === '' || dir === '.' ? 'project root' : dir;
}
