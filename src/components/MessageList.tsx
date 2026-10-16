// MessageList.tsx
//
// Renders an open chat's messages through a window of at most WINDOW_MAX
// (lib/messageWindow.ts). Sentinels at the top and bottom move the window,
// loading older or newer pages from the database when the loaded messages
// run out. Scroll position is kept by hand when content changes above the
// viewport (native scroll anchoring isn't reliable across WebKit versions).
import {
  createComputed,
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
import type { ChatMessage, ChatToolCall } from '../types';
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
import { formatBytes, isBlank, plural } from '../lib/textUtils';
import { attachmentUrl } from '../lib/chatApi';
import type { ComposerPdf } from '../lib/composerAttachments';
import { copySegments } from '../lib/composerClipboard';
import { segmentsOfMessage, type DraftSegment } from '../lib/composerSegments';
import { canRetry, describeFailure, formatDuration } from '../lib/chatErrors';
import { pauseText } from '../lib/runPause';
import MessageContent from './MessageContent';
import ToolCallBlock from './ToolCallBlock';
import ToolCallGroup from './ToolCallGroup';
import VerificationRecord from './VerificationRecord';
import ReviewRecord from './ReviewRecord';
import { liveSubagent } from '../lib/subagents';
import { indexToolResults } from '../lib/toolResults';
import type { ReviewMeta } from '../lib/changeReview';
import { openPlan } from '../lib/plans';
import { isSettled, parseTodo } from '../lib/todo';
import { groupToolCalls, type ToolRun } from '../lib/toolGroups';

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
 * Heading for the end (End key), this close to the bottom counts as arriving
 * there: the last messages can still be settling at their real height
 * (rendered as they come into view), which moves the bottom while a smooth
 * scroll is on its way to where it was. Any other scrolling has to reach the
 * bottom itself (BOTTOM_TOLERANCE_PX): stopping a little short of it is the
 * user's choice, and snapping the rest of the way there fought them.
 */
const FOLLOW_TOLERANCE_PX = 150;
const AROUND = 25;

/**
 * Where the reader was in a chat when they left it (for another chat, or
 * another sidebar tab) while not following the newest message: its first
 * visible message and how far that was from the top. Coming back puts
 * them there again, instead of at the end of the chat.
 */
const readingPlaces = new Map<string, { id: string; offset: number }>();
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
  /** The chat can't change now (it's moving): no Retry, Continue or Implement. */
  readOnly?: boolean;
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
    return !props.sending && !props.readOnly && list[list.length - 1]?.id === m.id;
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
    // Sent, nothing back yet: not "Thinking", the model hasn't started.
    if (!status) return 'Roviking…';
    if (status.since) {
      const elapsed = now() - status.since;
      return elapsed >= 1000 ? `${status.text}… ${formatDuration(elapsed)}` : `${status.text}…`;
    }
    if (!status.until) return status.text;
    const left = status.until - now();
    return left > 500 ? `${status.text} in ${formatDuration(left)}…` : `${status.text}…`;
  };

  /** Each call's result, by the call itself (P5.8: ids can repeat). */
  const toolIndex = createMemo(() => indexToolResults(messages()));
  const resultOf = (tc: ChatToolCall) => toolIndex().results.get(tc);
  /** Live progress (keyed by call id) is the newest call's with that id. */
  const newestOfId = (tc: ChatToolCall) => toolIndex().latest.get(tc.id) === tc;

  const runningPlanReview: ReviewMeta = {
    kind: 'plan',
    status: 'running',
    round: 0,
    maxRounds: 2,
  };
  /**
   * A submit_plan call's review (B10): from its result, or running while
   * the reviewer works.
   */
  const planReviewOf = (tc: ChatToolCall): ReviewMeta | undefined => {
    const result = resultOf(tc);
    if (result) return result.meta?.review;
    return newestOfId(tc) && liveSubagent(props.dialogId, tc.id) ? runningPlanReview : undefined;
  };
  /** P5.1: the calls themselves (stable), so each record keeps its state. */
  const planCalls = (m: ChatMessage) =>
    (m.toolCalls ?? []).filter((tc) => tc.name === 'submit_plan');

  // Checklist snapshots (step 10): every `todo` call writes the whole list,
  // so only the latest call in the chat is the current state. Earlier ones
  // are superseded: dimmed, labelled "earlier update", never spinning — an
  // old snapshot's "in progress" step must not read as work happening now.
  // The latest snapshot with no run going and steps unfinished is
  // interrupted: its "in progress" icon becomes paused.
  const todoCallStates = createMemo(() => {
    const ids: string[] = [];
    for (const m of messages()) {
      for (const tc of m.toolCalls ?? []) {
        if (tc.name === 'todo') ids.push(tc.id);
      }
    }
    const latest = ids.length ? ids[ids.length - 1] : null;
    const superseded = new Set(ids.slice(0, -1));
    let interruptedLatest = false;
    if (latest && !props.sending) {
      outer: for (let i = messages().length - 1; i >= 0; i--) {
        for (const tc of messages()[i].toolCalls ?? []) {
          if (tc.id === latest && tc.name === 'todo') {
            // Dismissed by the user: closed, not interrupted.
            if (messages()[i].meta?.checklistClosed) break outer;
            try {
              const items = parseTodo(JSON.parse(tc.arguments));
              interruptedLatest = items.length > 0 && items.some((item) => !isSettled(item));
            } catch {
              interruptedLatest = false;
            }
            break outer;
          }
        }
      }
    }
    return { superseded, latest, interruptedLatest };
  });

  /*
   * Tool calls show as one line each (ToolCallGroup), and consecutive calls
   * to one tool fold into one line, also across replies: a reply that is
   * nothing but tool calls continues the tool calls of the reply before it,
   * and renders inside it rather than as a row of its own. Anything else shown with a reply (text, thinking, an
   * error, a pause) starts a new row, so nothing is moved out of order.
   */
  const onlyToolCalls = (m: ChatMessage) =>
    m.role === 'assistant' &&
    !!m.toolCalls?.length &&
    isBlank(m.content) &&
    !m.error &&
    !m.meta?.reasoning?.text &&
    !hasPlanSubmit(m) &&
    !(m.pending && m.status);
  /** A submit_plan call: its plan review shows after the reply's calls (B10). */
  const hasPlanSubmit = (m: ChatMessage) => !!m.toolCalls?.some((tc) => tc.name === 'submit_plan');
  /** Nothing shown after its tool calls, so the next reply's can follow on. */
  const toolCallsLast = (m: ChatMessage) =>
    !m.meta?.interrupted &&
    !m.meta?.paused &&
    !m.meta?.verification &&
    !m.meta?.review &&
    !m.meta?.planFile &&
    !hasPlanSubmit(m) &&
    props.planMessageId !== m.id &&
    !(m.pending && m.status);

  const toolRows = createMemo(() => {
    const { start, end } = win();
    /** Row message id -> its tool calls, with the replies folded into it. */
    const calls = new Map<string, ChatToolCall[]>();
    const folded = new Set<string>();
    let open: ChatToolCall[] | null = null;
    for (const m of messages().slice(start, end)) {
      if (m.role === 'tool') continue;
      if (open && onlyToolCalls(m)) {
        open.push(...m.toolCalls!);
        folded.add(m.id);
      } else {
        open = m.toolCalls?.length ? [...m.toolCalls] : null;
        if (open) calls.set(m.id, open);
      }
      if (!toolCallsLast(m)) open = null;
    }
    const runs = new Map<string, ToolRun[]>();
    for (const [id, list] of calls) runs.set(id, groupToolCalls(list));
    return { runs, folded };
  });

  // Tool results render inside their call's block, not as rows of their own.
  const visible = createMemo(() => {
    const { start, end } = win();
    const { folded } = toolRows();
    return messages()
      .slice(start, end)
      .filter((m) => m.role !== 'tool' && !folded.has(m.id));
  });

  /** A row's tool call runs, by the id of each run's first call (stable keys for <For>). */
  const runsOf = (m: ChatMessage) => toolRows().runs.get(m.id) ?? [];
  const runKeys = (m: ChatMessage) => runsOf(m).map((r) => r.calls[0].id);
  const runAt = (m: ChatMessage, key: string) => runsOf(m).find((r) => r.calls[0].id === key);

  const renderCall = (tc: ChatToolCall, initiallyExpanded = false) => (
    <ToolCallBlock
      initiallyExpanded={initiallyExpanded}
      dialogId={props.dialogId}
      toolCall={tc}
      result={resultOf(tc)}
      notLive={!newestOfId(tc)}
      running={props.sending}
      note={props.toolNotes?.[tc.id]}
      superseded={todoCallStates().superseded.has(tc.id)}
      interrupted={tc.id === todoCallStates().latest && todoCallStates().interruptedLatest}
    />
  );

  const atNewest = () => win().end >= messages().length && !props.chat?.hasNewer;

  /*
   * -----------------------------------------------------------------------
   * Scrolling
   * -----------------------------------------------------------------------
   */

  /*
   * A press on a message holds the view still until it's released.
   * Following a reply as it streamed moved what was under the pointer
   * between press and release, so the click (on a link, a file name, a
   * group's header) was lost or landed on the line below, and a drag to
   * select text ran off. Following goes on once the button is up.
   */
  let held = false;
  const following = () => stick() && !held;

  function hold() {
    if (held) return;
    held = true;
    const release = () => {
      held = false;
      window.removeEventListener('mouseup', release, true);
      window.removeEventListener('blur', release);
      window.removeEventListener('dragend', release, true);
      // After the click this release makes, which is dispatched first.
      requestAnimationFrame(() => !topLock && following() && scrollToBottom());
    };
    window.addEventListener('mouseup', release, true);
    window.addEventListener('blur', release);
    window.addEventListener('dragend', release, true);
  }

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
  /** The user's last scroll input asked for the very end (End key). */
  let userHeadingToEnd = false;
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
    // The top lock owns every compensation while it is held.
    if (topLock) return;
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

  /*
   * -----------------------------------------------------------------------
   * Top lock: scrolling is frozen while older messages load.
   * -----------------------------------------------------------------------
   *
   * Scrolling up through history jumped because three things raced: the
   * database fetch resolving (messages prepended) at one moment, the window
   * shifting (more rows rendered above) at another, and the user's own wheel
   * scroll animating underneath both (on WebKitGTK a scrollTop set mid
   * animation is lost or fought). Patching each jump after the fact never
   * converged, so the top path no longer patches: while older messages load
   * the list can't scroll at all, and the viewport is re-anchored instead.
   *
   * While locked:
   * - the container gets .scroll-lock (overflow-y: hidden, touch-action:
   *   none): user scrolling stops, programmatic scrollTop still works, and
   *   any in-flight wheel animation is cancelled rather than fought;
   * - wheel/touch/scroll-key inputs are swallowed in the capture phase;
   * - every scroll event is clamped back to the locked position, so queued
   *   events from before the lock can't move the view either;
   * - the first visible message (id + offset from the container top) is the
   *   anchor: after each change above the viewport scrollTop is moved by how
   *   far the anchor moved, which — unlike a scrollHeight delta — ignores
   *   rows trimmed below the viewport by the window;
   * - the item observer and the follow logic stay quiet; the lock's own
   *   settle loop owns every compensation until heights stop changing
   *   (content-visibility placeholders resolving, code colorizing), then the
   *   lock lifts. A generation counter aborts a settle left behind by an
   *   explicit jump-to-latest, and try/finally always unlocks.
   */
  interface TopAnchor {
    id: string;
    offset: number;
    top: number;
    height: number;
  }
  let topLock: TopAnchor | null = null;
  let topLockGen = 0;
  let adjustingScroll = false;
  const [topLocked, setTopLocked] = createSignal(false);

  const nextFrame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

  function captureTopAnchor(): TopAnchor | null {
    const el = scrollRef;
    if (!el) return null;
    const top = el.getBoundingClientRect().top;
    for (const item of el.querySelectorAll<HTMLElement>('.chat-item')) {
      const rect = item.getBoundingClientRect();
      if (rect.bottom > top + 1) {
        return {
          id: item.dataset.id ?? '',
          offset: rect.top - top,
          top: el.scrollTop,
          height: el.scrollHeight,
        };
      }
    }
    return { id: '', offset: 0, top: el.scrollTop, height: el.scrollHeight };
  }

  /** Moves scrollTop so the locked anchor sits where it sat, before paint. */
  function compensateTopAnchor() {
    const el = scrollRef;
    if (!el || !topLock) return;
    if (topLock.id) {
      const anchor = el.querySelector<HTMLElement>(
        `.chat-item[data-id="${CSS.escape(topLock.id)}"]`
      );
      if (anchor) {
        const delta =
          anchor.getBoundingClientRect().top - el.getBoundingClientRect().top - topLock.offset;
        topLock.height = el.scrollHeight;
        if (Math.abs(delta) < 0.5) {
          topLock.top = el.scrollTop;
          lastScrollTop = el.scrollTop;
          return;
        }
        adjustingScroll = true;
        el.scrollTop += delta;
        adjustingScroll = false;
        topLock.top = el.scrollTop;
        lastScrollTop = el.scrollTop;
        return;
      }
    }
    // The anchor isn't rendered (window jumped past it): fall back to keeping
    // the distance from the bottom edge of the added content.
    const growth = el.scrollHeight - topLock.height;
    topLock.height = el.scrollHeight;
    if (Math.abs(growth) < 0.5) {
      topLock.top = el.scrollTop;
      lastScrollTop = el.scrollTop;
      return;
    }
    adjustingScroll = true;
    el.scrollTop = topLock.top + growth;
    adjustingScroll = false;
    topLock.top = el.scrollTop;
    lastScrollTop = el.scrollTop;
  }

  function blockScrollInput(e: Event) {
    if (topLock) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  function blockScrollKey(e: KeyboardEvent) {
    if (!topLock) return;
    if (!['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' '].includes(e.key)) return;
    const target = e.target as HTMLElement | null;
    const inList = !!scrollRef && !!target && scrollRef.contains(target);
    const onPage = !target || target === document.body || target === document.documentElement;
    if (inList || onPage) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  function lockTopScroll() {
    const el = scrollRef;
    // Fold a pending WebKit transform correction into the scroll position so
    // the lock starts from the exact visual position.
    if (pendingCorrection !== 0 && el) {
      const shift = pendingCorrection;
      pendingCorrection = 0;
      if (innerRef) innerRef.style.transform = '';
      el.scrollTop += shift;
    }
    window.clearTimeout(settleTimer);
    topLockGen += 1;
    topLock = captureTopAnchor();
    setTopLocked(true);
    el?.classList.add('scroll-lock');
    el?.addEventListener('wheel', blockScrollInput, { capture: true, passive: false });
    el?.addEventListener('touchmove', blockScrollInput, { capture: true, passive: false });
    window.addEventListener('keydown', blockScrollKey, true);
  }

  function unlockTopScroll() {
    const el = scrollRef;
    el?.classList.remove('scroll-lock');
    el?.removeEventListener('wheel', blockScrollInput, { capture: true });
    el?.removeEventListener('touchmove', blockScrollInput, { capture: true });
    window.removeEventListener('keydown', blockScrollKey, true);
    topLockGen += 1;
    topLock = null;
    setTopLocked(false);
    if (el) lastScrollTop = el.scrollTop;
  }

  /**
   * Holds the viewport on its anchor until inserted rows stop changing height
   * (placeholders resolving to real heights, code colorizing). Every
   * compensation runs inside the rAF callback, before paint, so a wrong
   * height is never shown for a frame.
   */
  async function settleTopLock(maxMs = 1200, quietMs = 200) {
    const el = scrollRef;
    if (!el || !topLock) return;
    const gen = topLockGen;
    const frameWithAnchor = () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => {
          if (topLock && gen === topLockGen) compensateTopAnchor();
          resolve();
        })
      );
    await frameWithAnchor();
    if (!topLock || gen !== topLockGen) return;
    await frameWithAnchor();
    if (!topLock || gen !== topLockGen) return;
    const start = performance.now();
    let lastChange = performance.now();
    let lastHeight = el.scrollHeight;
    while (performance.now() - start < maxMs) {
      await frameWithAnchor();
      if (!topLock || gen !== topLockGen) return;
      if (Math.abs(el.scrollHeight - lastHeight) >= 1) {
        lastHeight = el.scrollHeight;
        lastChange = performance.now();
      } else if (performance.now() - lastChange >= quietMs) break;
    }
    if (topLock && gen === topLockGen) compensateTopAnchor();
  }
  // Never leave the page with swallowed inputs if the panel unmounts mid-load.
  onCleanup(() => {
    if (topLock) unlockTopScroll();
  });
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
    // Frozen while older messages load: the capture-phase blocker already
    // swallowed scroll keys; don't let them mark input or set a direction.
    if (topLock) return;
    markUserInput();
    markScrollActivity();
    userHeadingToEnd = e.key === 'End';
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
    if (topLock) {
      // Frozen: clamp back anything that moved (a queued event from before
      // the lock, a programmatic scroll). Our own compensations update
      // topLock.top first, so they pass through untouched.
      if (!adjustingScroll && Math.abs(el.scrollTop - topLock.top) >= 1) {
        adjustingScroll = true;
        el.scrollTop = topLock.top;
        adjustingScroll = false;
      }
      return;
    }
    markScrollActivity();
    const top = el.scrollTop;
    const movedUp = top < lastScrollTop - 1;
    lastScrollTop = top;
    const distance = el.scrollHeight - top - el.clientHeight;
    const tolerance = userHeadingToEnd ? FOLLOW_TOLERANCE_PX : BOTTOM_TOLERANCE_PX;
    if (!atNewest()) setStick(false);
    else if (distance <= tolerance && !movedUp && userHeadingDown()) {
      if (!stick()) {
        setStick(true);
        // Close the gap the settling messages left.
        if (distance > BOTTOM_TOLERANCE_PX)
          requestAnimationFrame(() => stick() && scrollToBottom());
      }
    } else if (movedUp && userIsScrolling()) setStick(false);
  }

  function handleWheel(e: WheelEvent) {
    // Frozen while older messages load: the capture-phase blocker already
    // swallowed this input; don't let it change follow state or direction.
    if (topLock) return;
    markUserInput();
    // Starts an animated scroll whose first scroll event comes a frame later.
    markScrollActivity();
    userHeadingToEnd = false;
    if (e.deltaY !== 0) userDirection = e.deltaY > 0 ? 'down' : 'up';
    // Upward intent ends following at once, even if a scroll-to-bottom is
    // queued for this frame (it checks stick() before scrolling).
    const el = scrollRef;
    if (e.deltaY < 0 && el && el.scrollHeight > el.clientHeight) setStick(false);
  }

  function handlePointerDown(e: PointerEvent) {
    // Frozen while older messages load: no scrollbar to drag (it's hidden).
    if (topLock) return;
    // Only the container itself: a press on its scrollbar, not on content.
    if (e.target !== scrollRef) return;
    draggingScrollbar = true;
    userHeadingToEnd = false;
    const stop = () => {
      draggingScrollbar = false;
      markUserInput();
      window.removeEventListener('pointerup', stop);
      window.removeEventListener('pointercancel', stop);
    };
    window.addEventListener('pointerup', stop);
    window.addEventListener('pointercancel', stop);
  }

  /*
   * Clearing a selection of message text. WebKit doesn't always drop one on
   * a click: a press inside it waits to start a drag, and a press between
   * messages (the list's gaps and padding) leaves it be, so a drag that ran
   * on into other messages couldn't be undone. A click (no drag, not a
   * double click) on the list now clears it, and so does Escape.
   */
  const CLICK_SLOP_PX = 4;
  let clickStart: { x: number; y: number; hadSelection: boolean } | null = null;

  function listSelection(): Selection | null {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || sel.rangeCount === 0 || !scrollRef) return null;
    return scrollRef.contains(sel.getRangeAt(0).commonAncestorContainer) ||
      (sel.anchorNode && scrollRef.contains(sel.anchorNode)) ||
      (sel.focusNode && scrollRef.contains(sel.focusNode))
      ? sel
      : null;
  }

  function onScrollbar(e: MouseEvent) {
    const el = scrollRef;
    if (!el || e.target !== el) return false;
    const rect = el.getBoundingClientRect();
    return e.clientX - rect.left >= el.clientWidth || e.clientY - rect.top >= el.clientHeight;
  }

  function handleSelectMouseDown(e: MouseEvent) {
    if (e.button === 0 && e.target !== scrollRef) hold();
    clickStart =
      e.button === 0 && e.detail <= 1 && !e.shiftKey && !onScrollbar(e)
        ? { x: e.clientX, y: e.clientY, hadSelection: !!listSelection() }
        : null;
  }

  function handleSelectMouseUp(e: MouseEvent) {
    const start = clickStart;
    clickStart = null;
    if (!start?.hadSelection || e.button !== 0 || e.detail > 1) return;
    if (Math.hypot(e.clientX - start.x, e.clientY - start.y) > CLICK_SLOP_PX) return;
    // After the browser's own handling of the click, in case it did clear it.
    requestAnimationFrame(() => listSelection()?.removeAllRanges());
  }

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      const sel = listSelection();
      if (!sel) return;
      sel.removeAllRanges();
      e.preventDefault();
    };
    window.addEventListener('keydown', onKey);
    onCleanup(() => window.removeEventListener('keydown', onKey));
  });

  async function jumpToLatest() {
    // An explicit jump outranks a history load in flight: drop the lock so
    // its settle loop aborts (generation check) instead of dragging the view
    // back to the anchor it was holding.
    if (topLock) unlockTopScroll();
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
          // A history load from the previous chat must not hold the new one.
          if (topLock) unlockTopScroll();
          const place = dialogId ? readingPlaces.get(dialogId) : undefined;
          if (dialogId) readingPlaces.delete(dialogId);
          const at = place ? messages().findIndex((m) => m.id === place.id) : -1;
          if (place && at >= 0) {
            setWin(clampWindow({ start: at - AROUND, end: at + AROUND + 1 }, length));
            setStick(false);
            requestAnimationFrame(() => returnTo(place));
          } else {
            setWin(initialWindow(length));
            setStick(true);
            requestAnimationFrame(scrollToBottom);
          }
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

  /** Back where the reader was when the list last went away. */
  function returnTo(place: { id: string; offset: number }) {
    const el = scrollRef;
    const item = el?.querySelector<HTMLElement>(`.chat-item[data-id="${CSS.escape(place.id)}"]`);
    if (!el || !item) return;
    el.scrollTop +=
      item.getBoundingClientRect().top - el.getBoundingClientRect().top - place.offset;
    lastScrollTop = el.scrollTop;
  }

  function rememberPlace(dialogId: string) {
    if (stick()) {
      readingPlaces.delete(dialogId);
      return;
    }
    const anchor = captureTopAnchor();
    if (anchor?.id) readingPlaces.set(dialogId, { id: anchor.id, offset: anchor.offset });
  }

  // Leaving for another chat: a computation, which runs before the list
  // shows the other chat, while the one left is still there to measure.
  createComputed(
    on(
      () => props.dialogId,
      (_, left) => {
        if (left) rememberPlace(left);
      },
      { defer: true }
    )
  );

  onCleanup(() => {
    if (prevDialog) rememberPlace(prevDialog);
  });

  // While following, stay at the bottom as the last message grows.
  createEffect(
    on(
      () => {
        const list = messages();
        const last = list[list.length - 1];
        return [list.length, last?.content.length, last?.toolCalls?.length];
      },
      () => {
        if (following()) requestAnimationFrame(() => following() && scrollToBottom());
      }
    )
  );

  // Content that resizes after rendering (code colorized, images loaded).
  onMount(() => {
    if (!innerRef) return;
    let lastHeight = innerRef.offsetHeight;
    const observer = new ResizeObserver(() => {
      const height = innerRef?.offsetHeight ?? 0;
      const shrank = height < lastHeight;
      lastHeight = height;
      // The top lock owns the scroll position while held; its settle loop
      // compensates every height change, so stay out of its way (but keep
      // lastHeight fresh, so unlocking never sees a stale "shrank").
      if (topLock) return;
      if (stick()) {
        if (!held) scrollToBottom();
        return;
      }
      // Content shrinking under the view (a tool result collapsed) can leave
      // it at the very end: nothing is below any more, so follow again, or
      // "Scroll to latest" stays up with nothing to scroll to.
      const el = scrollRef;
      if (!el || !shrank || !atNewest() || pendingCorrection !== 0) return;
      if (el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_TOLERANCE_PX) {
        lastScrollTop = el.scrollTop;
        setStick(true);
      }
    });
    observer.observe(innerRef);
    onCleanup(() => observer.disconnect());
  });

  // The view itself resizing: a bar opening under the list (a landing's
  // Undo, the checklist, a question) shrinks it from below, and the newest
  // message slid out of sight while following.
  onMount(() => {
    const el = scrollRef;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      // Next frame: scrolling inside the callback renders more items, whose
      // resizing the observers would only hear of in the frame after.
      if (!topLock && following())
        requestAnimationFrame(() => !topLock && following() && scrollToBottom());
    });
    observer.observe(el);
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
      // While the top lock is held its settle loop owns every compensation;
      // still record heights above so placeholders stay accurate.
      if (before === undefined || !el || stick() || topLock || entry.target === lastItem) continue;
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
    if (busy || topLock) return;
    if (!(win().start > 0 || (win().start === 0 && props.chat?.hasOlder))) return;
    busy = true;
    lockTopScroll();
    let moved = false;
    try {
      if (win().start === 0 && props.chat?.hasOlder) {
        await props.onLoadOlder();
        // The prepend (and the window shift the bookkeeping effect applies
        // for it) lands across microtasks and frames: hold the anchor over
        // two frames so both are compensated before the settle loop decides
        // heights are quiet.
        await nextFrame();
        if (topLock) compensateTopAnchor();
        await nextFrame();
        if (topLock) compensateTopAnchor();
      }
      if (win().start > 0) {
        setStick(false);
        setWin((w) => shiftUp(w));
        // Synchronous first compensation: Solid reconciles the <For> in the
        // same task, so a forced layout here sees the new rows and no wrong
        // frame is ever painted.
        if (topLock) compensateTopAnchor();
        moved = true;
      }
      await settleTopLock();
    } finally {
      unlockTopScroll();
      busy = false;
    }
    // The observer only reports changes: if the sentinel never left the
    // margin (fast scroll, short messages), nothing would load next.
    if (moved) requestAnimationFrame(() => nearViewport(topSentinel) && void onTopVisible());
  }

  async function onBottomVisible() {
    if (busy || topLock) return;
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
          // A history load in flight owns the scroll position; its lock
          // clamps this back, so don't fight it — the highlight still shows.
          if (topLock) {
            setFlashId(id);
            window.setTimeout(() => setFlashId((f) => (f === id ? null : f)), FLASH_MS);
            return;
          }
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
        classList={{ 'scroll-lock': topLocked() }}
        onScroll={handleScroll}
        onWheel={handleWheel}
        onTouchMove={() => {
          if (topLock) return;
          markUserInput();
          userDirection = null; // a swipe's direction isn't known here
        }}
        onKeyDown={() => {
          if (topLock) return;
          markUserInput();
        }}
        onPointerDown={handlePointerDown}
        onMouseDown={handleSelectMouseDown}
        onMouseUp={handleSelectMouseUp}
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
                  <div class="tool-group active chat-waiting" role="status">
                    <div class="tool-group-header">
                      <span class="tool-call-spinner" aria-hidden="true" />
                      <span class="tool-group-verb">{statusText(m)}</span>
                    </div>
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
                  {(text) => (
                    <ThinkingBlock
                      text={text()}
                      ms={m.meta?.reasoning?.ms}
                      thinking={!!m.pending && isBlank(m.content) && !m.toolCalls?.length}
                    />
                  )}
                </Show>

                <Show
                  when={!m.error && (!isBlank(m.content) || hasAttachments(m)) && !m.meta?.harness}
                >
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
                      <Show when={!isBlank(m.content)}>
                        <MessageContent content={m.content} streaming={!!m.pending} />
                      </Show>
                      {/* Images attached to the message: a click shows one whole. */}
                      <Show when={userImages(m).length}>
                        <div class="chat-message-images">
                          <For each={userImages(m)}>
                            {(id) => (
                              <img
                                class="chat-message-image"
                                src={attachmentUrl(id)}
                                alt="Attached image"
                                title="Click to enlarge"
                                onClick={(e) => {
                                  const expanded = e.currentTarget.classList.toggle('expanded');
                                  e.currentTarget.title = expanded
                                    ? 'Click to shrink'
                                    : 'Click to enlarge';
                                }}
                              />
                            )}
                          </For>
                        </div>
                      </Show>
                      <Show when={userPdfs(m).length}>
                        <div class="chat-message-files">
                          <For each={userPdfs(m)}>
                            {(pdf) => (
                              <span
                                class="chat-message-file"
                                title={`${pdf.name}: PDF, ${plural(pdf.pages, 'page')}, ${formatBytes(pdf.size)}`}
                              >
                                <svg
                                  class="chat-attachment-icon"
                                  width="12"
                                  height="12"
                                  viewBox="0 0 16 16"
                                  fill="none"
                                  stroke="currentColor"
                                  stroke-width="1.4"
                                  stroke-linejoin="round"
                                  aria-hidden="true"
                                >
                                  <path d="M3.5 1.5h6l3 3v10h-9z" />
                                  <path d="M9.5 1.5v3h3" />
                                </svg>
                                <span class="chat-message-file-name">{pdf.name}</span>
                                <span class="chat-message-file-size">
                                  {plural(pdf.pages, 'page')}
                                </span>
                              </span>
                            )}
                          </For>
                        </div>
                      </Show>
                    </div>
                    {/* Messages only: tool calls and results have none. */}
                    <Show when={!m.pending && (!isBlank(m.content) || hasAttachments(m))}>
                      <CopyMessageButton segments={segmentsOfMessage(m)} />
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
                      <span class="chat-paused-text">{pauseText(paused())}</span>
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

                <Show when={props.planMessageId === m.id && !props.readOnly}>
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
                        onClick={() => void openPlan(props.dialogId, file())}
                      >
                        {file().split(/[\\/]/).slice(-3).join('/')}
                      </button>
                      , where you can edit it before implementing.
                    </div>
                  )}
                </Show>

                <Show when={m.meta?.review}>
                  {(r) => (
                    <ReviewRecord
                      meta={r()}
                      dialogId={props.dialogId}
                      recordId={m.id}
                      live={props.sending}
                    />
                  )}
                </Show>

                <Show when={m.meta?.planCheck}>
                  {(c) => (
                    <div class="chat-verify flagged" role="status">
                      <div class="chat-verify-head">
                        <span class="chat-verify-title">
                          The plan in {c().file} has problems as it is now; the agent was told
                          before it started:
                        </span>
                      </div>
                      <ul class="chat-verify-flags">
                        <For each={c().problems}>{(p) => <li>{p}</li>}</For>
                      </ul>
                    </div>
                  )}
                </Show>

                <For each={planCalls(m)}>
                  {(tc) => (
                    <Show when={planReviewOf(tc)}>
                      {(meta) => (
                        <ReviewRecord
                          meta={meta()}
                          dialogId={props.dialogId}
                          recordId={tc.id}
                          live={props.sending && newestOfId(tc)}
                        />
                      )}
                    </Show>
                  )}
                </For>

                <Show when={m.meta?.verification}>
                  {(v) => (
                    <VerificationRecord
                      meta={v()}
                      dialogId={props.dialogId}
                      recordId={m.id}
                      latest={isLatest(m)}
                      live={props.sending && messages()[messages().length - 1]?.id === m.id}
                      onRunChecks={() => props.onRunChecks()}
                      onOpenProjectChecks={() => props.onOpenProjectChecks()}
                    />
                  )}
                </Show>

                <Show when={runKeys(m).length > 0}>
                  <div class="message-tools">
                    <For each={runKeys(m)}>
                      {(key) => {
                        const run = () => runAt(m, key);
                        return (
                          <Show when={run()}>
                            <ToolCallGroup
                              name={run()!.name}
                              calls={run()!.calls}
                              dialogId={props.dialogId}
                              result={resultOf}
                              running={props.sending}
                              superseded={run()!.calls.every((c) =>
                                todoCallStates().superseded.has(c.id)
                              )}
                              renderCall={renderCall}
                            />
                          </Show>
                        );
                      }}
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

/** A user message has images or PDFs attached (a tool result's are its screenshots). */
function hasAttachments(m: ChatMessage): boolean {
  return m.role === 'user' && !!m.attachments?.length;
}

/** The PDFs attached to a user message, named in its meta. */
function userPdfs(m: ChatMessage): ComposerPdf[] {
  const ids = new Set(hasAttachments(m) ? m.attachments : []);
  return (m.meta?.pdfs ?? []).filter((pdf) => ids.has(pdf.id));
}

/** The images attached to a user message: its attachments that aren't PDFs. */
function userImages(m: ChatMessage): string[] {
  if (!hasAttachments(m)) return [];
  const pdfs = new Set((m.meta?.pdfs ?? []).map((pdf) => pdf.id));
  return m.attachments!.filter((id) => !pdfs.has(id));
}

/**
 * Under a message, shown on hover: copies the whole message. Other apps
 * get its text; pasted back into the chat input, its pastes, attached
 * files, images and PDFs come back as chips (lib/composerClipboard.ts).
 */
function CopyMessageButton(props: { segments: DraftSegment[] }) {
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
        if (!(await copySegments(props.segments))) return;
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
 * The model's reasoning, as a line like a tool call's: "Thinking…" with its
 * latest line while it thinks, "Thought for 12 s" after; opens to the whole
 * text (plain, it can be long).
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
    <div class="tool-group" classList={{ open: open(), active: props.thinking }}>
      <div class="tool-group-header" onClick={() => setOpen((o) => !o)}>
        <button type="button" class="tool-group-toggle" aria-expanded={open()}>
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
          <span class="tool-group-items chat-thinking-preview">{lastLine()}</span>
        </Show>
        <Show when={props.thinking}>
          <span class="tool-call-spinner" aria-label="Thinking" />
        </Show>
      </div>
      <Show when={open()}>
        <div class="tool-group-calls">
          <div class="chat-thinking-text">{props.text}</div>
        </div>
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
  if (m.role === 'user') {
    const images = userImages(m).length ? 168 : 0;
    const pdfs = userPdfs(m).length ? 30 : 0;
    return 32 + (isBlank(m.content) ? 0 : LINE * lines(m.content)) + images + pdfs + 24;
  }
  let height = 0;
  if (m.content.trim()) {
    const fences = (m.content.match(/```/g) ?? []).length / 2;
    height += 24 + LINE * lines(m.content) + fences * 40 + 24;
  }
  height += (m.toolCalls?.length ?? 0) * 40;
  return Math.max(40, Math.round(height));
}
