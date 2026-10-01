// chatStore.ts
//
// Chat state for the UI: one page of the chat list and up to MAX_OPEN open
// chats. Everything else stays in the database (src-tauri/src/chat_db).
//
// Actions update the store first, so the UI never waits, then persist.
// Writes go through one queue, so the database sees them in the order they
// were made, even though Tauri may run concurrent commands in parallel.
import { createStore, produce } from 'solid-js/store';
import type { ChatMessage, ChatToolCall, DialogCursor, DialogSummary, ToolMeta } from '../types';
import { chatApi } from './chatApi';

const MAX_OPEN = 5;
const PAGE = 50;
const LIST_PAGE = 50;
const CHECKPOINT_MS = 2000;
const PREVIEW_CHARS = 200;

export interface OpenChat {
  messages: ChatMessage[];
  hasOlder: boolean;
  /** Opened around a search hit: newer messages exist that aren't loaded. */
  hasNewer: boolean;
  loadingOlder: boolean;
  lastUsed: number;
  /** An agent run targets this chat; it is never evicted. */
  busy: boolean;
}

interface ChatState {
  list: {
    items: DialogSummary[];
    nextCursor: DialogCursor | null;
    loading: boolean;
    loaded: boolean;
  };
  open: Record<string, OpenChat>;
  activeId: string | null;
  /** The active chat's summary, even when it isn't in the loaded list page. */
  active: DialogSummary | null;
  /** Last persistence error, shown in the UI. */
  error: string | null;
}

export type MessagePatch = Partial<
  Pick<ChatMessage, 'content' | 'toolCalls' | 'error' | 'failed' | 'pending' | 'meta' | 'finalized'>
>;

function previewOf(text: string): string {
  return text.replace(/\s+/g, ' ').trim().slice(0, PREVIEW_CHARS);
}

export function createChatStore() {
  const [state, setState] = createStore<ChatState>({
    list: { items: [], nextCursor: null, loading: false, loaded: false },
    open: {},
    activeId: null,
    active: null,
    error: null,
  });

  /*
   * -------------------------------------------------------------------------
   * Ordered persistence
   * -------------------------------------------------------------------------
   */

  let queue: Promise<unknown> = Promise.resolve();

  function persist<T>(task: () => Promise<T>): Promise<T | undefined> {
    const run = queue.then(task).catch((err) => {
      console.error('Chat storage error:', err);
      setState('error', String(err));
      return undefined;
    });
    queue = run;
    return run;
  }

  /** Resolves once every write queued so far has finished. */
  function flush(): Promise<void> {
    return queue.then(() => undefined);
  }

  /*
   * -------------------------------------------------------------------------
   * Helpers
   * -------------------------------------------------------------------------
   */

  function messagesOf(dialogId: string): ChatMessage[] {
    return state.open[dialogId]?.messages ?? [];
  }

  /** The streaming message is near the end, so search from there. */
  function indexOfMessage(dialogId: string, messageId: string): number {
    const list = messagesOf(dialogId);
    for (let i = list.length - 1; i >= 0; i--) if (list[i].id === messageId) return i;
    return -1;
  }

  function updateSummary(dialogId: string, fn: (d: DialogSummary) => void) {
    setState(
      produce((s) => {
        const item = s.list.items.find((d) => d.id === dialogId);
        if (item) fn(item);
        if (s.active?.id === dialogId) fn(s.active);
      })
    );
  }

  /** A chat with new messages moves to the top of the list. */
  function moveToTop(dialogId: string) {
    setState(
      produce((s) => {
        const i = s.list.items.findIndex((d) => d.id === dialogId);
        if (i > 0) {
          const [item] = s.list.items.splice(i, 1);
          s.list.items.unshift(item);
        }
      })
    );
  }

  function evictIfNeeded() {
    const ids = Object.keys(state.open);
    if (ids.length <= MAX_OPEN) return;
    const candidates = ids
      .filter((id) => id !== state.activeId && !state.open[id].busy)
      .sort((a, b) => state.open[a].lastUsed - state.open[b].lastUsed);
    for (const id of candidates.slice(0, ids.length - MAX_OPEN)) {
      setState(
        'open',
        produce((open) => delete open[id])
      );
    }
  }

  /*
   * -------------------------------------------------------------------------
   * Chat list
   * -------------------------------------------------------------------------
   */

  async function loadFirstPage() {
    setState('list', 'loading', true);
    try {
      const page = await chatApi.listDialogs(null, LIST_PAGE);
      setState('list', { items: page.items, nextCursor: page.nextCursor, loaded: true });
    } finally {
      setState('list', 'loading', false);
    }
  }

  async function loadMoreDialogs() {
    const { nextCursor, loading } = state.list;
    if (!nextCursor || loading) return;
    setState('list', 'loading', true);
    try {
      const page = await chatApi.listDialogs(nextCursor, LIST_PAGE);
      setState(
        produce((s) => {
          const known = new Set(s.list.items.map((d) => d.id));
          s.list.items.push(...page.items.filter((d) => !known.has(d.id)));
          s.list.nextCursor = page.nextCursor;
        })
      );
    } catch (err) {
      setState('error', String(err));
    } finally {
      setState('list', 'loading', false);
    }
  }

  /*
   * -------------------------------------------------------------------------
   * Opening chats
   * -------------------------------------------------------------------------
   */

  function setActive(summary: DialogSummary) {
    setState({ activeId: summary.id, active: { ...summary } });
  }

  /** Opens a chat at its newest messages (reusing what's loaded). */
  async function openChat(summary: DialogSummary, opts: { reload?: boolean } = {}) {
    setActive(summary);
    const existing = state.open[summary.id];
    if (existing && !opts.reload && !existing.hasNewer) {
      setState('open', summary.id, 'lastUsed', Date.now());
      return;
    }
    const page = await chatApi.getMessages(summary.id, undefined, PAGE);
    setState('open', summary.id, {
      messages: page.messages,
      hasOlder: page.hasOlder,
      hasNewer: false,
      loadingOlder: false,
      lastUsed: Date.now(),
      busy: existing?.busy ?? false,
    });
    evictIfNeeded();
  }

  /** Opens a chat centered on one message (a search hit). */
  async function openChatAround(summary: DialogSummary, messageId: string) {
    const page = await chatApi.getMessagesAround(messageId, 25, 25);
    setActive(summary);
    setState('open', summary.id, {
      messages: page.messages,
      hasOlder: page.hasOlder,
      hasNewer: page.hasNewer,
      loadingOlder: false,
      lastUsed: Date.now(),
      busy: state.open[summary.id]?.busy ?? false,
    });
    evictIfNeeded();
  }

  /** Prepends the next older page; returns how many messages were added. */
  async function loadOlder(dialogId: string): Promise<number> {
    const chat = state.open[dialogId];
    if (!chat || !chat.hasOlder || chat.loadingOlder) return 0;
    setState('open', dialogId, 'loadingOlder', true);
    try {
      const first = chat.messages.find((m) => m.seq !== undefined);
      const page = await chatApi.getMessages(dialogId, first?.seq, PAGE);
      const known = new Set(messagesOf(dialogId).map((m) => m.id));
      const older = page.messages.filter((m) => !known.has(m.id));
      setState(
        'open',
        dialogId,
        produce((c) => {
          c.messages.unshift(...older);
          c.hasOlder = page.hasOlder;
        })
      );
      return older.length;
    } finally {
      setState('open', dialogId, 'loadingOlder', false);
    }
  }

  /** Appends the next newer page (after opening around a search hit). */
  async function loadNewer(dialogId: string): Promise<number> {
    const chat = state.open[dialogId];
    const last = chat?.messages[chat.messages.length - 1];
    if (!chat || !chat.hasNewer || !last) return 0;
    const page = await chatApi.getMessagesAround(last.id, 0, PAGE);
    const known = new Set(messagesOf(dialogId).map((m) => m.id));
    const newer = page.messages.filter((m) => !known.has(m.id));
    setState(
      'open',
      dialogId,
      produce((c) => {
        c.messages.push(...newer);
        c.hasNewer = page.hasNewer;
      })
    );
    return newer.length;
  }

  function setBusy(dialogId: string, busy: boolean) {
    if (state.open[dialogId]) setState('open', dialogId, 'busy', busy);
  }

  /*
   * -------------------------------------------------------------------------
   * Changing chats
   * -------------------------------------------------------------------------
   */

  function createDialog(title: string): DialogSummary {
    const now = Date.now();
    const summary: DialogSummary = {
      id: crypto.randomUUID(),
      title,
      createdAt: now,
      updatedAt: now,
      messageCount: 0,
      preview: null,
    };
    setState(
      produce((s) => {
        s.list.items.unshift(summary);
        s.open[summary.id] = {
          messages: [],
          hasOlder: false,
          hasNewer: false,
          loadingOlder: false,
          lastUsed: now,
          busy: false,
        };
        s.activeId = summary.id;
        s.active = { ...summary };
      })
    );
    void persist(() => chatApi.createDialog(summary));
    evictIfNeeded();
    return summary;
  }

  function renameDialog(dialogId: string, title: string) {
    updateSummary(dialogId, (d) => {
      d.title = title;
    });
    return persist(() => chatApi.renameDialog(dialogId, title));
  }

  function deleteDialog(dialogId: string) {
    setState(
      produce((s) => {
        s.list.items = s.list.items.filter((d) => d.id !== dialogId);
        delete s.open[dialogId];
        if (s.activeId === dialogId) {
          s.activeId = null;
          s.active = null;
        }
      })
    );
    return persist(() => chatApi.deleteDialog(dialogId));
  }

  /**
   * Adds messages to a chat. Resolves once they are stored, which matters
   * before a request: the backend builds the model's history from the
   * database.
   */
  function appendMessages(dialogId: string, messages: ChatMessage[]) {
    const now = Date.now();
    const stamped = messages.map((m) => ({ ...m, createdAt: m.createdAt ?? now }));
    if (state.open[dialogId]) {
      setState('open', dialogId, 'messages', (list) => [...list, ...stamped]);
    }
    const last = [...stamped]
      .reverse()
      .find(
        (m) => (m.role === 'user' || m.role === 'assistant') && m.finalized && m.content.trim()
      );
    updateSummary(dialogId, (d) => {
      d.updatedAt = now;
      d.messageCount += stamped.length;
      if (last) d.preview = previewOf(last.content);
    });
    moveToTop(dialogId);

    return persist(async () => {
      const seqs = await chatApi.appendMessages(dialogId, stamped);
      // Record the database positions; paging for older messages uses them.
      setState(
        'open',
        dialogId,
        produce((c) => {
          if (!c) return;
          stamped.forEach((m, i) => {
            const target = c.messages.find((x) => x.id === m.id);
            if (target) target.seq = seqs[i];
          });
        })
      );
    });
  }

  /** Store-only change, e.g. each streamed token. */
  function patchMessage(dialogId: string, messageId: string, patch: MessagePatch) {
    const i = indexOfMessage(dialogId, messageId);
    if (i >= 0) setState('open', dialogId, 'messages', i, patch);
  }

  /*
   * Streaming checkpoints: at most one write per message per CHECKPOINT_MS,
   * so a crash mid-reply loses seconds, not the reply.
   */
  const checkpoints = new Map<string, { timer: number; dialogId: string }>();

  function checkpoint(dialogId: string, messageId: string) {
    if (checkpoints.has(messageId)) return;
    const timer = window.setTimeout(() => {
      checkpoints.delete(messageId);
      const message = messagesOf(dialogId).find((m) => m.id === messageId);
      if (!message || message.finalized) return;
      const content = message.content;
      const toolCalls = message.toolCalls ? [...message.toolCalls] : undefined;
      void persist(() => chatApi.updateMessage(messageId, { content, toolCalls }));
    }, CHECKPOINT_MS);
    checkpoints.set(messageId, { timer, dialogId });
  }

  /** Final content of a message: stored, indexed for search, and previewed. */
  function finalizeMessage(
    dialogId: string,
    messageId: string,
    fields: { content?: string; toolCalls?: ChatToolCall[]; error?: string; meta?: ToolMeta }
  ) {
    const pending = checkpoints.get(messageId);
    if (pending) {
      window.clearTimeout(pending.timer);
      checkpoints.delete(messageId);
    }
    patchMessage(dialogId, messageId, { ...fields, pending: false, finalized: true });

    const message = messagesOf(dialogId).find((m) => m.id === messageId);
    const content = fields.content ?? message?.content;
    if (message?.role === 'assistant' && !fields.error && content?.trim()) {
      updateSummary(dialogId, (d) => {
        d.preview = previewOf(content);
        d.updatedAt = Date.now();
      });
    }
    return persist(() =>
      chatApi.updateMessage(messageId, {
        content,
        toolCalls: fields.toolCalls ?? message?.toolCalls,
        error: fields.error,
        meta: fields.meta,
        finalize: true,
      })
    );
  }

  function markFailed(dialogId: string, messageId: string) {
    patchMessage(dialogId, messageId, { failed: true });
    return persist(() => chatApi.updateMessage(messageId, { failed: true }));
  }

  function clearError() {
    setState('error', null);
  }

  return {
    state,
    messagesOf,
    flush,
    loadFirstPage,
    loadMoreDialogs,
    openChat,
    openChatAround,
    loadOlder,
    loadNewer,
    setBusy,
    createDialog,
    renameDialog,
    deleteDialog,
    appendMessages,
    patchMessage,
    checkpoint,
    finalizeMessage,
    markFailed,
    clearError,
  };
}

export type ChatStore = ReturnType<typeof createChatStore>;
