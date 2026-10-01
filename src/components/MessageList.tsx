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
import MessageContent from './MessageContent';
import ToolCallBlock from './ToolCallBlock';

const BOTTOM_TOLERANCE_PX = 8;
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
}) {
  let scrollRef: HTMLDivElement | undefined;
  let innerRef: HTMLDivElement | undefined;
  let topSentinel: HTMLDivElement | undefined;
  let bottomSentinel: HTMLDivElement | undefined;

  const messages = (): ChatMessage[] => props.chat?.messages ?? [];
  const [win, setWin] = createSignal<MessageWindow>({ start: 0, end: 0 });
  const [stick, setStick] = createSignal(true);
  const [flashId, setFlashId] = createSignal<string | null>(null);

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
  }

  function handleScroll() {
    const el = scrollRef;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    setStick(distance <= BOTTOM_TOLERANCE_PX && atNewest());
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
   * -----------------------------------------------------------------------
   * Sentinels: move the window, load more when it reaches the loaded edge
   * -----------------------------------------------------------------------
   */

  let busy = false;

  async function onTopVisible() {
    if (busy) return;
    busy = true;
    try {
      if (win().start === 0 && props.chat?.hasOlder) {
        await props.onLoadOlder(); // the effect above shifts the window
      }
      if (win().start > 0) {
        setStick(false);
        keepAnchor(() => setWin((w) => shiftUp(w)));
      }
    } finally {
      busy = false;
    }
  }

  async function onBottomVisible() {
    if (busy) return;
    busy = true;
    try {
      if (win().end >= messages().length && props.chat?.hasNewer) {
        await props.onLoadNewer();
      }
      if (win().end < messages().length) {
        keepAnchor(() => setWin((w) => shiftDown(w, messages().length)));
      }
    } finally {
      busy = false;
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
    <div ref={scrollRef} class="chat-messages" onScroll={handleScroll}>
      <div ref={topSentinel} class="chat-sentinel" aria-hidden="true" />
      <Show when={props.chat?.loadingOlder}>
        <div class="chat-loading-older">Loading earlier messages…</div>
      </Show>

      <div ref={innerRef} class="chat-messages-inner">
        <For each={visible()} fallback={<div class="empty-state">Ask the model anything</div>}>
          {(m) => (
            <div class="chat-item" classList={{ flash: flashId() === m.id }} data-id={m.id}>
              <Show when={m.pending && m.content.length === 0 && !m.toolCalls?.length}>
                <div class="chat-message assistant">
                  <span class="chat-pending">Thinking...</span>
                </div>
              </Show>

              <Show when={m.error}>
                <div
                  class="chat-message"
                  classList={{ user: m.role === 'user', assistant: m.role === 'assistant' }}
                >
                  <span class="chat-error">{m.error}</span>
                </div>
              </Show>

              <Show when={!m.error && !isBlank(m.content)}>
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

              <Show when={m.toolCalls && m.toolCalls.length > 0}>
                <div class="message-tools">
                  <For each={m.toolCalls!}>
                    {(tc) => (
                      <ToolCallBlock
                        toolCall={tc}
                        result={toolResults().get(tc.id)}
                        running={props.sending}
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
