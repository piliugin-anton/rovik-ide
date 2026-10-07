// ToolCallBlock.tsx
//
// One collapsible row per tool invocation. The header is a single readable
// sentence ("Read src/App.tsx", "Searched for "foo" · 12 matches"); expanding
// it shows the call arguments and the result together.
import { createMemo, createResource, createSignal, For, Show } from 'solid-js';
import type { ChatMessage, ChatToolCall, ReviewCheck, ToolApproval, ToolMeta } from '../types';
import StaticCodeBlock from './StaticCodeBlock';
import EditDiffBlock from './EditDiffBlock';
import QuestionCard from './QuestionCard';
import { StatusIcon } from './TodoPanel';
import { parseTodo, progressLabel } from '../lib/todo';
import { attachmentUrl, chatApi } from '../lib/chatApi';
import { extensionOf } from '../lib/language';
import { diffRows, snippetLines } from '../lib/lineDiff';
import { describeToolCall } from '../lib/permissions';
import { APPROVE_AT, REJECT_AT, reviewChecks } from '../lib/aiApproval';
import { requestOpenFile } from '../lib/editorBridge';
import {
  allow,
  decideRemaining,
  deny,
  hunkCounts,
  pendingApproval,
  revealEdit,
  type PendingApproval,
} from '../lib/approvals';

export interface ToolCallBlockProps {
  toolCall: ChatToolCall;
  /** Content of the matching role:"tool" message, if it has arrived. */
  result?: ChatMessage;
  /** True while the agent loop is still running (args streaming or tool executing). */
  running?: boolean;
  /** Live note while the call waits, e.g. for approval. */
  note?: string;
}

/*
 * Full bodies of large results, loaded on expand. Small LRU: 20 bodies or
 * 2 MB, whichever fills first.
 */
const BODY_CACHE_ENTRIES = 20;
const BODY_CACHE_CHARS = 2_000_000;
const bodyCache = new Map<string, string>();

async function loadBody(messageId: string): Promise<string> {
  const cached = bodyCache.get(messageId);
  if (cached !== undefined) {
    bodyCache.delete(messageId);
    bodyCache.set(messageId, cached); // most recently used last
    return cached;
  }
  const body = await chatApi.getMessageBody(messageId);
  bodyCache.set(messageId, body);
  let size = [...bodyCache.values()].reduce((n, b) => n + b.length, 0);
  for (const [key, value] of bodyCache) {
    if (bodyCache.size <= BODY_CACHE_ENTRIES && size <= BODY_CACHE_CHARS) break;
    if (key === messageId) continue;
    bodyCache.delete(key);
    size -= value.length;
  }
  return body;
}

type Args = Record<string, unknown>;
type Status = 'running' | 'done' | 'error' | 'idle';

/**
 * Arguments stream in as partial JSON, so a strict parse fails until the
 * last chunk lands. Fall back to pulling complete string/number fields out
 * with a regex so the header can say "Reading src/foo.ts" mid-stream.
 */
function parseArgsLenient(raw: string): Args {
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

const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
const num = (v: unknown) => (typeof v === 'number' ? v : undefined);

/** A line-number argument; models sometimes send it as a string ("12"). */
function lineArg(v: unknown): number | undefined {
  const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : num(v);
  return n !== undefined && Number.isFinite(n) && n >= 1 ? Math.floor(n) : undefined;
}

function countLines(text: string): number {
  if (!text) return 0;
  return text.split(/\r?\n/).length;
}

function plural(n: number, one: string, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

function isErrorResult(result?: string) {
  return result !== undefined && /^Error:/.test(result.trimStart());
}

/** "https://docs.rs/tokio/latest/" → "docs.rs/tokio/latest" */
function shortUrl(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  const stripped = raw.replace(/^https?:\/\//, '').replace(/\/$/, '');
  return stripped.length > 60 ? `${stripped.slice(0, 57)}…` : stripped;
}

interface Summary {
  /** Main sentence, e.g. "Read src/App.tsx". */
  title: string;
  /** Secondary, muted detail, e.g. "120 lines". */
  meta?: string;
  /** Lines added and removed (edits), shown green and red. */
  counts?: { added: number; removed: number };
}

/**
 * Header counts for a finished tool result. Computed once when the tool
 * finishes and stored with the message, so the header never needs the full
 * body (which may only be loaded on expand).
 */
export function toolResultMeta(name: string, result: string): ToolMeta | undefined {
  if (isErrorResult(result)) return undefined;
  switch (name) {
    case 'read_file': {
      // By symbol: the header names the lines it found (read_symbol.rs).
      const found = result.match(/^\[Symbol read: .*?, lines (\d+)-(\d+) of /);
      return found
        ? { count: countLines(result), range: [Number(found[1]), Number(found[2])] }
        : { count: countLines(result) };
    }
    case 'list_dir':
      return { count: result.startsWith('(empty') ? 0 : countLines(result) };
    case 'search_code': {
      // First line: "12 matches in 3 files" (context lines aren't matches).
      if (result.trim() === 'No matches.') return { count: 0 };
      const count = Number(result.match(/^(\d+) match/)?.[1]);
      return Number.isFinite(count) ? { count } : { count: countLines(result) };
    }
    case 'web_search':
      return { count: result.split('\n').filter((line) => /^\d+\. /.test(line)).length };
    case 'run_command': {
      const line = result.split('\n')[1] ?? '';
      const code = line.match(/^exit code (-?\d+)/)?.[1];
      return { exitCode: code !== undefined ? Number(code) : null };
    }
    case 'browse_url': {
      const pageTitle = result.match(/^# (.+)$/m)?.[1];
      return pageTitle && pageTitle !== '(untitled)' ? { pageTitle } : {};
    }
    default:
      return undefined;
  }
}

function describe(name: string, args: Args, status: Status, meta?: ToolMeta): Summary {
  const active = status === 'running';
  const failed = status === 'error';
  const path = str(args.path);
  const count = failed ? undefined : meta?.count;

  switch (name) {
    case 'read_file': {
      const start = num(args.start_line);
      const end = num(args.end_line);
      const symbol = str(args.symbol)?.trim();
      const found = meta?.range;
      const range =
        start !== undefined || end !== undefined
          ? `lines ${start ?? 1}–${end ?? 'end'}`
          : found
            ? `lines ${found[0]}–${found[1]}`
            : undefined;
      const title = symbol
        ? `${active ? 'Reading' : 'Read'} ${symbol} in ${path ?? 'file'}`
        : `${active ? 'Reading' : 'Read'} ${path ?? 'file'}`;
      if (count === undefined) return { title, meta: range };
      const lines = plural(count, 'line');
      return { title, meta: range ? `${range}, ${lines}` : lines };
    }

    case 'submit_plan': {
      const plan = str(args.plan);
      const title = plan?.match(/^#\s+(.+)$/m)?.[1];
      return {
        title: active ? 'Saving the plan' : 'Saved the plan',
        meta: title ?? (plan ? plural(countLines(plan), 'line') : undefined),
      };
    }

    case 'todo': {
      const items = parseTodo(args);
      const current = items.find((i) => i.status === 'in_progress');
      return {
        title: 'Checklist',
        meta: items.length
          ? `${progressLabel(items)}${current ? ` · ${current.title}` : ''}`
          : undefined,
      };
    }

    case 'edit_file': {
      const title = `${active ? 'Editing' : 'Edited'} ${path ?? 'file'}`;
      const start = lineArg(args.start_line);
      const end = lineArg(args.end_line) ?? start;
      const range = start !== undefined ? `only lines ${start}–${end}` : undefined;
      const oldText = str(args.old_text);
      const newText = str(args.new_text);
      // The same line diff as the expanded view (EditDiffBlock), once the
      // arguments are complete: an edited line counts as one removed and one added.
      let counts: Summary['counts'];
      if (!active && oldText !== undefined && newText !== undefined) {
        const rows = diffRows(snippetLines(oldText), snippetLines(newText));
        counts = {
          added: rows.filter((r) => r.kind === 'added').length,
          removed: rows.filter((r) => r.kind === 'removed').length,
        };
      }
      return { title, meta: range, counts };
    }

    case 'write_file': {
      const title = `${active ? 'Writing' : 'Wrote'} ${path ?? 'file'}`;
      const content = str(args.content);
      if (content === undefined) return { title };
      const lines = plural(countLines(content), 'line');
      return { title, meta: active ? `${lines} so far` : lines };
    }

    case 'run_command': {
      const command = str(args.command);
      const shown = command && command.length > 60 ? `${command.slice(0, 57)}…` : command;
      const title = `${active ? 'Running' : 'Ran'} ${shown ? `\`${shown}\`` : 'a command'}`;
      if (failed || meta?.exitCode === undefined) return { title };
      return {
        title,
        meta:
          meta.exitCode === null ? 'killed' : meta.exitCode === 0 ? 'ok' : `exit ${meta.exitCode}`,
      };
    }

    case 'list_dir': {
      const title = `${active ? 'Listing' : 'Listed'} ${path || 'project root'}`;
      if (count === undefined) return { title };
      return { title, meta: count === 0 ? 'empty' : plural(count, 'entry', 'entries') };
    }

    case 'search_code': {
      const query = str(args.query);
      const glob = str(args.path_glob);
      const title =
        `${active ? 'Searching' : 'Searched'} for ${query ? `"${query}"` : 'text'}` +
        (glob ? ` in ${glob}` : '');
      if (count === undefined) return { title };
      return { title, meta: count === 0 ? 'no matches' : plural(count, 'match', 'matches') };
    }

    case 'web_search': {
      const query = str(args.query);
      const title = `${active ? 'Searching' : 'Searched'} the web for ${query ? `"${query}"` : '…'}`;
      if (count === undefined) return { title };
      return { title, meta: count ? plural(count, 'result') : 'no results' };
    }

    case 'browse_url': {
      const url = shortUrl(str(args.url));
      const title = `${active ? 'Opening' : 'Read'} ${url ?? 'page'}`;
      return { title, meta: !failed && meta?.pageTitle ? meta.pageTitle.slice(0, 40) : undefined };
    }

    case 'restore_context': {
      const revision = str(args.revision) ?? num(args.revision)?.toString();
      const what =
        revision?.trim().toLowerCase() === 'stored'
          ? 'the stored chat'
          : revision
            ? `revision ${revision}`
            : 'an earlier context';
      return { title: `${active ? 'Restoring' : 'Restored'} its context to ${what}` };
    }

    case 'browser_screenshot': {
      const url = shortUrl(str(args.url));
      return {
        title: `${active ? 'Taking' : 'Took'} a screenshot of ${url ?? 'the current page'}`,
      };
    }

    default:
      return { title: `${active ? 'Calling' : 'Called'} ${name || 'tool'}` };
  }
}

function prettyArgs(raw: string, omit: string[] = []): string {
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      for (const key of omit) delete parsed[key];
    }
    return JSON.stringify(parsed, null, 2);
  } catch {
    return raw;
  }
}

function ChevronIcon() {
  return (
    <svg class="tool-call-chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path
        d="M6 3.5 10.5 8 6 12.5"
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}

/**
 * The request for the user's decision, under the tool call's header. File
 * edits are reviewed change by change in the editor; this offers the
 * shortcuts.
 */
/** One check's score as a bar, with the reject and approve thresholds marked. */
function CheckRow(props: { check: ReviewCheck }) {
  const percent = () => Math.round(props.check.score * 100);
  const level = () =>
    props.check.score >= APPROVE_AT ? 'pass' : props.check.score <= REJECT_AT ? 'fail' : 'unsure';
  return (
    <li class={level()}>
      <span class="tool-call-approval-check">{props.check.label}</span>
      <span class="tool-call-approval-bar" aria-hidden="true">
        <span class="tool-call-approval-fill" style={{ width: `${percent()}%` }} />
        <span class="tool-call-approval-tick" style={{ left: `${REJECT_AT * 100}%` }} />
        <span class="tool-call-approval-tick" style={{ left: `${APPROVE_AT * 100}%` }} />
      </span>
      <span class="tool-call-approval-score">{percent()}%</span>
    </li>
  );
}

/**
 * Who decided and why. A typed reviewer's scores show as bars, folded
 * into one line when every check passed; a text reviewer's reason shows
 * as written.
 */
function ApprovalDetails(props: { approval: ToolApproval }) {
  const checks = () => (props.approval.by === 'ai' ? reviewChecks(props.approval) : undefined);
  const allPassed = () => checks()?.every((c) => c.score >= APPROVE_AT) ?? false;
  const [open, setOpen] = createSignal(false);
  const shown = () => open() || !allPassed();
  return (
    <div class="tool-call-approval" classList={{ rejected: !props.approval.approved }}>
      <div class="tool-call-approval-verdict">
        <span class="tool-call-approval-mark" aria-hidden="true">
          {props.approval.approved ? '✓' : '✕'}
        </span>
        {props.approval.approved ? 'Approved' : 'Rejected'}
        {props.approval.by === 'ai' ? ' by AI reviewer' : ' by you'}
        <Show when={allPassed()}>
          <button
            class="tool-call-approval-toggle"
            aria-expanded={open()}
            onClick={() => setOpen(!open())}
          >
            {checks()!.length === 1 ? '1 check passed' : `${checks()!.length} checks passed`}
            <span aria-hidden="true">{open() ? ' ▾' : ' ▸'}</span>
          </button>
        </Show>
      </div>
      <Show when={props.approval.by === 'ai' && props.approval.model}>
        <div class="tool-call-approval-model" title={props.approval.model}>
          {props.approval.model}
        </div>
      </Show>
      <Show
        when={checks()}
        fallback={
          <Show when={props.approval.reason}>
            <div class="tool-call-approval-reason">{props.approval.reason}</div>
          </Show>
        }
      >
        {(list) => (
          <Show when={shown()}>
            <ul class="tool-call-approval-checks">
              <For each={list()}>{(c) => <CheckRow check={c} />}</For>
            </ul>
          </Show>
        )}
      </Show>
    </div>
  );
}

function ApprovalBar(props: { request: PendingApproval }) {
  const id = () => props.request.toolCallId;
  const counts = () => (props.request.edit ? hunkCounts(props.request.edit) : null);

  const status = () => {
    const c = counts();
    if (!c) return 'Waiting for your approval';
    if (c.pending === c.total) {
      return `Waiting for your review · ${c.total} change${c.total === 1 ? '' : 's'}`;
    }
    return `${c.pending} of ${c.total} left · ${c.accepted} accepted · ${c.rejected} rejected`;
  };

  return (
    <div class="tool-approval" role="group" aria-label="Approval">
      <div class="tool-approval-status">
        <span class="tool-approval-dot" aria-hidden="true" />
        {status()}
      </div>
      <Show when={props.request.note}>
        <div class="tool-approval-note">{props.request.note}</div>
      </Show>
      <Show when={props.request.command}>
        {(command) => <pre class="tool-approval-command">{command()}</pre>}
      </Show>
      <div class="tool-approval-actions">
        <Show
          when={props.request.edit}
          fallback={
            <>
              <button class="tool-approval-button primary" onClick={() => allow(id())}>
                Allow
              </button>
              <button class="tool-approval-button" onClick={() => deny(id())}>
                Deny
              </button>
            </>
          }
        >
          <button
            class="tool-approval-button primary"
            onClick={() => revealEdit(id())}
            title="Open the file with the changes highlighted"
          >
            Review in editor
          </button>
          <button
            class="tool-approval-button"
            onClick={() => decideRemaining(id(), 'accepted')}
            title="Accept every remaining change"
          >
            Accept all
          </button>
          <button
            class="tool-approval-button danger"
            onClick={() => decideRemaining(id(), 'rejected')}
            title="Reject every remaining change"
          >
            Reject all
          </button>
        </Show>
      </div>
    </div>
  );
}

export default function ToolCallBlock(props: ToolCallBlockProps) {
  // The agent's questions have their own card, always open.
  if (props.toolCall.name === 'ask_user') {
    return <QuestionCard toolCall={props.toolCall} result={props.result} running={props.running} />;
  }
  const request = () => pendingApproval(props.toolCall.id);

  const [expanded, setExpanded] = createSignal(false);

  const args = createMemo(() => parseArgsLenient(props.toolCall.arguments));

  const status = createMemo<Status>(() => {
    if (props.result === undefined) return props.running ? 'running' : 'idle';
    return isErrorResult(props.result.content) ? 'error' : 'done';
  });

  // Stored counts; computed from the text only for results that predate them.
  const resultMeta = createMemo(() => {
    const r = props.result;
    if (!r) return undefined;
    return r.meta ?? (r.bodyExternal ? undefined : toolResultMeta(props.toolCall.name, r.content));
  });

  // A call with no result that isn't running never ran (e.g. the app was
  // closed while it waited for approval): say so instead of a past tense.
  const summary = createMemo(() =>
    status() === 'idle' && !request()
      ? { title: describeToolCall(props.toolCall.name, props.toolCall.arguments), meta: 'not run' }
      : describe(props.toolCall.name, args(), status(), resultMeta())
  );

  // A large result arrives as a preview; the full text loads on expand.
  const [fullBody] = createResource(
    () => (expanded() && props.result?.bodyExternal ? props.result.id : null),
    loadBody
  );
  const resultText = () =>
    props.result?.bodyExternal ? (fullBody() ?? undefined) : props.result?.content;

  const approval = () => props.result?.meta?.approval;

  const meta = () => {
    if (request()) return undefined;
    if (status() === 'running' && props.note) return props.note;
    const a = approval();
    if (a && !a.approved) return a.by === 'ai' ? 'rejected by AI' : 'denied';
    const base = status() === 'error' ? 'failed' : summary().meta;
    const by = a?.by === 'ai' ? 'approved by AI' : undefined;
    return [base, by].filter(Boolean).join(' · ') || undefined;
  };

  const language = () => {
    const path = str(args().path);
    return path ? extensionOf(path) : undefined;
  };

  // write_file's `content` is shown as a highlighted code block instead of
  // an escaped JSON string, so drop it from the argument dump.
  const isWrite = () => props.toolCall.name === 'write_file';
  const isEdit = () => props.toolCall.name === 'edit_file';
  const isRead = () => props.toolCall.name === 'read_file';
  /*
   * "Open in editor" on any code a file tool shows (read, write, edit) opens
   * that file in the editor, never an inline editor in the message. The line
   * to center: the range the tool worked on, else where the edit landed
   * (edit_file's result names it), else the top.
   */
  /** edit_file: the file line where the edit landed, from its result. */
  const editLandedLine = (): number | undefined => {
    if (props.toolCall.name !== 'edit_file') return undefined;
    const landed = Number(resultText()?.match(/(?:replaced|removed) lines? (\d+)/)?.[1]);
    return Number.isFinite(landed) && landed >= 1 ? landed : undefined;
  };

  /** Where to open the file: a known line, else code to find in it. */
  const openTarget = (): { line?: number; snippet?: string } => {
    const a = args();
    const start = lineArg(a.start_line) ?? resultMeta()?.range?.[0];
    if (start !== undefined) return { line: start };
    if (props.toolCall.name === 'edit_file') {
      const landed = editLandedLine();
      if (landed !== undefined) return { line: landed };
      // Not applied (yet): the text it replaces is still in the file.
      const oldText = str(a.old_text);
      const failedOrPending = !resultText() || resultText()!.startsWith('Error:');
      const snippet = failedOrPending ? oldText : str(a.new_text) || oldText;
      if (snippet) return { snippet };
    }
    return { line: 1 };
  };
  const openInEditor = () => {
    const path = str(args().path);
    if (path) requestOpenFile({ path, ...openTarget() });
  };

  // Code arguments are shown highlighted below, not as escaped JSON strings.
  const callText = () =>
    prettyArgs(
      props.toolCall.arguments,
      isWrite()
        ? ['content']
        : isEdit()
          ? ['old_text', 'new_text']
          : props.toolCall.name === 'todo'
            ? ['items']
            : props.toolCall.name === 'submit_plan'
              ? ['plan']
              : []
    );
  const hasCallText = () => {
    const t = callText().trim();
    return t !== '' && t !== '{}';
  };

  return (
    <div
      class="tool-call"
      classList={{
        expanded: expanded(),
        awaiting: !!request(),
        running: status() === 'running',
        error: status() === 'error' || approval()?.approved === false,
        'not-run': status() === 'idle' && !request(),
      }}
    >
      <button
        type="button"
        class="tool-call-header"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded()}
        title={summary().title}
      >
        <ChevronIcon />
        <span class="tool-call-title">{summary().title}</span>
        <Show when={meta()}>
          <span class="tool-call-meta">{meta()}</span>
        </Show>
        <Show when={!request() && status() !== 'error' && summary().counts}>
          {(counts) => (
            <span
              class="tool-call-counts"
              title={`${counts().added} lines added, ${counts().removed} removed`}
            >
              <span class="added">+{counts().added}</span>
              <span class="removed">−{counts().removed}</span>
            </span>
          )}
        </Show>
        <Show when={status() === 'running' && !request()}>
          <span class="tool-call-spinner" aria-label="Running" />
        </Show>
      </button>

      <Show when={request()}>{(r) => <ApprovalBar request={r()} />}</Show>

      <Show when={expanded()}>
        <div class="tool-call-body">
          <section class="tool-call-section">
            <div class="tool-call-label">Call</div>
            <Show when={hasCallText()}>
              <pre class="tool-call-pre">{callText()}</pre>
            </Show>
            <Show when={isWrite() && str(args().content) !== undefined}>
              <StaticCodeBlock
                code={str(args().content)!}
                language={language()}
                onOpenInEditor={openInEditor}
              />
            </Show>
            <Show when={props.toolCall.name === 'submit_plan' && str(args().plan)}>
              {(plan) => <StaticCodeBlock code={plan()} language="markdown" />}
            </Show>
            <Show when={props.toolCall.name === 'todo'}>
              <ol class="todo-list tool-todo-list">
                <For each={parseTodo(args())}>
                  {(item) => (
                    <li class={`todo-item ${item.status}`}>
                      <StatusIcon status={item.status} />
                      <span class="todo-item-text">
                        <span class="todo-item-title">{item.title}</span>
                        <Show when={item.note}>
                          <span class="todo-item-note">{item.note}</span>
                        </Show>
                      </span>
                    </li>
                  )}
                </For>
              </ol>
            </Show>
            <Show when={isEdit() && str(args().old_text) !== undefined}>
              <div class="tool-call-label">Change</div>
              <Show
                when={str(args().new_text) !== undefined}
                fallback={<div class="tool-call-pending">…</div>}
              >
                <EditDiffBlock
                  oldText={str(args().old_text)!}
                  newText={str(args().new_text)!}
                  language={language()}
                  startLine={editLandedLine()}
                  onOpenInEditor={openInEditor}
                />
              </Show>
            </Show>
          </section>

          <Show when={approval()}>
            {(a) => (
              <section class="tool-call-section">
                <div class="tool-call-label">Approval</div>
                <ApprovalDetails approval={a()} />
              </section>
            )}
          </Show>

          <section class="tool-call-section">
            <div class="tool-call-label">Result</div>
            <Show
              when={props.result !== undefined}
              fallback={
                <div class="tool-call-pending">
                  {status() === 'running' ? 'Waiting for result…' : 'No result'}
                </div>
              }
            >
              <Show
                when={resultText() !== undefined}
                fallback={
                  <div class="tool-call-pending">
                    {fullBody.error ? 'Could not load the result' : 'Loading…'}
                  </div>
                }
              >
                <Show
                  when={isRead() && status() === 'done'}
                  fallback={<pre class="tool-call-pre">{resultText()}</pre>}
                >
                  <StaticCodeBlock
                    code={resultText()!}
                    language={language()}
                    onOpenInEditor={openInEditor}
                  />
                </Show>
              </Show>
              <For each={props.result?.attachments ?? []}>
                {(id) => (
                  <img class="tool-call-image" src={attachmentUrl(id)} alt="Browser screenshot" />
                )}
              </For>
            </Show>
          </section>
        </div>
      </Show>
    </div>
  );
}
