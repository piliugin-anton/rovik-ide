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
import { requestOpenFile } from '../lib/editorBridge';

const BOTTOM_TOLERANCE_PX = 8;
/** A scroll this soon after wheel/touch/key/scrollbar input is the user's. */
const USER_INPUT_WINDOW_MS = 300;
/**
 * How long the user's last scroll input counts when deciding to follow the
 * newest message again: longer than USER_INPUT_WINDOW_MS, because a smooth
 * scroll (End, Page Down) can take most of a second to reach the bottom.
 */
const FOLLOW_INTENT_MS = 1200;
/**
 * Heading down, this close to the bottom counts as arriving there: the last
 * messages can still be settling at their real height (rendered as they come
 * into view), which moves the bottom while a smooth scroll is on its way to
 * where it was.
 */
const FOLLOW_TOLERANCE_PX = 150;
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
  /** The finished Plan-mode reply that gets "Implement this plan". */
  planMessageId?: string | null;
  onImplementPlan?: () => void;
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
    if (status.since) {
      const elapsed = now() - status.since;
      return elapsed >= 1000 ? `${status.text}… ${formatDuration(elapsed)}` : `${status.text}…`;
    }
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
    // A pending correction no longer matters once the view goes to the newest.
    if (pendingCorrection !== 0) {
      pendingCorrection = 0;
      if (innerRef) innerRef.style.transform = '';
    }
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
    if (again) queueAnchorCorrection(again.getBoundingClientRect().top - top - offset);
    // Messages just inserted are laid out at their placeholder height
    // (content-visibility): the anchoring above used it. Their real height
    // comes a frame later, and the item observer only corrects *changes* to
    // a height it has seen: without this, it saw the real height first and
    // corrected nothing, so every batch of older messages jumped the view.
    for (const item of el.querySelectorAll<HTMLElement>('.chat-item')) {
      if (!itemHeights.has(item)) itemHeights.set(item, item.getBoundingClientRect().height);
    }
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
  /** Which way the user last asked to scroll (wheel, keys); null: can't tell. */
  let userDirection: 'up' | 'down' | null = null;
  /*
   * The list keeping the reader's place (messages above changing height,
   * batches inserted above) by moving the content by `delta`.
   *
   * Never by setting the scroll position while a scroll is animating:
   * WebKitGTK animates wheel scrolling, and a scroll position set during the
   * animation is lost or fought (measured: 100px to thousands off). Nor by
   * holding the correction until later: the content visibly jumps meanwhile
   * (measured: up to 187px), and "input idle" isn't "animation finished".
   *
   * While scrolling (scroll events, wheel or keys in the last 150ms), the
   * correction is a transform on the content: the text stays still at once,
   * and the animation is left alone. Once scrolling has gone quiet, the
   * transform becomes a scroll position change in the same frame, so nothing
   * visibly moves. Otherwise the scroll position is corrected directly.
   */
  let pendingCorrection = 0;
  let lastScrollActivity = 0;
  /*
   * Only WebKit (WebKitGTK on Linux, macOS) needs this: Chromium-based
   * webviews apply a scroll position set during a scroll correctly
   * (measured: no jumps), and there the transform would stop the user from
   * scrolling into a batch just inserted at the top until scrolling paused.
   */
  const fightsScrollCorrections =
    /AppleWebKit/.test(navigator.userAgent) && !/Chrome|Chromium|Edg\//.test(navigator.userAgent);
  let settleTimer: number | undefined;
  const SETTLE_MS = 150;
  const scrollingNow = () => performance.now() - lastScrollActivity < SETTLE_MS;
  const markScrollActivity = () => {
    lastScrollActivity = performance.now();
    if (pendingCorrection !== 0) scheduleSettle();
  };

  function queueAnchorCorrection(delta: number) {
    const el = scrollRef;
    if (!el || Math.abs(delta) < 0.5) return;
    if (fightsScrollCorrections && (scrollingNow() || pendingCorrection !== 0)) {
      pendingCorrection += delta;
      if (innerRef) innerRef.style.transform = `translateY(${-pendingCorrection}px)`;
      scheduleSettle();
      return;
    }
    el.scrollTop += delta;
    lastScrollTop = el.scrollTop;
  }

  function scheduleSettle() {
    window.clearTimeout(settleTimer);
    settleTimer = window.setTimeout(() => {
      if (scrollingNow()) return scheduleSettle();
      requestAnimationFrame(() => {
        const el = scrollRef;
        if (!el || pendingCorrection === 0) return;
        if (scrollingNow()) return scheduleSettle();
        const shift = pendingCorrection;
        pendingCorrection = 0;
        if (innerRef) innerRef.style.transform = '';
        el.scrollTop += shift;
        lastScrollTop = el.scrollTop;
      });
    }, SETTLE_MS);
  }
  onCleanup(() => window.clearTimeout(settleTimer));
  /**
   * Following the newest message resumes only when the user is scrolling
   * down to the bottom themselves (wheel, keys, scrollbar). Not on any scroll
   * that happens to end there: one of the list's own corrections landing at
   * the end while the user scrolled up turned following back on, and the
   * list then kept snapping to the last message.
   */
  const userHeadingDown = () =>
    draggingScrollbar ||
    (performance.now() - lastUserInput < FOLLOW_INTENT_MS && userDirection !== 'up');

  function handleKeyInput(e: KeyboardEvent) {
    markUserInput();
    markScrollActivity();
    if (['ArrowDown', 'PageDown', 'End', ' '].includes(e.key)) userDirection = 'down';
    else if (['ArrowUp', 'PageUp', 'Home'].includes(e.key)) userDirection = 'up';
  }

  // Scrolling keys reach the list even when it isn't focused (browsers send
  // them to the last scrolled area), so they're watched at the window too:
  // when focus is on the page itself or inside the list, not in a text field.
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const inList = !!scrollRef && !!target && scrollRef.contains(target);
      const onPage = !target || target === document.body || target === document.documentElement;
      if (inList || onPage) handleKeyInput(e);
    };
    window.addEventListener('keydown', onKey, true);
    onCleanup(() => window.removeEventListener('keydown', onKey, true));
  });
  const userIsScrolling = () =>
    draggingScrollbar || performance.now() - lastUserInput < USER_INPUT_WINDOW_MS;

  function handleScroll() {
    const el = scrollRef;
    if (!el) return;
    markScrollActivity();
    const top = el.scrollTop;
    const movedUp = top < lastScrollTop - 1;
    lastScrollTop = top;
    const distance = el.scrollHeight - top - el.clientHeight;
    if (!atNewest()) setStick(false);
    else if (distance <= FOLLOW_TOLERANCE_PX && !movedUp && userHeadingDown()) {
      if (!stick()) {
        setStick(true);
        // Close the gap the settling messages left.
        if (distance > BOTTOM_TOLERANCE_PX)
          requestAnimationFrame(() => stick() && scrollToBottom());
      }
    } else if (movedUp && userIsScrolling()) setStick(false);
  }

  function handleWheel(e: WheelEvent) {
    markUserInput();
    // Starts an animated scroll whose first scroll event comes a frame later.
    markScrollActivity();
    if (e.deltaY !== 0) userDirection = e.deltaY > 0 ? 'down' : 'up';
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
   * nears the viewport. When that happens to a message before the one being
   * read, everything being read jumps (the stutter when scrolling up through
   * history). Compensate by the same amount, before paint. Native anchoring
   * is off (overflow-anchor: none) so Chromium-based webviews don't do it
   * twice.
   */
  const itemHeights = new WeakMap<Element, number>();
  /** Real heights by message id, for its placeholder when it's rendered again. */
  const measuredHeights = new Map<string, number>();
  const placeholderHeight = (m: ChatMessage) => measuredHeights.get(m.id) ?? estimatedHeight(m);

  const itemObserver = new ResizeObserver((entries) => {
    const el = scrollRef;
    let delta = 0;
    const viewTop = el?.getBoundingClientRect().top ?? 0;
    // The anchor, as browsers' native scroll anchoring picks it: the first
    // message that starts inside the view. A change in height of anything
    // before it (above the view, across its top edge, or at the very top of
    // it while it renders) moves what's being read, so it's compensated.
    // Never the last message: a reply streaming in grows in the part being
    // read, where compensating would shove the text.
    const items = el ? [...el.querySelectorAll<HTMLElement>('.chat-item')] : [];
    const lastItem = items[items.length - 1];
    const anchorTop =
      items.map((i) => i.getBoundingClientRect().top).find((t) => t >= viewTop - 1) ?? Infinity;
    for (const entry of entries) {
      const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
      const before = itemHeights.get(entry.target);
      itemHeights.set(entry.target, height);
      const item = entry.target as HTMLElement & {
        checkVisibility?: (options?: { contentVisibilityAuto?: boolean }) => boolean;
      };
      // Only real renders: an item skipped by content-visibility reports its
      // placeholder, not its height.
      const skipped = item.checkVisibility
        ? !item.checkVisibility({ contentVisibilityAuto: true })
        : true;
      if (item.dataset.id && !skipped) measuredHeights.set(item.dataset.id, height);
      if (before === undefined || !el || stick() || entry.target === lastItem) continue;
      if (entry.target.getBoundingClientRect().top < anchorTop) delta += height - before;
    }
    if (delta !== 0) queueAnchorCorrection(delta); // ours, not the user's
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
    <div class="chat-messages-frame">
      <div
        ref={scrollRef}
        class="chat-messages"
        onScroll={handleScroll}
        onWheel={handleWheel}
        onTouchMove={() => {
          markUserInput();
          userDirection = null; // a swipe's direction isn't known here
        }}
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
                // The placeholder height before it first renders: close to the
                // real one, so a message rendering late barely moves anything.
                style={{ 'contain-intrinsic-size': `auto ${placeholderHeight(m)}px` }}
              >
                <Show
                  when={
                    m.pending &&
                    m.content.length === 0 &&
                    !m.toolCalls?.length &&
                    !m.meta?.reasoning
                  }
                >
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
                {/* The model's thinking, apart from the reply. */}
                <Show when={m.role === 'assistant' && m.meta?.reasoning?.text}>
                  <ThinkingBlock
                    text={m.meta!.reasoning!.text}
                    ms={m.meta?.reasoning?.ms}
                    thinking={!!m.pending && isBlank(m.content) && !m.toolCalls?.length}
                  />
                </Show>

                <Show when={!m.error && !isBlank(m.content) && !m.meta?.harness}>
                  <div class="chat-message-wrap" classList={{ user: m.role === 'user' }}>
                    <div
                      class="chat-message"
                      classList={{
                        user: m.role === 'user',
                        assistant: m.role === 'assistant',
                        failed: !!m.failed,
                      }}
                      title={
                        m.failed ? 'Not delivered: text was put back into the input' : undefined
                      }
                    >
                      <MessageContent content={m.content} streaming={!!m.pending} />
                    </div>
                    {/* Messages only: tool calls and results have none. */}
                    <Show when={!m.pending}>
                      <CopyMessageButton text={copyableText(m.content)} />
                    </Show>
                  </div>
                </Show>

                {/* The reply stopped arriving but isn't over (see REPLY_IDLE_MS). */}
                <Show when={m.pending && m.status && (m.content.length > 0 || m.toolCalls?.length)}>
                  <span
                    class="chat-pending status chat-working"
                    role="status"
                    title="The model is still working. Some providers send a tool call, such as a file being written, only once it's complete."
                  >
                    {statusText(m)}
                  </span>
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

                <Show when={props.planMessageId === m.id}>
                  <div class="chat-plan-actions">
                    <button
                      type="button"
                      class="chat-action-button primary"
                      onClick={() => props.onImplementPlan?.()}
                      title="Switch to Agent mode and carry out this plan"
                    >
                      Implement this plan
                    </button>
                    <span class="chat-plan-hint">or reply to change it</span>
                  </div>
                </Show>
                <Show when={m.meta?.planFile}>
                  {(file) => (
                    <div class="chat-plan-hint chat-plan-file">
                      Saved as{' '}
                      <button
                        type="button"
                        class="link-button"
                        title={`${file()}: edit it, then implement it from the editor or here`}
                        onClick={() => requestOpenFile({ path: file() })}
                      >
                        {file().split(/[\\/]/).slice(-3).join('/')}
                      </button>
                      , where you can edit it before implementing.
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
      </div>

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

/**
 * What "copy" gives for a message: its text as written (Markdown and code
 * blocks included), with pasted text in full and without the
 * <pasted_text> markers the model gets.
 */
export function copyableText(content: string): string {
  return content
    .replace(/<pasted_text(?:\s+lines="\d+")?>\n?([\s\S]*?)\n?<\/pasted_text>/g, '$1')
    .trim();
}

async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback for webviews without the async clipboard API.
    const area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.append(area);
    area.select();
    const ok = document.execCommand('copy');
    area.remove();
    return ok;
  }
}

/** Under a message, shown on hover: copies the whole message. */
function CopyMessageButton(props: { text: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: number | undefined;
  onCleanup(() => window.clearTimeout(timer));
  return (
    <button
      type="button"
      class="chat-copy-button"
      classList={{ copied: copied() }}
      title={copied() ? 'Copied' : 'Copy message'}
      aria-label={copied() ? 'Copied' : 'Copy message'}
      onClick={async () => {
        if (!(await writeClipboard(props.text))) return;
        setCopied(true);
        window.clearTimeout(timer);
        timer = window.setTimeout(() => setCopied(false), 1500);
      }}
    >
      <Show
        when={copied()}
        fallback={
          <svg
            width="13"
            height="13"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
            <path d="M10.5 5.5V3.5a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2" />
          </svg>
        }
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M3.5 8.5 6.5 11.5 12.5 4.5" />
        </svg>
      </Show>
    </button>
  );
}

/**
 * The model's reasoning: "Thinking…" with its latest line while it thinks,
 * "Thought for 12 s" after; opens to the whole text (plain, it can be long).
 */
function ThinkingBlock(props: { text: string; ms?: number; thinking: boolean }) {
  const [open, setOpen] = createSignal(false);
  const seconds = () => (props.ms ? Math.max(1, Math.round(props.ms / 1000)) : null);
  const lastLine = () => {
    const lines = props.text.trimEnd().split('\n');
    const line = lines[lines.length - 1] ?? '';
    return line.length > 160 ? `…${line.slice(-160)}` : line;
  };
  return (
    <div class="chat-thinking" classList={{ active: props.thinking }}>
      <button
        type="button"
        class="chat-thinking-toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open()}
      >
        <svg
          class="chat-pasted-caret"
          classList={{ open: open() }}
          width="10"
          height="10"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M6 4l4 4-4 4" />
        </svg>
        {props.thinking ? 'Thinking…' : seconds() ? `Thought for ${seconds()} s` : 'Thought'}
      </button>
      <Show when={props.thinking && !open()}>
        <div class="chat-thinking-preview">{lastLine()}</div>
      </Show>
      <Show when={open()}>
        <div class="chat-thinking-text">{props.text}</div>
      </Show>
    </div>
  );
}

/**
 * Roughly how tall a message renders, for its placeholder before it's drawn
 * (content-visibility). A fixed 120px was far off for short user messages
 * (about 56px) and long replies alike, and the difference moved the view
 * whenever one rendered late. It only needs to be close, not exact.
 */
function estimatedHeight(m: ChatMessage): number {
  const LINE = 20;
  const CHARS_PER_LINE = 52;
  const lines = (text: string) =>
    text
      .split('\n')
      .reduce((n, line) => n + Math.max(1, Math.ceil(line.length / CHARS_PER_LINE)), 0);
  if (m.role === 'user') return 32 + LINE * lines(m.content) + 24;
  let height = 0;
  if (m.content.trim()) {
    const fences = (m.content.match(/```/g) ?? []).length / 2;
    height += 24 + LINE * lines(m.content) + fences * 40 + 24;
  }
  height += (m.toolCalls?.length ?? 0) * 40;
  return Math.max(40, Math.round(height));
}
