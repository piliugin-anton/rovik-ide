// MessageList.tsx
//
// Renders an open chat's messages through a window of at most WINDOW_MAX
// (lib/messageWindow.ts). Sentinels at the top and bottom move the window,
// loading older or newer pages from the database when the loaded messages
// run out. Scroll position is kept by hand when content changes above the
// viewport (native scroll anchoring isn't reliable across WebKit versions).
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
  untrack,
} from 'solid-js';
import type { ChatMessage } from '../types';
import type { OpenChat } from '../lib/chatStore';
import {
  afterAppend,
  afterPrepend,
  clampWindow,
  initialWindow,
  shiftDown,
  shiftUp,
  type MessageWindow,
} from '../lib/messageWindow';
import { isBlank } from '../lib/textUtils';
import { canRetry, describeFailure, formatDuration } from '../lib/chatErrors';
import MessageContent from './MessageContent';
import ToolCallBlock from './ToolCallBlock';
import VerificationRecord from './VerificationRecord';

const BOTTOM_TOLERANCE_PX = 8;
/** A scroll this soon after wheel/touch/key/scrollbar input is the user's. */
const USER_INPUT_WINDOW_MS = 300;
const AROUND = 25;
const FLASH_MS = 1600;

export interface MessageListController {
  /** Back to the newest messages and follow them (after sending). */
  followLatest(): void;
}

export default function MessageList(props: {
  dialogId: string | null;
  chat: OpenChat | undefined;
  sending: boolean;
  /** Message to scroll to and highlight (a search hit). */
  focusMessageId: string | null;
  onLoadOlder: () => Promise<number>;
  onLoadNewer: () => Promise<number>;
  /** Replace an "opened around a hit" chat with its newest page. */
  onReloadLatest: () => Promise<void>;
  controller?: (c: MessageListController) => void;
  /** Live notes per tool call id (e.g. waiting for approval). */
  toolNotes?: Record<string, string>;
  /** Re-run the last failed turn. */
  onRetry: () => void;
  /** Continue an interrupted reply. */
  onContinue: () => void;
  /** Continue a run that paused (tool-call checkpoint or loop check). */
  onContinueRun: () => void;
  /** Run checks that waited for the user's approval. */
  onRunChecks: () => void;
  /** Open the settings page where the project's checks are set up. */
  onOpenProjectChecks: () => void;
}) {
  let scrollRef: HTMLDivElement | undefined;
  let innerRef: HTMLDivElement | undefined;
  let topSentinel: HTMLDivElement | undefined;
  let bottomSentinel: HTMLDivElement | undefined;

  const messages = (): ChatMessage[] => props.chat?.messages ?? [];
  const [win, setWin] = createSignal<MessageWindow>({ start: 0, end: 0 });
  const [stick, setStick] = createSignal(true);
  const [flashId, setFlashId] = createSignal<string | null>(null);
  const [openDetails, setOpenDetails] = createSignal<string | null>(null);

  // Ticks for countdowns (retry waits, rate-limit resets).
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    onCleanup(() => window.clearInterval(timer));
  });

  /*
   * Retry / Continue only on the latest message, while nothing is running.
   * When the very first request failed, the user's message went back into
   * the input instead (it's marked failed), so there is nothing to retry.
   */
  const isLatest = (m: ChatMessage) => {
    const list = messages();
    return !props.sending && list[list.length - 1]?.id === m.id;
  };
  const requestUndelivered = () => {
    const list = messages();
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i].role === 'user' && !list[i].meta?.harness) return !!list[i].failed;
    }
    return false;
  };

  const statusText = (m: ChatMessage) => {
    const status = m.status;
    if (!status) return 'Thinking...';
    if (!status.until) return status.text;
    const left = status.until - now();
    return left > 500 ? `${status.text} in ${formatDuration(left)}…` : `${status.text}…`;
  };

  // tool_call_id -> its result message, so each call renders with its result.
  const toolResults = createMemo(() => {
    const map = new Map<string, ChatMessage>();
    for (const m of messages()) {
      if (m.role === 'tool' && m.tool_call_id) map.set(m.tool_call_id, m);
    }
    return map;
  });

  // Tool results render inside their call's block, not as rows of their own.
  const visible = createMemo(() => {
    const { start, end } = win();
    return messages()
      .slice(start, end)
      .filter((m) => m.role !== 'tool');
  });

  const atNewest = () => win().end >= messages().length && !props.chat?.hasNewer;

  /*
   * -----------------------------------------------------------------------
   * Scrolling
   * -----------------------------------------------------------------------
   */

  function scrollToBottom() {
    if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
  }

  /** Runs a window change and keeps the first visible message where it was. */
  function keepAnchor(change: () => void) {
    const el = scrollRef;
    if (!el) return change();
    const top = el.getBoundingClientRect().top;
    let anchorId: string | undefined;
    let offset = 0;
    for (const item of el.querySelectorAll<HTMLElement>('.chat-item')) {
      const rect = item.getBoundingClientRect();
      if (rect.bottom > top) {
        anchorId = item.dataset.id;
        offset = rect.top - top;
        break;
      }
    }
    change();
    if (!anchorId) return;
    const again = el.querySelector<HTMLElement>(`.chat-item[data-id="${CSS.escape(anchorId)}"]`);
    if (again) el.scrollTop += again.getBoundingClientRect().top - top - offset;
    lastScrollTop = el.scrollTop;
  }

  /*
   * Following (stick) ends only when the *user* scrolls up. The old check,
   * "not at the bottom on any scroll event", also fired after our own
   * scrollToBottom: by the time that scroll event arrives, items scrolled
   * into view have rendered at their real height (content-visibility), the
   * bottom moved, and following silently switched off — so new messages
   * stopped scrolling into view.
   */
  let lastScrollTop = 0;
  let lastUserInput = 0;
  let draggingScrollbar = false;

  const markUserInput = () => {
    lastUserInput = performance.now();
  };
  const userIsScrolling = () =>
    draggingScrollbar || performance.now() - lastUserInput < USER_INPUT_WINDOW_MS;

  function handleScroll() {
    const el = scrollRef;
    if (!el) return;
    const top = el.scrollTop;
    const movedUp = top < lastScrollTop - 1;
    lastScrollTop = top;
    const distance = el.scrollHeight - top - el.clientHeight;
    if (!atNewest()) setStick(false);
    else if (distance <= BOTTOM_TOLERANCE_PX) setStick(true);
    else if (movedUp && userIsScrolling()) setStick(false);
  }

  function handleWheel(e: WheelEvent) {
    markUserInput();
    // Upward intent ends following at once, even if a scroll-to-bottom is
    // queued for this frame (it checks stick() before scrolling).
    const el = scrollRef;
    if (e.deltaY < 0 && el && el.scrollHeight > el.clientHeight) setStick(false);
  }

  function handlePointerDown(e: PointerEvent) {
    // Only the container itself: a press on its scrollbar, not on content.
    if (e.target !== scrollRef) return;
    draggingScrollbar = true;
    const stop = () => {
      draggingScrollbar = false;
      markUserInput();
      window.removeEventListener('pointerup', stop);
      window.removeEventListener('pointercancel', stop);
    };
    window.addEventListener('pointerup', stop);
    window.addEventListener('pointercancel', stop);
  }

  async function jumpToLatest() {
    if (props.chat?.hasNewer) await props.onReloadLatest();
    setWin(initialWindow(messages().length));
    setStick(true);
    requestAnimationFrame(scrollToBottom);
  }

  props.controller?.({ followLatest: () => void jumpToLatest() });

  /*
   * -----------------------------------------------------------------------
   * Window bookkeeping
   * -----------------------------------------------------------------------
   */

  let prevDialog: string | null = null;
  let prevLength = 0;
  let prevFirstId: string | undefined;

  // Only structure matters here (dialog, count, first id): streamed tokens
  // change a field inside a message and don't re-run this.
  createEffect(
    on(
      () => {
        const list = messages();
        return [props.dialogId, list.length, list[0]?.id] as const;
      },
      ([dialogId, length, firstId]) => {
        if (dialogId !== prevDialog) {
          prevDialog = dialogId;
          setWin(initialWindow(length));
          setStick(true);
          requestAnimationFrame(scrollToBottom);
        } else if (prevFirstId && firstId !== prevFirstId) {
          const shiftedBy = messages().findIndex((m) => m.id === prevFirstId);
          if (shiftedBy > 0) {
            setWin((w) => afterPrepend(w, shiftedBy));
            prevLength += shiftedBy;
          } else {
            setWin(initialWindow(length)); // replaced wholesale (reload)
            prevLength = length;
          }
        }
        if (length !== prevLength) {
          setWin((w) => clampWindow(afterAppend(w, prevLength, length, untrack(stick)), length));
        }
        prevLength = length;
        prevFirstId = firstId;
      }
    )
  );

  // While following, stay at the bottom as the last message grows.
  createEffect(
    on(
      () => {
        const list = messages();
        const last = list[list.length - 1];
        return [list.length, last?.content.length, last?.toolCalls?.length];
      },
      () => {
        if (stick()) requestAnimationFrame(() => stick() && scrollToBottom());
      }
    )
  );

  // Content that resizes after rendering (code colorized, images loaded).
  onMount(() => {
    if (!innerRef) return;
    const observer = new ResizeObserver(() => {
      if (stick()) scrollToBottom();
    });
    observer.observe(innerRef);
    onCleanup(() => observer.disconnect());
  });

  /*
   * Scroll anchoring, by hand. Items render lazily (content-visibility) and
   * code blocks colorize after mounting, so a message's height changes as it
   * nears the viewport. When that happens to a message *above* the viewport,
   * everything being read jumps — the stutter when scrolling up through
   * history. Compensate by the same amount, before paint. Native anchoring
   * is off (overflow-anchor: none) so Chromium-based webviews don't do it
   * twice.
   */
  const itemHeights = new WeakMap<Element, number>();
  const itemObserver = new ResizeObserver((entries) => {
    const el = scrollRef;
    let delta = 0;
    const viewTop = el?.getBoundingClientRect().top ?? 0;
    for (const entry of entries) {
      const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
      const before = itemHeights.get(entry.target);
      itemHeights.set(entry.target, height);
      if (before === undefined || !el || stick()) continue;
      // Only items that were entirely above the viewport. One straddling
      // the top edge (a long reply streaming in) grows below it, where
      // compensating would shove the text being read.
      const growth = height - before;
      if (entry.target.getBoundingClientRect().bottom - growth <= viewTop + 1) delta += growth;
    }
    if (el && delta !== 0) {
      el.scrollTop += delta;
      lastScrollTop = el.scrollTop; // ours, not an upward user scroll
    }
  });
  onCleanup(() => itemObserver.disconnect());

  function trackItem(el: HTMLElement) {
    itemObserver.observe(el);
    onCleanup(() => itemObserver.unobserve(el));
  }

  /*
   * -----------------------------------------------------------------------
   * Sentinels: move the window, load more when it reaches the loaded edge
   * -----------------------------------------------------------------------
   */

  let busy = false;

  /** Within the observer's 300px margin: still wants more after a step. */
  function nearViewport(sentinel: HTMLElement | undefined) {
    const el = scrollRef;
    if (!el || !sentinel) return false;
    const view = el.getBoundingClientRect();
    const rect = sentinel.getBoundingClientRect();
    return rect.bottom >= view.top - 300 && rect.top <= view.bottom + 300;
  }

  async function onTopVisible() {
    if (busy) return;
    busy = true;
    let moved = false;
    try {
      if (win().start === 0 && props.chat?.hasOlder) {
        await props.onLoadOlder(); // the effect above shifts the window
      }
      if (win().start > 0) {
        setStick(false);
        keepAnchor(() => setWin((w) => shiftUp(w)));
        moved = true;
      }
    } finally {
      busy = false;
    }
    // The observer only reports changes: if the sentinel never left the
    // margin (fast scroll, short messages), nothing would load next.
    if (moved) requestAnimationFrame(() => nearViewport(topSentinel) && void onTopVisible());
  }

  async function onBottomVisible() {
    if (busy) return;
    busy = true;
    let moved = false;
    try {
      if (win().end >= messages().length && props.chat?.hasNewer) {
        await props.onLoadNewer();
      }
      if (win().end < messages().length) {
        keepAnchor(() => setWin((w) => shiftDown(w, messages().length)));
        moved = true;
      }
    } finally {
      busy = false;
    }
    if (moved) {
      requestAnimationFrame(
        () => !stick() && nearViewport(bottomSentinel) && void onBottomVisible()
      );
    }
  }

  onMount(() => {
    if (!scrollRef || !topSentinel || !bottomSentinel) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          if (entry.target === topSentinel) void onTopVisible();
          else if (entry.target === bottomSentinel && !stick()) void onBottomVisible();
        }
      },
      { root: scrollRef, rootMargin: '300px 0px' }
    );
    observer.observe(topSentinel);
    observer.observe(bottomSentinel);
    onCleanup(() => observer.disconnect());
  });

  /*
   * -----------------------------------------------------------------------
   * Jump to a search hit
   * -----------------------------------------------------------------------
   */

  createEffect(
    on(
      () => [props.focusMessageId, messages().length] as const,
      ([id]) => {
        if (!id) return;
        const index = messages().findIndex((m) => m.id === id);
        if (index < 0) return;
        setStick(false);
        setWin(clampWindow({ start: index - AROUND, end: index + AROUND + 1 }, messages().length));
        requestAnimationFrame(() => {
          const el = scrollRef?.querySelector<HTMLElement>(
            `.chat-item[data-id="${CSS.escape(id)}"]`
          );
          el?.scrollIntoView({ block: 'center' });
          setFlashId(id);
          window.setTimeout(() => setFlashId((f) => (f === id ? null : f)), FLASH_MS);
        });
      }
    )
  );

  return (
    <div
      ref={scrollRef}
      class="chat-messages"
      onScroll={handleScroll}
      onWheel={handleWheel}
      onTouchMove={markUserInput}
      onKeyDown={markUserInput}
      onPointerDown={handlePointerDown}
    >
      <div ref={topSentinel} class="chat-sentinel" aria-hidden="true" />
      {/* Overlaid, not in the flow: showing and hiding it used to push the
          whole list down and back up by its height on every page load. */}
      <Show when={props.chat?.loadingOlder}>
        <div class="chat-loading-older" role="status">
          <span>Loading earlier messages…</span>
        </div>
      </Show>

      <div ref={innerRef} class="chat-messages-inner">
        <For each={visible()} fallback={<div class="empty-state">Ask the model anything</div>}>
          {(m) => (
            <div
              ref={trackItem}
              class="chat-item"
              classList={{ flash: flashId() === m.id }}
              data-id={m.id}
            >
              <Show when={m.pending && m.content.length === 0 && !m.toolCalls?.length}>
                <div class="chat-message assistant">
                  <span class="chat-pending" classList={{ status: !!m.status }} role="status">
                    {statusText(m)}
                  </span>
                </div>
              </Show>

              <Show when={m.error}>
                <div
                  class="chat-message chat-failure"
                  classList={{ user: m.role === 'user', assistant: m.role === 'assistant' }}
                >
                  <span class="chat-error">
                    {m.meta?.failure ? describeFailure(m.meta.failure, now()) : m.error}
                  </span>
                  <div class="chat-failure-actions">
                    <Show
                      when={
                        m.meta?.failure &&
                        canRetry(m.meta.failure) &&
                        isLatest(m) &&
                        !requestUndelivered()
                      }
                    >
                      <button
                        type="button"
                        class="chat-action-button"
                        onClick={() => props.onRetry()}
                      >
                        Retry
                      </button>
                    </Show>
                    <Show when={m.meta?.failure?.details}>
                      <button
                        type="button"
                        class="link-button"
                        aria-expanded={openDetails() === m.id}
                        onClick={() => setOpenDetails((id) => (id === m.id ? null : m.id))}
                      >
                        {openDetails() === m.id ? 'Hide details' : 'Details'}
                      </button>
                    </Show>
                  </div>
                  <Show when={openDetails() === m.id}>
                    <pre class="chat-failure-details">{m.meta?.failure?.details}</pre>
                  </Show>
                </div>
              </Show>

              {/* The IDE's messages to the model (check failures) show as the
                  verification record instead. */}
              <Show when={!m.error && !isBlank(m.content) && !m.meta?.harness}>
                <div
                  class="chat-message"
                  classList={{
                    user: m.role === 'user',
                    assistant: m.role === 'assistant',
                    failed: !!m.failed,
                  }}
                  title={m.failed ? 'Not delivered: text was put back into the input' : undefined}
                >
                  <MessageContent content={m.content} streaming={!!m.pending} />
                </div>
              </Show>

              <Show when={m.meta?.interrupted}>
                {(interrupted) => (
                  <div class="chat-interrupted">
                    <span>Reply interrupted. {describeFailure(interrupted(), now())}</span>
                    <Show when={isLatest(m)}>
                      <button
                        type="button"
                        class="chat-action-button"
                        onClick={() => props.onContinue()}
                      >
                        Continue
                      </button>
                    </Show>
                  </div>
                )}
              </Show>

              <Show when={m.meta?.paused}>
                {(paused) => (
                  <div
                    class="chat-paused"
                    classList={{ loop: paused().kind === 'loop' }}
                    role="status"
                  >
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                      aria-hidden="true"
                    >
                      <circle cx="8" cy="8" r="6" />
                      <path d="M6.5 5.75v4.5M9.5 5.75v4.5" />
                    </svg>
                    <span class="chat-paused-text">
                      {paused().kind === 'checkpoint'
                        ? `Paused after ${paused().toolCalls} tool calls.`
                        : `Paused after ${paused().toolCalls} tool calls: the agent seems stuck, ${paused().reason}.`}
                    </span>
                    <Show when={isLatest(m)}>
                      <button
                        type="button"
                        class="chat-action-button"
                        onClick={() => props.onContinueRun()}
                      >
                        Continue
                      </button>
                    </Show>
                  </div>
                )}
              </Show>

              <Show when={m.meta?.verification}>
                {(v) => (
                  <VerificationRecord
                    meta={v()}
                    latest={isLatest(m)}
                    live={props.sending && messages()[messages().length - 1]?.id === m.id}
                    onRunChecks={() => props.onRunChecks()}
                    onOpenProjectChecks={() => props.onOpenProjectChecks()}
                  />
                )}
              </Show>

              <Show when={m.toolCalls && m.toolCalls.length > 0}>
                <div class="message-tools">
                  <For each={m.toolCalls!}>
                    {(tc) => (
                      <ToolCallBlock
                        toolCall={tc}
                        result={toolResults().get(tc.id)}
                        running={props.sending}
                        note={props.toolNotes?.[tc.id]}
                      />
                    )}
                  </For>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>

      <div ref={bottomSentinel} class="chat-sentinel" aria-hidden="true" />

      <Show when={!stick() && messages().length > 0}>
        <button
          type="button"
          class="chat-jump-latest"
          onClick={() => void jumpToLatest()}
          title="Scroll to latest"
          aria-label="Scroll to latest message"
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path
              d="M8 3v9M4 8.5 8 12.5l4-4"
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </Show>
    </div>
  );
}
