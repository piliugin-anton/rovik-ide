// chatStore.ts
//
// Chat state for the UI: one page of the chat list and up to MAX_OPEN open
// chats. Everything else stays in the database (src-tauri/src/chat_db).
//
// One store for the app (`chatStore`), not one per chat panel: agent runs
// go on in the backend while the panel isn't mounted, and what they store
// arrives here (lib/agentRuns.ts) either way.
//
// The UI's own actions update the store first, so the UI never waits, then
// persist. Writes go through one queue, so the database sees them in the
// order they were made, even though Tauri may run concurrent commands in
// parallel. What a run stores is already in the database when it arrives.
import { createStore, produce } from 'solid-js/store';
import type { ChatMessage, DialogCursor, DialogSummary, MessageStatus } from '../types';
import { chatApi, fromRow, type MessageRow } from './chatApi';

export type { MessageRow };

const MAX_OPEN = 5;
const PAGE = 50;
const LIST_PAGE = 50;
const PREVIEW_CHARS = 200;

export interface OpenChat {
  messages: ChatMessage[];
  hasOlder: boolean;
  /** Opened around a search hit: newer messages exist that aren't loaded. */
  hasNewer: boolean;
  loadingOlder: boolean;
  lastUsed: number;
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
  /** Chats with an agent run going on: never evicted, never switched away from. */
  running: Record<string, true>;
  /** Chats whose run is being started (until the backend answers). */
  starting: Record<string, number>;
  /** Chats with agent messages the user hasn't seen yet (dot indicator). */
  unread: Record<string, true>;
  /** The chat panel is mounted (sidebar tab is "chat"): the active chat is seen. */
  chatVisible: boolean;
}

export type MessagePatch = Partial<
  Pick<ChatMessage, 'content' | 'toolCalls' | 'error' | 'failed' | 'pending' | 'meta' | 'finalized'>
> & { status?: MessageStatus | null };

const UNREAD_KEY = 'tauri-ide:chat-unread';

/** Unread chat ids from the last session (survives restarts; pruned on read/delete). */
function loadPersistedUnread(): Record<string, true> {
  try {
    const raw = localStorage.getItem(UNREAD_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    const out: Record<string, true> = {};
    for (const id of Object.keys(parsed as Record<string, unknown>)) {
      if (typeof id === 'string' && id) out[id] = true;
    }
    return out;
  } catch {
    return {};
  }
}

function savePersistedUnread(unread: Record<string, true>) {
  try {
    if (Object.keys(unread).length === 0) localStorage.removeItem(UNREAD_KEY);
    else localStorage.setItem(UNREAD_KEY, JSON.stringify(unread));
  } catch (err) {
    console.error('Failed to persist unread chats:', err);
  }
}

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
    running: {},
    starting: {},
    unread: loadPersistedUnread(),
    chatVisible: false,
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
      .filter((id) => id !== state.activeId && !isRunning(id))
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

  /*
   * The project whose chats are listed and created (chat_db/projects.rs);
   * null: no folder open, the chats with no project.
   */
  let projectId: string | null = null;

  /**
   * Another project: its chat list, from the start. Chats with a run still
   * going stay loaded (the run finishes in the background); the rest of the
   * old project's chats are let go.
   */
  async function switchProject(id: string | null) {
    if (id === projectId && state.list.loaded) return;
    projectId = id;
    setState(
      produce((s) => {
        s.list = { items: [], nextCursor: null, loading: false, loaded: false };
        for (const chatId of Object.keys(s.open)) if (!s.running[chatId]) delete s.open[chatId];
        s.activeId = null;
        s.active = null;
      })
    );
    await loadFirstPage();
  }

  async function loadFirstPage() {
    setState('list', 'loading', true);
    const forProject = projectId;
    try {
      const page = await chatApi.listDialogs(forProject, null, LIST_PAGE);
      if (forProject !== projectId) return; // switched meanwhile
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
      const forProject = projectId;
      const page = await chatApi.listDialogs(forProject, nextCursor, LIST_PAGE);
      if (forProject !== projectId) return;
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
    markRead(summary.id);
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

  function setRunning(dialogId: string, running: boolean) {
    setState(
      'running',
      produce((r) => {
        if (running) r[dialogId] = true;
        else delete r[dialogId];
      })
    );
  }

  /** A run is being started in the chat (counted: starts can overlap). */
  function setStarting(dialogId: string, starting: boolean) {
    setState(
      'starting',
      produce((s) => {
        const n = (s[dialogId] ?? 0) + (starting ? 1 : -1);
        if (n > 0) s[dialogId] = n;
        else delete s[dialogId];
      })
    );
  }

  /** A run is going on in the chat, or being started. */
  const isRunning = (dialogId: string) => !!state.running[dialogId] || !!state.starting[dialogId];

  /*
   * -------------------------------------------------------------------------
   * Unread indicators (dot on the "AI Chat" tab + in the chat list)
   * -------------------------------------------------------------------------
   */

  /** Whether the user is looking at this chat's messages right now. */
  function isSeen(dialogId: string): boolean {
    return state.chatVisible && state.activeId === dialogId;
  }

  /** Marks a chat unseen when an agent message arrives while it isn't seen. */
  function markUnreadIfUnseen(dialogId: string) {
    if (isSeen(dialogId) || state.unread[dialogId]) return;
    setState('unread', dialogId, true);
    savePersistedUnread(state.unread);
  }

  /** The user looked at the chat: its dot goes away. */
  function markRead(dialogId: string) {
    if (!state.unread[dialogId]) return;
    setState(
      'unread',
      produce((u) => delete u[dialogId])
    );
    savePersistedUnread(state.unread);
  }

  /** Whether the chat panel's messages are on screen (sidebar tab is chat). */
  function setChatVisible(visible: boolean) {
    setState('chatVisible', visible);
    // Showing the panel means the active chat is seen.
    if (visible && state.activeId) markRead(state.activeId);
  }

  /** Any chat the user hasn't looked at yet (drives the tab dot). */
  const hasAnyUnread = () => Object.keys(state.unread).length > 0;

  /** Any chat with an agent run going on (drives the tab spinner). */
  const hasAnyRunning = () =>
    Object.keys(state.running).length > 0 || Object.keys(state.starting).length > 0;

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
        };
        s.activeId = summary.id;
        s.active = { ...summary };
      })
    );
    const forProject = projectId;
    void persist(() => chatApi.createDialog({ ...summary, projectId: forProject }));
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
        delete s.unread[dialogId];
        if (s.activeId === dialogId) {
          s.activeId = null;
          s.active = null;
        }
      })
    );
    savePersistedUnread(state.unread);
    return persist(() => chatApi.deleteDialog(dialogId));
  }

  /** The chat list's summary after messages were added at the end. */
  function noteAdded(dialogId: string, added: ChatMessage[]) {
    const now = Date.now();
    const last = [...added]
      .reverse()
      .find(
        (m) =>
          (m.role === 'user' || m.role === 'assistant') &&
          m.finalized &&
          !m.meta?.harness &&
          m.content.trim()
      );
    updateSummary(dialogId, (d) => {
      d.updatedAt = now;
      d.messageCount += added.length;
      if (last) d.preview = previewOf(last.content);
    });
    moveToTop(dialogId);
  }

  /**
   * Shows a message before the backend stores it (a sent message: the run
   * stores it first thing). The stored copy replaces it when it arrives.
   */
  function addLocal(dialogId: string, message: ChatMessage) {
    const stamped = { ...message, createdAt: message.createdAt ?? Date.now() };
    if (state.open[dialogId]) {
      setState('open', dialogId, 'messages', (list) => [...list, stamped]);
    }
    noteAdded(dialogId, [stamped]);
  }

  /** Messages a run stored (lib/agentRuns.ts); ones shown already are updated. */
  function receiveMessages(dialogId: string, rows: (MessageRow & { pending?: boolean })[]) {
    const chat = state.open[dialogId];
    const known = new Set(chat?.messages.map((m) => m.id) ?? []);
    const added: ChatMessage[] = [];
    // Opened around a search hit: the newest aren't loaded, so these wait for that.
    const showNew = !!chat && !chat.hasNewer;
    for (const row of rows) {
      const message: ChatMessage = { ...fromRow(row), ...(row.pending ? { pending: true } : {}) };
      if (known.has(message.id)) {
        patchMessage(dialogId, message.id, { seq: message.seq, finalized: message.finalized });
        continue;
      }
      added.push(message);
    }
    if (added.length === 0) return;
    if (showNew) setState('open', dialogId, 'messages', (list) => [...list, ...added]);
    noteAdded(dialogId, added);
    // New agent activity the user isn't looking at: dot indicator.
    if (added.some((m) => m.role !== 'user' && !m.meta?.harness)) markUnreadIfUnseen(dialogId);
  }

  /** A change a run made to a stored message (streamed text, its final state). */
  function applyPatch(dialogId: string, messageId: string, patch: MessagePatch) {
    const i = indexOfMessage(dialogId, messageId);
    if (i < 0) {
      // Streaming into a chat that isn't open: still unseen activity.
      markUnreadIfUnseen(dialogId);
      return;
    }
    const message = messagesOf(dialogId)[i];
    const { status, ...fields } = patch;
    const next: Partial<ChatMessage> = { ...fields };
    if ('status' in patch) next.status = status ?? undefined;
    // A reply still being written (also after a reload, when its stored copy
    // didn't say so).
    if (!fields.finalized && !message.finalized && message.role === 'assistant')
      next.pending = true;
    setState('open', dialogId, 'messages', i, next);

    const content = fields.content ?? message.content;
    if (fields.finalized && message.role === 'assistant' && !fields.error && content.trim()) {
      updateSummary(dialogId, (d) => {
        d.preview = previewOf(content);
        d.updatedAt = Date.now();
      });
    }
    if (message.role !== 'user' && !(message.meta as { harness?: unknown } | undefined)?.harness)
      markUnreadIfUnseen(dialogId);
  }

  /** Store-only change. */
  function patchMessage(dialogId: string, messageId: string, patch: Partial<ChatMessage>) {
    const i = indexOfMessage(dialogId, messageId);
    if (i >= 0) setState('open', dialogId, 'messages', i, patch);
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
    switchProject,
    /** The project being shown (null: none). */
    projectId: () => projectId,
    openChat,
    openChatAround,
    loadOlder,
    loadNewer,
    setRunning,
    setStarting,
    isRunning,
    createDialog,
    renameDialog,
    deleteDialog,
    addLocal,
    receiveMessages,
    applyPatch,
    markFailed,
    clearError,
    markRead,
    setChatVisible,
    hasAnyUnread,
    hasAnyRunning,
  };
}

export type ChatStore = ReturnType<typeof createChatStore>;

/** The app's chats. */
export const chatStore = createChatStore();
