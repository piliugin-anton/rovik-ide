// toolGroups.ts
//
// Every tool call shows in the chat as one line ("Read App.tsx", "Ran npm",
// "Updated the checklist"), opened like the model's thinking, and
// consecutive calls to the same tool fold into one ("Read App.tsx,
// main.tsx"). Only the tool's name decides; the arguments only name the line.
import type { ChatToolCall } from '../types';
import { isLiveContextPath } from './contextDoc';
import {
  dirName,
  isPartialWrite,
  lineArg,
  parseArgsLenient,
  planTitle,
  str,
  todoSummary,
  type Args,
} from './toolArgs';
import { diffRows, snippetLines } from './lineDiff';
import { kindLabel } from './subagents';

/** Consecutive calls to one tool; a single call is a run of one. */
export interface ToolRun {
  name: string;
  calls: ChatToolCall[];
  /** A call that never folds with the calls around it (see standsAlone). */
  alone?: boolean;
}

/**
 * Calls that keep a line of their own: each question, checklist update,
 * plan and subagent (`task`: its approvals show on its own block) is its
 * own event, and a change to the agent's context file says "its context",
 * not a file name.
 */
function standsAlone(call: ChatToolCall): boolean {
  if (['ask_user', 'todo', 'submit_plan', 'task'].includes(call.name)) return true;
  return call.name === 'write' && isLiveContextPath(str(parseArgsLenient(call.arguments).path));
}

export function groupToolCalls(calls: readonly ChatToolCall[]): ToolRun[] {
  const runs: ToolRun[] = [];
  let open: ToolRun | null = null;
  for (const call of calls) {
    if (open && open.name === call.name && !standsAlone(call)) {
      open.calls.push(call);
      continue;
    }
    const alone = standsAlone(call);
    open = alone ? { name: call.name, calls: [call], alone } : { name: call.name, calls: [call] };
    runs.push(open);
    if (alone) open = null;
  }
  return runs;
}

/** One thing a group's line names: a file (opens in the editor), a command, a site. */
export interface GroupItem {
  text: string;
  /** A file to open on click, as the model gave it. */
  path?: string;
  /** Where to open it. */
  line?: number;
  /** The whole value, for a tooltip when `text` is shortened. */
  title?: string;
}

export interface GroupLabel {
  /** "Reading", "Ran", "Searched the web for". */
  verb: string;
  items: GroupItem[];
  /** Items left out of the line (more than MAX_ITEMS). */
  more: number;
}

const MAX_ITEMS = 4;

function short(text: string, max = 40): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** "src/lib/a.ts" → "a.ts" */
export function fileName(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/**
 * The program a shell command starts with, without its arguments:
 * "FOO=1 /usr/bin/npm test && git status" → "npm".
 */
export function commandName(command: string | undefined): string | undefined {
  for (const token of (command ?? '').trim().split(/\s+/)) {
    if (!token || /^[A-Za-z_][A-Za-z0-9_]*=/.test(token)) continue;
    const bare = token.replace(/^['"(]+|['");]+$/g, '');
    return bare ? fileName(bare) : undefined;
  }
  return undefined;
}

/** "https://www.docs.rs/tokio/" → "docs.rs" */
export function domainName(url: string | undefined): string | undefined {
  const raw = url?.trim();
  if (!raw) return undefined;
  try {
    const host = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) ? raw : `https://${raw}`).hostname;
    return host.replace(/^www\./, '') || undefined;
  } catch {
    return undefined;
  }
}

/** Distinct items in call order, at most MAX_ITEMS of them shown. */
function label(verb: string, items: (GroupItem | undefined)[]): GroupLabel {
  const seen = new Set<string>();
  const unique: GroupItem[] = [];
  for (const item of items) {
    if (!item) continue;
    const key = item.path ?? item.text;
    if (seen.has(key)) continue;
    seen.add(key);
    unique.push(item);
  }
  return {
    verb,
    items: unique.slice(0, MAX_ITEMS),
    more: Math.max(0, unique.length - MAX_ITEMS),
  };
}

function file(args: Args): GroupItem | undefined {
  const path = str(args.path)?.trim();
  if (!path) return undefined;
  return { text: fileName(path), path, line: lineArg(args.start_line), title: path };
}

function quoted(value: unknown): GroupItem | undefined {
  const text = str(value)?.trim();
  return text ? { text: `"${short(text)}"`, title: text } : undefined;
}

export interface LineCounts {
  added: number;
  removed: number;
}

/**
 * Lines added and removed by a group's writes, as each one's own header
 * counts them: an edit (a quoted piece of a file replaced) from its
 * arguments, like EditDiffBlock, where an edited line is one removed and one
 * added; a whole-file write from its result, which compared the file before
 * (results from before that have no counts). Edits to the agent's context
 * aren't counted. Undefined when nothing could be counted.
 */
export function editCounts(
  writes: { args: Args; result?: Partial<LineCounts> }[]
): LineCounts | undefined {
  let counted = 0;
  let added = 0;
  let removed = 0;
  for (const { args, result } of writes) {
    if (isLiveContextPath(str(args.path))) continue;
    const oldText = str(args.old_text);
    const newText = str(args.content);
    if (oldText !== undefined && newText !== undefined) {
      counted += 1;
      for (const row of diffRows(snippetLines(oldText), snippetLines(newText))) {
        if (row.kind === 'added') added += 1;
        else if (row.kind === 'removed') removed += 1;
      }
    } else if (oldText === undefined && result?.added !== undefined) {
      counted += 1;
      added += result.added;
      removed += result.removed ?? 0;
    }
  }
  return counted ? { added, removed } : undefined;
}

/** The group's line, e.g. { verb: "Edited", items: [App.tsx, main.tsx] }. */
export function describeGroup(
  name: string,
  argsList: Args[],
  active: boolean,
  /** Each call's stored result facts, where it has finished. */
  results: ({ dir?: string } | undefined)[] = []
): GroupLabel {
  const verb = (running: string, done: string) => (active ? running : done);
  switch (name) {
    case 'read':
      return label(verb('Reading', 'Read'), argsList.map(file));
    case 'write': {
      const edits = argsList.some((a) => isPartialWrite(name, a));
      if (argsList.length && argsList.every((a) => isLiveContextPath(str(a.path)))) {
        return edits
          ? label(verb('Editing its context', 'Edited its context'), [])
          : label(verb('Rewriting its context', 'Rewrote its context'), []);
      }
      return label(
        edits ? verb('Editing', 'Edited') : verb('Writing', 'Wrote'),
        argsList.map(file)
      );
    }
    case 'outline':
      return label(verb('Outlining', 'Outlined'), argsList.map(file));
    case 'neighbours':
      return label(verb('Mapping neighbours of', 'Mapped neighbours of'), argsList.map(file));
    case 'diagnostics':
      return label(
        verb('Checking', 'Checked'),
        argsList.map((a) => file(a) ?? { text: 'all files' })
      );
    case 'list_dir':
      return label(
        verb('Listing', 'Listed'),
        argsList.map((a, i) => {
          const dir = dirName(str(a.path), results[i]?.dir);
          return { text: short(dir), title: dir };
        })
      );
    case 'bash':
      return label(
        verb('Running', 'Ran'),
        argsList.map((a) => {
          const command = commandName(str(a.command));
          return command ? { text: command, title: str(a.command) } : undefined;
        })
      );
    case 'browse_url':
      return label(
        verb('Browsing', 'Browsed'),
        argsList.map((a) => {
          const domain = domainName(str(a.url));
          return domain ? { text: domain, title: str(a.url) } : undefined;
        })
      );
    case 'browser_screenshot':
      return label(
        verb('Taking screenshots of', 'Took screenshots of'),
        argsList.map((a) => {
          const domain = domainName(str(a.url));
          return domain ? { text: domain, title: str(a.url) } : { text: 'the current page' };
        })
      );
    case 'search_code':
      return label(
        verb('Searching for', 'Searched for'),
        argsList.map((a) => quoted(a.query))
      );
    case 'web_search':
      return label(
        verb('Searching the web for', 'Searched the web for'),
        argsList.map((a) => quoted(a.query))
      );
    case 'callers':
      return label(
        verb('Finding callers of', 'Found callers of'),
        argsList.map((a) => quoted(a.symbol))
      );
    case 'todo': {
      const text = todoSummary(argsList[argsList.length - 1] ?? {}, (t) => short(t, 60));
      return label(verb('Updating the checklist', 'Updated the checklist'), [
        text ? { text } : undefined,
      ]);
    }
    case 'submit_plan':
      return label(
        verb('Saving the plan', 'Saved the plan'),
        argsList.map((a) => {
          const title = planTitle(str(a.plan))?.trim();
          return title ? { text: short(title, 60), title } : undefined;
        })
      );
    case 'ask_user':
      return label(
        verb('Asking', 'Asked'),
        argsList.flatMap((a) =>
          (Array.isArray(a.questions) ? a.questions : [a]).map((q) => {
            const text = str((q as Args | null)?.question)?.trim();
            return text ? { text: short(text, 60), title: text } : undefined;
          })
        )
      );
    case 'task': {
      // "Scout: where is the parser?" (a follow-up says so).
      const a = argsList[0] ?? {};
      const goal = str(a.goal)?.trim();
      const kind = str(a.continue) ? 'Follow-up' : kindLabel(str(a.kind));
      return label(kind, [goal ? { text: short(goal, 60), title: goal } : undefined]);
    }
    case 'restore_context':
      return label(verb('Restoring its context', 'Restored its context'), []);
    default:
      return label(verb(`Calling ${name || 'tool'}`, `Called ${name || 'tool'}`), []);
  }
}
