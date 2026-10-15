// ChatList.tsx
//
// The chat list with its search box. Rows have fixed heights, so only the
// rows in view (plus overscan) are rendered, positioned from a prefix-sum
// of row heights; the next page is requested as the end comes near.
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Index,
  on,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import type { DialogSummary } from '../types';
import { chatBadges } from '../lib/chatBadges';
import type { HostSummary } from '../lib/cloud';
import type { Highlighted } from '../lib/chatSearch';
import HighlightedText from './HighlightedText';

const ROW_HEIGHT = 44;
const ROW_WITH_SNIPPET = 60;
const OVERSCAN = 10;
const LOAD_MORE_WITHIN = 20;

export interface ChatListRow {
  dialog: DialogSummary;
  title: Highlighted;
  snippet?: Highlighted;
  /** The matched message, when the row is a search hit. */
  messageId?: string;
}

export default function ChatList(props: {
  rows: ChatListRow[];
  activeId: string | null;
  query: string;
  onQueryChange: (query: string) => void;
  /** Search finished and found nothing (vs. still typing). */
  noMatches: boolean;
  loading: boolean;
  onSelect: (row: ChatListRow) => void;
  onDelete: (dialog: DialogSummary) => void;
  onNearEnd: () => void;
  /** Chats with unseen agent messages (dot indicator), by dialog id. */
  unreadIds?: Record<string, true>;
  /** Chats with an agent run going on (spinner indicator), by dialog id. */
  runningIds?: Record<string, true>;
  /** Chats an approval or a question waits in, by dialog id. */
  waitingIds?: Record<string, true>;
  /** The cloud hosts, for the badges of chats on them. */
  hosts?: HostSummary[];
}) {
  let scrollRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewport, setViewport] = createSignal(280);
  const [highlight, setHighlight] = createSignal(-1);

  const heightOf = (row: ChatListRow) => (row.snippet ? ROW_WITH_SNIPPET : ROW_HEIGHT);

  // offsets[i] = top of row i; offsets[n] = total height.
  const offsets = createMemo(() => {
    const out = [0];
    for (const row of props.rows) out.push(out[out.length - 1] + heightOf(row));
    return out;
  });

  /** Index of the row containing y (binary search over offsets). */
  function rowAt(y: number): number {
    const o = offsets();
    let lo = 0;
    let hi = o.length - 2;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (o[mid] <= y) lo = mid;
      else hi = mid - 1;
    }
    return Math.max(0, lo);
  }

  const range = createMemo(() => {
    const count = props.rows.length;
    if (count === 0) return { from: 0, to: 0 };
    const from = Math.max(0, rowAt(scrollTop()) - OVERSCAN);
    const to = Math.min(count, rowAt(scrollTop() + viewport()) + 1 + OVERSCAN);
    return { from, to };
  });

  // <Index> (by position) below: scrolling reuses row DOM with new data.
  const visibleRows = createMemo(() =>
    props.rows.slice(range().from, range().to).map((row, i) => ({ row, index: range().from + i }))
  );

  createEffect(() => {
    if (props.rows.length > 0 && range().to >= props.rows.length - LOAD_MORE_WITHIN)
      props.onNearEnd();
  });

  // A new query or result set starts at the top.
  createEffect(
    on(
      () => props.query,
      () => {
        setHighlight(-1);
        if (scrollRef) scrollRef.scrollTop = 0;
        setScrollTop(0);
      },
      { defer: true }
    )
  );

  onMount(() => {
    if (!scrollRef) return;
    setViewport(scrollRef.clientHeight || 280);
    const observer = new ResizeObserver(() => setViewport(scrollRef!.clientHeight || 280));
    observer.observe(scrollRef);
    onCleanup(() => observer.disconnect());
    searchRef?.focus();
  });

  function keepInView(index: number) {
    const el = scrollRef;
    if (!el) return;
    const top = offsets()[index];
    const bottom = offsets()[index + 1];
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }

  function onKeyDown(e: KeyboardEvent) {
    const count = props.rows.length;
    if (e.key === 'ArrowDown' && count) {
      e.preventDefault();
      const next = Math.min(count - 1, highlight() + 1);
      setHighlight(next);
      keepInView(next);
    } else if (e.key === 'ArrowUp' && count) {
      e.preventDefault();
      const next = Math.max(0, highlight() - 1);
      setHighlight(next);
      keepInView(next);
    } else if (e.key === 'Enter' && highlight() >= 0 && highlight() < count) {
      e.preventDefault();
      props.onSelect(props.rows[highlight()]);
    } else if (e.key === 'Escape' && props.query) {
      e.preventDefault();
      props.onQueryChange('');
    }
  }

  return (
    <>
      <div class="chat-history-search">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" stroke-width="1.5" />
          <path
            d="m10.5 10.5 3 3"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
          />
        </svg>
        <input
          ref={searchRef}
          value={props.query}
          onInput={(e) => props.onQueryChange(e.currentTarget.value)}
          onKeyDown={onKeyDown}
          placeholder="Search chats"
          spellcheck={false}
          aria-label="Search chats by title or message"
        />
        <Show when={props.query}>
          <button
            class="chat-history-search-clear"
            onClick={() => props.onQueryChange('')}
            aria-label="Clear search"
            title="Clear"
          >
            ×
          </button>
        </Show>
      </div>

      <Show
        when={props.rows.length > 0}
        fallback={
          <div class="chat-history-empty">
            {props.loading
              ? 'Loading...'
              : props.noMatches
                ? `No chats match "${props.query}"`
                : props.query
                  ? 'Searching...'
                  : 'No conversations yet'}
          </div>
        }
      >
        <div
          ref={scrollRef}
          class="chat-history-list"
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
          role="listbox"
          aria-label="Chats"
        >
          <div class="chat-history-canvas" style={{ height: `${offsets()[props.rows.length]}px` }}>
            <Index each={visibleRows()}>
              {(entry) => (
                <div
                  class="chat-history-item"
                  classList={{
                    active: entry().row.dialog.id === props.activeId,
                    highlighted: entry().index === highlight(),
                  }}
                  style={{
                    top: `${offsets()[entry().index]}px`,
                    height: `${heightOf(entry().row)}px`,
                  }}
                  role="option"
                  aria-selected={entry().row.dialog.id === props.activeId}
                >
                  <Show
                    when={props.runningIds?.[entry().row.dialog.id]}
                    fallback={
                      <Show when={props.unreadIds?.[entry().row.dialog.id]}>
                        <span
                          class="chat-unread-dot"
                          title="New messages"
                          aria-label="New messages from the agent"
                        />
                      </Show>
                    }
                  >
                    <span
                      class="chat-running-spinner"
                      title="The agent is working"
                      aria-label="The agent is working"
                      role="status"
                    />
                  </Show>
                  <button
                    class="chat-history-item-main"
                    onClick={() => props.onSelect(entry().row)}
                  >
                    <span class="chat-history-item-title">
                      <HighlightedText
                        text={entry().row.title.text}
                        ranges={entry().row.title.ranges}
                      />
                    </span>
                    <Show when={entry().row.snippet}>
                      {(snippet) => (
                        <span class="chat-history-item-snippet">
                          <HighlightedText text={snippet().text} ranges={snippet().ranges} />
                        </span>
                      )}
                    </Show>
                    <span class="chat-history-item-meta">
                      <span class="chat-history-item-date">
                        {new Date(entry().row.dialog.updatedAt).toLocaleString()}
                      </span>
                      <For
                        each={chatBadges(
                          entry().row.dialog,
                          !!props.waitingIds?.[entry().row.dialog.id],
                          props.hosts ?? []
                        )}
                      >
                        {(badge) => (
                          <span class={`chat-badge ${badge.kind}`} title={badge.title}>
                            {badge.label}
                          </span>
                        )}
                      </For>
                    </span>
                  </button>
                  <button
                    class="chat-history-delete"
                    title="Delete chat"
                    onClick={() => props.onDelete(entry().row.dialog)}
                  >
                    ×
                  </button>
                </div>
              )}
            </Index>
          </div>
        </div>
      </Show>
    </>
  );
}
