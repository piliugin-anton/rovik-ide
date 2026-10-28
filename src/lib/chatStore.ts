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
import { batch } from 'solid-js';
import { createStore, produce, reconcile } from 'solid-js/store';
import type { ChatMessage, ChatPlace, DialogCursor, DialogSummary, MessageStatus } from '../types';
import { keptStage, readStage, type StageMeta } from './autopilot';
import type { ChatMode } from './chatMode';
import { chatApi, fromRow, type MessageRow } from './chatApi';
import { listedBy, sameOwners, type ChatOwners } from './openFolders';
import { sharedMap, type Entries } from './sharedState';

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
  /**
   * Autopilot stage records as patches brought them, by message id, for
   * messages the chat doesn't hold (lib/autopilot.ts `taskRecords`): the
   * strip reads the task's records from the database, and a remote chat's
   * mirror isn't written by a patch, only by the pull that follows it.
   */
  stagePatches?: Record<string, StageMeta>;
}

/**
 * A message sent to the chat's run while it works (steering), that the run
 * hasn't read yet: shown at the end of the chat until it's read, taken back
 * or handed back (docs/design/steering-plan.md 3.7).
 */
export interface QueuedSteer {
  /** As it's stored once read: a user message, its text, files and meta. */
  message: ChatMessage;
  /** When it was sent, in milliseconds since the epoch. */
  queuedAt: number;
  /** The run could no longer take it: it's sent as a new message once the run has ended. */
  late?: boolean;
  /** The mode of the run it was sent to: a late one is sent in it. */
  mode?: ChatMode;
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
  /**
   * Chats whose Autopilot driver is at work on their task (`taskDriving`):
   * between two of its stages, or in one the IDE carries out itself, no run
   * goes, but the task does. They count as running.
   */
  driving: Record<string, true>;
  /** Chats whose run is being started (until the backend answers). */
  starting: Record<string, number>;
  /** Chats whose run takes messages while it works: an Agent-mode run's (`runStarted`). */
  steerable: Record<string, true>;
  /** Messages sent to a run while it works that it hasn't read yet, by chat, oldest first. */
  queued: Record<string, QueuedSteer[]>;
  /** Chats with agent messages the user hasn't seen yet (dot indicator). */
  unread: Record<string, true>;
  /** The chat panel is mounted (sidebar tab is "chat"): the active chat is seen. */
  chatVisible: boolean;
}

export type MessagePatch = Partial<
  Pick<ChatMessage, 'content' | 'toolCalls' | 'error' | 'failed' | 'pending' | 'meta' | 'finalized'>
> & { status?: MessageStatus | null };

/** A patch's object field, merged into the store's (reconcile); none clears it. */
export function merged<T extends object>(value: T | null | undefined) {
  return value ? reconcile(value) : undefined;
}

const UNREAD_KEY = 'rovik.chatUnread';
/** Where the unread ids were kept before the app was renamed. */
const LEGACY_UNREAD_KEY = 'tauri-ide:chat-unread';

/**
 * The unread chats' ids (survive restarts; gone once read or deleted),
 * shared by the windows (lib/sharedState.ts): a chat read in one window
 * isn't unread again because another saved its older copy, and a window
 * follows the others' marks as it comes to the front.
 */
function unreadMarks() {
  try {
    // Kept under another name before the app was renamed.
    const legacy = localStorage.getItem(LEGACY_UNREAD_KEY);
    if (legacy !== null) {
      if (localStorage.getItem(UNREAD_KEY) === null) localStorage.setItem(UNREAD_KEY, legacy);
      localStorage.removeItem(LEGACY_UNREAD_KEY);
    }
  } catch {
    // No storage: none kept from before.
  }
  return sharedMap('chat-unread', { key: UNREAD_KEY });
}

function unreadIds(saved: Entries): Record<string, true> {
  const out: Record<string, true> = {};
  for (const id of Object.keys(saved)) if (id && saved[id]) out[id] = true;
  return out;
}

function previewOf(text: string): string {
  return text.replace(/\s+/g, ' ').trim().slice(0, PREVIEW_CHARS);
}

export function createChatStore() {
  const marks = unreadMarks();
  const [state, setState] = createStore<ChatState>({
    list: { items: [], nextCursor: null, loading: false, loaded: false },
    open: {},
    activeId: null,
    active: null,
    error: null,
    running: {},
    driving: {},
    starting: {},
    steerable: {},
    queued: {},
    unread: unreadIds(marks.all()),
    chatVisible: false,
  });
  // Another window's marks: a chat read there, or a reply come in there.
  marks.subscribe((saved) => {
    const unread = unreadIds(saved);
    // The chat looked at here is read, whatever was saved.
    if (state.activeId && isSeen(state.activeId) && unread[state.activeId]) {
      delete unread[state.activeId];
      void marks.set({ [state.activeId]: null });
    }
    setState('unread', reconcile(unread));
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

  /** A chat's summary: the open one's, or the list's. */
  function summaryOf(dialogId: string): DialogSummary | undefined {
    if (state.active?.id === dialogId) return state.active;
    return state.list.items.find((d) => d.id === dialogId);
  }

  /** Where a chat is: on a host, moving, or here (absent). */
  function setPlace(dialogId: string, place: ChatPlace | undefined) {
    updateSummary(dialogId, (d) => {
      d.place = place;
    });
  }

  /** Where a chat is, read again: after a step of its move there or back. */
  async function refreshPlace(dialogId: string) {
    try {
      const fresh = await chatApi.getDialog(dialogId);
      if (fresh) setPlace(dialogId, fresh.place);
    } catch (err) {
      console.error('Chat summary unavailable:', err);
    }
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
   * Whose chats are listed (lib/openFolders.ts, `chatOwners`): the window's
   * owners, each open folder's project and its workspace's own, or, with no
   * folder open, the chats with no project. Null until the panel says.
   */
  let owners: ChatOwners | null = null;

  /** Whether `projectId` (null: no project) is one of the owners listed. */
  function listsOwner(projectId: string | null): boolean {
    return !!owners && listedBy(owners, projectId);
  }

  /**
   * Shows the chats of `next` (workspaces-plan.md, 3.8). Another target's:
   * its chat list, from the start; chats with a run still going stay loaded
   * (the run finishes in the background), the rest of the old one's are let
   * go. The same target's, other projects (a folder resolved, added,
   * removed or located): the list is read again for them, and the open chat
   * stays while its own project is among them, its input with it.
   */
  async function showOwners(next: ChatOwners) {
    if (owners && sameOwners(owners, next) && state.list.loaded) return;
    const sameTarget = owners !== null && owners.target === next.target;
    owners = next;
    const keepsActive = sameTarget && listsOwner(state.active?.projectId ?? null);
    setState(
      produce((s) => {
        if (!sameTarget) s.list = { items: [], nextCursor: null, loading: false, loaded: false };
        if (keepsActive) return;
        for (const chatId of Object.keys(s.open)) {
          if (!s.running[chatId] && (!sameTarget || chatId === s.activeId)) delete s.open[chatId];
        }
        s.activeId = null;
        s.active = null;
      })
    );
    await loadFirstPage();
  }

  async function loadFirstPage() {
    setState('list', 'loading', true);
    const listing = owners;
    try {
      const page = await chatApi.listDialogs(listing?.projects ?? null, null, LIST_PAGE);
      if (listing !== owners) return; // others shown meanwhile
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
      const listing = owners;
      const page = await chatApi.listDialogs(listing?.projects ?? null, nextCursor, LIST_PAGE);
      if (listing !== owners) return;
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

  /** The chat's flag in `running` or `driving`, set or cleared. */
  const flag = (dialogId: string, on: boolean) =>
    produce((flags: Record<string, true>) => {
      if (on) flags[dialogId] = true;
      else delete flags[dialogId];
    });

  function setRunning(dialogId: string, running: boolean) {
    setState('running', flag(dialogId, running));
  }

  /** The chat's Autopilot driver is at work on its task, or no longer is. */
  function setDriving(dialogId: string, driving: boolean) {
    setState('driving', flag(dialogId, driving));
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

  /** The chat's run takes messages while it works, or no longer does. */
  function setSteerable(dialogId: string, steerable: boolean) {
    setState('steerable', flag(dialogId, steerable));
  }

  const isSteerable = (dialogId: string) => !!state.steerable[dialogId];

  /*
   * -------------------------------------------------------------------------
   * Messages sent to a run while it works (steering)
   * -------------------------------------------------------------------------
   */

  /** The chat's messages its run hasn't read yet, oldest first. */
  const queuedOf = (dialogId: string): QueuedSteer[] => state.queued[dialogId] ?? [];

  /** Shows a message sent to the chat's run at the end of the chat (once). */
  function queueSteer(dialogId: string, queued: QueuedSteer) {
    if (queuedOf(dialogId).some((q) => q.message.id === queued.message.id)) return;
    setState('queued', dialogId, (list) => [...(list ?? []), queued]);
  }

  /** Takes a message off the chat's queue: it was read, taken back or handed back. */
  function unqueueSteer(dialogId: string, messageId: string): QueuedSteer | undefined {
    const found = queuedOf(dialogId).find((q) => q.message.id === messageId);
    if (!found) return undefined;
    setState(
      'queued',
      produce((all) => {
        const rest = (all[dialogId] ?? []).filter((q) => q.message.id !== messageId);
        if (rest.length > 0) all[dialogId] = rest;
        else delete all[dialogId];
      })
    );
    return found;
  }

  /** The run could no longer take the message: it goes as a new message after the run. */
  function markSteerLate(dialogId: string, messageId: string) {
    const list = queuedOf(dialogId);
    const i = list.findIndex((q) => q.message.id === messageId);
    if (i >= 0) setState('queued', dialogId, i, 'late', true);
  }

  /**
   * A run is going on in the chat, or being started, or its Autopilot
   * task's driver is at work between two runs.
   */
  const isRunning = (dialogId: string) =>
    !!state.running[dialogId] || !!state.starting[dialogId] || !!state.driving[dialogId];

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
    void marks.set({ [dialogId]: true });
  }

  /** The user looked at the chat: its dot goes away. */
  function markRead(dialogId: string) {
    if (!state.unread[dialogId]) return;
    setState(
      'unread',
      produce((u) => delete u[dialogId])
    );
    void marks.set({ [dialogId]: null });
  }

  /** Whether the chat panel's messages are on screen (sidebar tab is chat). */
  function setChatVisible(visible: boolean) {
    setState('chatVisible', visible);
    // Showing the panel means the active chat is seen.
    if (visible && state.activeId) markRead(state.activeId);
  }

  /** Any chat listed (this window's project's) the user hasn't looked at
   *  yet (drives the tab dot). Another project's shows where it's open:
   *  this window's list hasn't got it to find. */
  const hasAnyUnread = () => state.list.items.some((d) => state.unread[d.id]);

  /** Any chat with an agent run going on (drives the tab spinner). */
  const hasAnyRunning = () =>
    Object.keys(state.running).length > 0 ||
    Object.keys(state.starting).length > 0 ||
    Object.keys(state.driving).length > 0;

  /*
   * -------------------------------------------------------------------------
   * Changing chats
   * -------------------------------------------------------------------------
   */

  /**
   * A new chat, open and active, of `projectId`: the folder it works in's
   * project, or null with no folder open (workspaces-plan.md, 3.8); a
   * workspace's, with the `folders` it works in (`"all"`, or names as JSON).
   */
  function createDialog(
    title: string,
    projectId: string | null,
    folders: string | null = null
  ): DialogSummary {
    const now = Date.now();
    const summary: DialogSummary = {
      id: crypto.randomUUID(),
      title,
      createdAt: now,
      updatedAt: now,
      messageCount: 0,
      preview: null,
      projectId,
      folders,
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
    void persist(() => chatApi.createDialog({ ...summary, projectId, folders }));
    evictIfNeeded();
    return summary;
  }

  /**
   * The empty chat `dialogId` works in the folder of `projectId` from now on
   * (New chat reusing it for another folder, workspaces-plan.md 3.8), or in a
   * workspace's `folders` when `projectId` is a workspace's, once the writes
   * queued before it are in. False when the database refused it (it has a
   * message by now): the chat stays where it was.
   */
  async function moveEmpty(
    dialogId: string,
    projectId: string | null,
    folders: string | null = null
  ): Promise<boolean> {
    let moved = false;
    await persist(async () => {
      try {
        await chatApi.moveEmptyDialog(dialogId, projectId, folders);
        moved = true;
      } catch (err) {
        console.warn('The empty chat stays where it is:', err);
      }
    });
    if (moved) {
      updateSummary(dialogId, (d) => {
        d.projectId = projectId;
        d.folders = folders;
      });
    }
    return moved;
  }

  /**
   * Apply (D32): workspace chat `dialogId` works in `folders` from now on
   * (`"all"`, or the names checked as JSON), after the writes queued so
   * far. Null once it does; else the backend's refusal, which says why.
   */
  async function setFolders(dialogId: string, folders: string): Promise<string | null> {
    const refused = await persist(() =>
      chatApi.setFolders(dialogId, folders).then(
        () => null,
        (err: unknown) => String(err)
      )
    );
    if (refused) return refused;
    updateSummary(dialogId, (d) => {
      d.folders = folders;
    });
    return null;
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
    void marks.set({ [dialogId]: null });
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
    // A queued message read by the run: its row shows where it was stored.
    for (const row of rows) unqueueSteer(dialogId, row.id);
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
      // A stage record the open chat doesn't hold (the page held is a long
      // task's last): kept for its strip.
      const stage = readStage(patch.meta?.stage);
      const open = state.open[dialogId];
      if (stage && open) {
        const kept = open.stagePatches?.[messageId];
        if (keptStage(kept, stage) === stage)
          setState('open', dialogId, 'stagePatches', (all) => ({ ...all, [messageId]: stage }));
      }
      // Streaming into a chat that isn't open: still unseen activity.
      markUnreadIfUnseen(dialogId);
      return;
    }
    const message = messagesOf(dialogId)[i];
    const { status, toolCalls, meta, ...fields } = patch;
    const next: Partial<ChatMessage> = { ...fields };
    if ('status' in patch) next.status = status ?? undefined;
    // A reply still being written (also after a reload, when its stored copy
    // didn't say so).
    if (!fields.finalized && !message.finalized && message.role === 'assistant')
      next.pending = true;
    // A stage record that ended stays ended: a late pull's copy from before
    // its end isn't taken (`keptStage`).
    const held = message.meta?.stage;
    const stale = !!held && !!meta?.stage && keptStage(held, meta.stage) === held;
    batch(() => {
      setState('open', dialogId, 'messages', i, next);
      // Merged into what's there, not swapped for new objects: the parts
      // that didn't change keep theirs, and so does what's drawn from them
      // (a tool call's block opened while the reply streams, a failed
      // check's output opened in the verification record), which a click
      // landing as a patch came in otherwise missed.
      if ('toolCalls' in patch)
        setState('open', dialogId, 'messages', i, 'toolCalls', merged(toolCalls));
      if ('meta' in patch && !stale)
        setState('open', dialogId, 'messages', i, 'meta', merged(meta));
    });

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
    showOwners,
    /** Whose chats are shown (null: the panel hasn't said yet). */
    owners: () => owners,
    listsOwner,
    openChat,
    openChatAround,
    loadOlder,
    loadNewer,
    setRunning,
    setDriving,
    setStarting,
    isRunning,
    setSteerable,
    isSteerable,
    queuedOf,
    queueSteer,
    unqueueSteer,
    markSteerLate,
    createDialog,
    moveEmpty,
    setFolders,
    renameDialog,
    deleteDialog,
    summaryOf,
    setPlace,
    refreshPlace,
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
