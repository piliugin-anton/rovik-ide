// ToolCallBlock.tsx
//
// One collapsible row per tool invocation. The header is a single readable
// sentence ("Read src/App.tsx", "Searched for "foo" · 12 matches"); expanding
// it shows the call arguments and the result together.
import { createMemo, createResource, createSignal, For, Show } from 'solid-js';
import type { ChatMessage, ChatToolCall, ToolMeta } from '../types';
import StaticCodeBlock from './StaticCodeBlock';
import { attachmentUrl, chatApi } from '../lib/chatApi';
import { extensionOf } from '../lib/language';

export interface ToolCallBlockProps {
  toolCall: ChatToolCall;
  /** Content of the matching role:"tool" message, if it has arrived. */
  result?: ChatMessage;
  /** True while the agent loop is still running (args streaming or tool executing). */
  running?: boolean;
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
    return out;
  }
}

const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
const num = (v: unknown) => (typeof v === 'number' ? v : undefined);

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
}

/**
 * Header counts for a finished tool result. Computed once when the tool
 * finishes and stored with the message, so the header never needs the full
 * body (which may only be loaded on expand).
 */
export function toolResultMeta(name: string, result: string): ToolMeta | undefined {
  if (isErrorResult(result)) return undefined;
  switch (name) {
    case 'read_file':
      return { count: countLines(result) };
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
      const range =
        start !== undefined || end !== undefined
          ? `lines ${start ?? 1}–${end ?? 'end'}`
          : undefined;
      const title = `${active ? 'Reading' : 'Read'} ${path ?? 'file'}`;
      if (count === undefined) return { title, meta: range };
      const lines = plural(count, 'line');
      return { title, meta: range ? `${range}, ${lines}` : lines };
    }

    case 'write_file': {
      const title = `${active ? 'Writing' : 'Wrote'} ${path ?? 'file'}`;
      const content = str(args.content);
      return {
        title,
        meta: content !== undefined ? plural(countLines(content), 'line') : undefined,
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

export default function ToolCallBlock(props: ToolCallBlockProps) {
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

  const summary = createMemo(() => describe(props.toolCall.name, args(), status(), resultMeta()));

  // A large result arrives as a preview; the full text loads on expand.
  const [fullBody] = createResource(
    () => (expanded() && props.result?.bodyExternal ? props.result.id : null),
    loadBody
  );
  const resultText = () =>
    props.result?.bodyExternal ? (fullBody() ?? undefined) : props.result?.content;

  const meta = () => (status() === 'error' ? 'failed' : summary().meta);

  const language = () => {
    const path = str(args().path);
    return path ? extensionOf(path) : undefined;
  };

  // write_file's `content` is shown as a highlighted code block instead of
  // an escaped JSON string, so drop it from the argument dump.
  const isWrite = () => props.toolCall.name === 'write_file';
  const isRead = () => props.toolCall.name === 'read_file';
  const callText = () => prettyArgs(props.toolCall.arguments, isWrite() ? ['content'] : []);
  const hasCallText = () => {
    const t = callText().trim();
    return t !== '' && t !== '{}';
  };

  return (
    <div
      class="tool-call"
      classList={{
        expanded: expanded(),
        running: status() === 'running',
        error: status() === 'error',
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
        <Show when={status() === 'running'}>
          <span class="tool-call-spinner" aria-label="Running" />
        </Show>
      </button>

      <Show when={expanded()}>
        <div class="tool-call-body">
          <section class="tool-call-section">
            <div class="tool-call-label">Call</div>
            <Show when={hasCallText()}>
              <pre class="tool-call-pre">{callText()}</pre>
            </Show>
            <Show when={isWrite() && str(args().content) !== undefined}>
              <StaticCodeBlock code={str(args().content)!} language={language()} />
            </Show>
          </section>

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
                  <StaticCodeBlock code={resultText()!} language={language()} />
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
