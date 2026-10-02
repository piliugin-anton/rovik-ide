import {
  Accessor,
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import { Channel } from '@tauri-apps/api/core';
import type {
  ChatMessage,
  ChatStreamEvent,
  ChatError,
  ChatToolCall,
  DialogSummary,
  FileSuggestion,
  ModelProfile,
  ModelSelection,
  Provider,
  ToolApproval,
  TreeRow,
} from '../types';
import type { ActiveFileContext } from '../lib/activeContext';
import { buildContextHeader } from '../lib/activeContext';
import {
  loadAiSettings,
  reconcileSelection,
  resolveProfile,
  saveAiSettings,
} from '../lib/aiSettings';
import { createChatStore } from '../lib/chatStore';
import { chatApi } from '../lib/chatApi';
import { highlightTitle } from '../lib/chatSearch';
import { generateChatTitle } from '../lib/chatTitle';
import { cancelChatRequest, sendChatMessage, streamEndSignal, untilStreamEnd } from '../lib/aiChat';
import { describeFailure, retryReason, toStoredFailure } from '../lib/chatErrors';
import { agentToolsFor, executeAgentTool } from '../lib/tools';
import AiSettingsModal, { type AiSettingsPage } from './AiSettingsModal';
import ModelPicker from './ModelPicker';
import { confirmDialog } from './ConfirmDialog';
import { toolResultMeta } from './ToolCallBlock';
import ChatList, { type ChatListRow } from './ChatList';
import MessageList, { type MessageListController } from './MessageList';
import {
  clearPendingDraft,
  listPendingDrafts,
  savePendingDraft,
  takePendingDraft,
  type DraftSegment,
  type PendingDraft,
} from '../lib/pendingDraft';
import { toAbsolutePath, toRelativePath } from '../lib/fileTools';
import {
  checkToolCall,
  DEFAULT_PERMISSIONS,
  describeToolCall,
  isRiskyCommand,
  isSecretPath,
  sameModel,
  type AgentPermissions,
} from '../lib/permissions';
import { compactDiff, isJevModel, reviewWithAi } from '../lib/aiApproval';
import {
  buildEditReview,
  denyAll,
  notifyFileWritten,
  requestApproval,
  type EditReview,
  type ReviewHunk,
} from '../lib/approvals';
import { readFileContent } from '../lib/commands';

// Hard ceiling on how many read→act round-trips one user message can trigger.
const MAX_AGENT_STEPS = 24;

const NEW_CHAT_TITLE = 'New Chat';

interface ChatProps {
  rootPath: Accessor<string | null>;
  activeFileContext: Accessor<ActiveFileContext | null>;
  files: Accessor<TreeRow[]>;
}

export default function ChatPanel(props: ChatProps) {
  const [providers, setProviders] = createSignal<Provider[]>([]);
  const [selection, setSelection] = createSignal<ModelSelection | null>(null);
  const [permissions, setPermissions] = createSignal<AgentPermissions>(DEFAULT_PERMISSIONS);
  /** Live notes on tool calls waiting for approval, by tool call id. */
  const [toolNotes, setToolNotes] = createSignal<Record<string, string>>({});
  const setToolNote = (id: string, note?: string) =>
    setToolNotes((notes) => {
      const next = { ...notes };
      if (note) next[id] = note;
      else delete next[id];
      return next;
    });

  // Chat list page + open chats; everything else stays in the database.
  const store = createChatStore();
  const activeDialogId = () => store.state.activeId;

  const [input, setInput] = createSignal('');
  /** The open settings page, or null when the settings modal is closed. */
  const [settingsPage, setSettingsPage] = createSignal<AiSettingsPage | null>(null);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [historyQuery, setHistoryQuery] = createSignal('');
  /** Lags historyQuery slightly so fast typing doesn't search on every key. */
  const [searchQuery, setSearchQuery] = createSignal('');
  const [sending, setSending] = createSignal(false);
  const [loadingHistory, setLoadingHistory] = createSignal(true);
  /** Set when the chat database couldn't be read at startup. */
  const [historyLoadError, setHistoryLoadError] = createSignal<string | null>(null);
  /** A search hit to scroll to in the open chat. */
  const [focusMessageId, setFocusMessageId] = createSignal<string | null>(null);
  let listController: MessageListController | undefined;

  /** Chats whose title is being generated right now. */
  const titlesInFlight = new Set<string>();

  /*
   * -----------------------------------------------------------------------
   * File autocomplete
   * -----------------------------------------------------------------------
   */

  const [fileSuggestionsOpen, setFileSuggestionsOpen] = createSignal(false);
  const [fileQuery, setFileQuery] = createSignal('');
  const [fileTriggerStart, setFileTriggerStart] = createSignal<number | null>(null);
  const [selectedSuggestionIndex, setSelectedSuggestionIndex] = createSignal(0);

  /*
   * Files explicitly referenced by the current message.
   *
   * These are intentionally separate from the textarea contents.
   *
   * Example:
   *
   *   referencedFiles() =>
   *     [
   *       { name: 'ChatPanel.tsx', path: 'src/components/ChatPanel.tsx' }
   *     ]
   *
   * The user sees only "ChatPanel.tsx", while the complete project-relative
   * path remains available when constructing the message sent to the model.
   */
  const [referencedFiles, setReferencedFiles] = createSignal<FileSuggestion[]>([]);

  let currentChannel: Channel<ChatStreamEvent> | null = null;
  /** The running model request, so the Stop button can cancel it. */
  let currentRequestId: string | null = null;
  /** Stop pressed: no further tool steps or turns in this agent run. */
  let stopRequested = false;
  /** The pending reply of the running request, for the "Stopping…" status. */
  let currentPending: { dialogId: string; messageId: string } | null = null;
  /** Gives up waiting on the running request (see stopRequest). */
  let abandonCurrent: (() => void) | null = null;
  let stopTimer: number | undefined;
  /** How long Stop waits for the backend to confirm before giving up on it. */
  const STOP_GRACE_MS = 2000;

  const stoppedError = (): ChatError => ({
    kind: 'cancelled',
    message: 'Stopped.',
    retryable: false,
    retryAfterMs: null,
    attempts: 0,
    status: null,
    details: null,
    partial: false,
  });

  /** User message currently waiting for its first answer; its draft must not be restored. */
  let inFlightMessageId: string | null = null;
  let rafId: number | null = null;
  let isMounted = true;
  let inputTextareaRef: HTMLTextAreaElement | undefined;

  // Provider + model resolved into what the send path needs.
  const activeProfile = createMemo(() => resolveProfile(providers(), selection()));

  const activeDialog = () => store.state.active;
  const activeChat = () => {
    const id = store.state.activeId;
    return id ? store.state.open[id] : undefined;
  };
  const messages = (): ChatMessage[] => activeChat()?.messages ?? [];

  let searchDebounce: number | undefined;
  createEffect(
    on(historyQuery, (query) => {
      window.clearTimeout(searchDebounce);
      // Clearing is instant; typing waits for a short pause.
      if (!query.trim()) setSearchQuery('');
      else searchDebounce = window.setTimeout(() => setSearchQuery(query), 150);
    })
  );
  onCleanup(() => window.clearTimeout(searchDebounce));

  /*
   * Search runs in the database (FTS5). Results of an older query that
   * arrive late are ignored.
   */
  const [searchRows, setSearchRows] = createSignal<ChatListRow[] | null>(null);
  let searchRun = 0;
  createEffect(
    on(searchQuery, (query) => {
      const run = ++searchRun;
      if (!query.trim()) {
        setSearchRows(null);
        return;
      }
      chatApi
        .search(query)
        .then((hits) => {
          if (run !== searchRun) return;
          setSearchRows(
            hits.map((hit) => ({
              dialog: hit.dialog,
              title: highlightTitle(hit.dialog.title, query),
              snippet: hit.snippet ?? undefined,
              messageId: hit.messageId ?? undefined,
            }))
          );
        })
        .catch((err) => {
          console.error('Chat search failed:', err);
          if (run === searchRun) setSearchRows([]);
        });
    })
  );

  /*
   * Rows for the chat list. Cached per summary object so an unchanged chat
   * keeps the same row (and its DOM).
   */
  const rowCache = new WeakMap<DialogSummary, ChatListRow>();
  const historyRows = createMemo<ChatListRow[]>(
    () =>
      searchRows() ??
      store.state.list.items.map((dialog) => {
        let row = rowCache.get(dialog);
        if (!row || row.title.text !== dialog.title) {
          row = { dialog, title: { text: dialog.title, ranges: [] } };
          rowCache.set(dialog, row);
        }
        return row;
      })
  );

  const fileSuggestions = createMemo<FileSuggestion[]>(() => {
    if (!fileSuggestionsOpen()) return [];

    const query = fileQuery().trim().toLowerCase();

    return (
      props
        .files()
        // Skip ignored rows entirely — they should never be referenceable.
        .filter((file) => !file.ignored)
        // Only suggest actual files, not directories.
        .filter((file) => !file.isDir)
        .filter((file) => {
          const path = file.path.toLowerCase();
          const name = file.name.toLowerCase();

          if (!query) return true;

          return name.includes(query) || path.includes(query);
        })
        .sort((a, b) => {
          const aName = a.name.toLowerCase();
          const bName = b.name.toLowerCase();

          // Prefer filename matches over path-only matches.
          const aNameMatch = aName.includes(query);
          const bNameMatch = bName.includes(query);

          if (aNameMatch !== bNameMatch) {
            return aNameMatch ? -1 : 1;
          }

          return a.path.localeCompare(b.path);
        })
        .slice(0, 50)
        .map((file) => ({
          id: file.id,
          path: file.path,
          name: file.name,
          relativePath: toRelativePath(file.path, props.rootPath()),
        }))
    );
  });

  /*
   * -----------------------------------------------------------------------
   * Initialization
   * -----------------------------------------------------------------------
   */

  onMount(async () => {
    try {
      const settings = await loadAiSettings();
      if (!isMounted) return;
      setProviders(settings.providers);
      setSelection(reconcileSelection(settings.providers, settings.active));
      setPermissions(settings.permissions);

      try {
        await store.loadFirstPage();
      } catch (err) {
        console.error('Failed to load chats:', err);
        setHistoryLoadError(String(err));
        return;
      }
      if (!isMounted) return;

      const first = store.state.list.items[0];
      if (first) await store.openChat(first);
      else createNewDialog();

      await recoverDraftsAfterRestart();
    } finally {
      if (isMounted) setLoadingHistory(false);
    }
  });

  /*
   * -----------------------------------------------------------------------
   * Cleanup
   * -----------------------------------------------------------------------
   */

  onCleanup(() => {
    isMounted = false;

    if (currentChannel) {
      currentChannel = null;
    }

    if (rafId !== null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  });

  /*
   * -----------------------------------------------------------------------
   * Persistence
   * -----------------------------------------------------------------------
   */

  async function flushDialogPersistence() {
    await store.flush();
  }

  onCleanup(() => {
    void store.flush();
  });

  /*
   * -----------------------------------------------------------------------
   * Dialog helpers
   * -----------------------------------------------------------------------
   */

  function resetComposerState() {
    clearChatInput(false);
    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
  }

  /** An open chat with nothing in it (and no request running). */
  function isEmptyChat(dialogId: string): boolean {
    const chat = store.state.open[dialogId];
    const summary = activeDialog();
    return (
      !!chat &&
      chat.messages.length === 0 &&
      !chat.hasOlder &&
      !chat.busy &&
      (summary?.id !== dialogId || summary.messageCount === 0)
    );
  }

  /**
   * "New chat" from the UI. When the current chat is still empty it is
   * reused instead of piling up empty chats in the history.
   */
  function startNewChat(): DialogSummary {
    const current = activeDialog();
    if (current && isEmptyChat(current.id)) {
      setHistoryOpen(false);
      setFocusMessageId(null);
      inputTextareaRef?.focus();
      return current;
    }
    resetComposerState();
    const dialog = createNewDialog();
    inputTextareaRef?.focus();
    return dialog;
  }

  function createNewDialog(): DialogSummary {
    const dialog = store.createDialog(NEW_CHAT_TITLE);
    setHistoryOpen(false);
    setFocusMessageId(null);
    // References belong to the current composer, not to the new dialog.
    setReferencedFiles([]);
    return dialog;
  }

  async function selectRow(row: ChatListRow) {
    setHistoryOpen(false);
    resetComposerState();
    try {
      if (row.messageId) {
        await store.openChatAround(row.dialog, row.messageId);
        setFocusMessageId(row.messageId);
      } else {
        setFocusMessageId(null);
        await store.openChat(row.dialog);
      }
    } catch (err) {
      console.error('Failed to open chat:', err);
    }
    restoreDraftForDialog(row.dialog.id);
  }

  async function confirmAndDeleteDialog(dialog: DialogSummary) {
    const count = dialog.messageCount;
    const confirmed = await confirmDialog({
      title: 'Delete this chat?',
      message: (
        <>
          <strong>{dialog.title || 'Untitled chat'}</strong>
          {count > 0 ? ` and its ${count} message${count === 1 ? '' : 's'}` : ''} will be
          permanently deleted.
        </>
      ),
      confirmLabel: 'Delete',
      danger: true,
    });
    if (confirmed) await deleteDialog(dialog.id);
  }

  async function deleteDialog(id: string) {
    const wasActive = activeDialogId() === id;
    takePendingDraft(id);
    // A deleted chat leaves search results right away.
    setSearchRows((rows) => rows?.filter((r) => r.dialog.id !== id) ?? null);
    await store.deleteDialog(id);

    if (wasActive) {
      const next = store.state.list.items[0];
      if (next) await store.openChat(next);
      else createNewDialog();
    }
  }

  /*
   * -----------------------------------------------------------------------
   * File autocomplete
   * -----------------------------------------------------------------------
   */

  function updateFileAutocomplete(value: string, cursorPosition: number) {
    const beforeCursor = value.slice(0, cursorPosition);

    const match = beforeCursor.match(/(^|\s)@([^\s@]*)$/);

    if (!match) {
      setFileSuggestionsOpen(false);
      setFileTriggerStart(null);
      setFileQuery('');
      return;
    }

    const query = match[2] ?? '';

    const triggerStart = beforeCursor.length - query.length - 1;

    setFileTriggerStart(triggerStart);
    setFileQuery(query);
    setSelectedSuggestionIndex(0);
    setFileSuggestionsOpen(true);
  }

  function escapeHtml(text: string): string {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
  }

  /** Builds the non-editable "file reference" chip used in the composer. */
  function createFileChip(suggestion: FileSuggestion): HTMLSpanElement {
    const chip = document.createElement('span');
    chip.className = 'chat-file-reference';
    chip.contentEditable = 'false';
    chip.dataset.path = suggestion.path;
    chip.innerHTML = `<span class="chat-file-reference-name" title="${escapeHtml(suggestion.path)}">${escapeHtml(suggestion.name)}</span>
    <button
      type="button"
      class="chat-file-reference-remove"
      aria-label="Remove ${suggestion.name}"
      title="Remove reference"
    >
      <svg
        width="24"
        height="24"
        viewBox="0 0 24 24"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        aria-hidden="true"
      >
        <path
          d="M6 6l12 12M18 6L6 18"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </button>`;

    chip.querySelector('.chat-file-reference-remove')?.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      removeReferencedFile(suggestion.path);
      chip.remove();
    });

    return chip;
  }

  function acceptFileSuggestion(suggestion: FileSuggestion) {
    const container = inputTextareaRef;
    if (!container) return;

    const triggerStart = fileTriggerStart();
    if (triggerStart === null) return;

    /*
     * Focus FIRST. Calling focus() after setting the selection can
     * collapse the caret back to the start of the element (WebKit does
     * this), which is the main reason the cursor never moved.
     */
    container.focus();

    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return;

    const startLocation = findTextOffset(container, triggerStart);
    if (!startLocation) return;

    /*
     * Delete from the '@' up to the caret.
     */
    const deleteRange = document.createRange();
    deleteRange.setStart(startLocation.node, startLocation.offset);
    deleteRange.setEnd(selection.focusNode ?? container, selection.focusOffset);
    deleteRange.deleteContents();

    /*
     * Collapse the selection to the deletion point so insertNode()
     * inserts exactly where '@query' used to be.
     */
    deleteRange.collapse(false);
    selection.removeAllRanges();
    selection.addRange(deleteRange);

    const chip = createFileChip(suggestion);

    deleteRange.insertNode(chip);

    /*
     * Insert a trailing space.
     */
    const space = document.createTextNode(' ');
    chip.after(space);

    /*
     * Position the caret AFTER the chip with a fresh, collapsed range.
     * Don't reuse the range we inserted with — its position after the
     * insertions is engine-dependent.
     */
    const caretRange = document.createRange();
    caretRange.setStartAfter(space);
    caretRange.collapse(true);
    selection.removeAllRanges();
    selection.addRange(caretRange);

    setInput(readComposerText());

    setReferencedFiles((current) =>
      current.some((file) => file.path === suggestion.path) ? current : [...current, suggestion]
    );

    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    /*
     * Keep the caret visible. Note: getClientRects() returns nothing for a
     * collapsed range, so use getBoundingClientRect() on the range instead.
     */
    setTimeout(() => {
      const caretRect = caretRange.getBoundingClientRect();
      const containerRect = container.getBoundingClientRect();

      if (caretRect.bottom > containerRect.bottom) {
        container.scrollTop += caretRect.bottom - containerRect.bottom;
      }
    }, 0);
  }

  function removeReferencedFile(path: string) {
    setReferencedFiles((current) => current.filter((file) => file.path !== path));

    requestAnimationFrame(() => {
      inputTextareaRef?.focus();
    });
  }

  function clearChatInput(focus = true) {
    setInput('');
    setReferencedFiles([]);

    if (inputTextareaRef) {
      inputTextareaRef.innerHTML = '';
      if (focus) inputTextareaRef.focus();
    }
  }

  /*
   * -----------------------------------------------------------------------
   * Pending drafts (see lib/pendingDraft.ts)
   * -----------------------------------------------------------------------
   */

  /** Reads the composer as ordered text + file-chip segments. */
  function serializeComposer(): DraftSegment[] {
    const segments: DraftSegment[] = [];
    const pushText = (text: string) => {
      if (!text) return;
      const last = segments[segments.length - 1];
      if (last?.type === 'text') last.text += text;
      else segments.push({ type: 'text', text });
    };

    const walk = (node: Node) => {
      node.childNodes.forEach((child, index) => {
        if (child.nodeType === Node.TEXT_NODE) {
          pushText(child.textContent ?? '');
          return;
        }
        if (!(child instanceof HTMLElement)) return;

        if (child.classList.contains('chat-file-reference')) {
          const path = child.dataset.path ?? '';
          const file = referencedFiles().find((f) => f.path === path);
          if (file) segments.push({ type: 'file', file });
          else pushText(child.textContent ?? '');
          return;
        }
        if (child.tagName === 'BR') {
          // A <br> that is the only/last thing in a line <div> is WebKit's
          // placeholder for an empty line; the <div> already counts as the break.
          const isLinePlaceholder =
            child.parentElement !== inputTextareaRef &&
            child.parentElement?.tagName === 'DIV' &&
            child.nextSibling === null;
          if (!isLinePlaceholder) pushText('\n');
          return;
        }
        // contenteditable wraps new lines in <div>s.
        if (child.tagName === 'DIV' && index > 0) pushText('\n');
        walk(child);
      });
    };

    if (inputTextareaRef) walk(inputTextareaRef);
    return segments;
  }

  /**
   * The composer's text with real line breaks (textContent drops the ones
   * WebKit makes out of <div>/<br>) and file chips as their names.
   */
  function readComposerText(): string {
    return serializeComposer()
      .map((segment) => (segment.type === 'text' ? segment.text : segment.file.name))
      .join('');
  }

  function handleComposerInput(target: HTMLElement) {
    /*
     * Once the composer is visually empty, drop whatever markup is left
     * (stray <br>, empty styled <span>s). Otherwise WebKit keeps applying the
     * last node's style to new text, and the :empty placeholder never shows.
     */
    if (!target.textContent && !target.querySelector('.chat-file-reference')) {
      target.innerHTML = '';
    }

    setInput(readComposerText());

    // Autocomplete works on raw text-node offsets, so keep using textContent here.
    const raw = target.textContent ?? '';
    const sel = window.getSelection();
    const caretOffset =
      sel && sel.focusNode ? getTextOffset(target, sel.focusNode, sel.focusOffset) : raw.length;

    updateFileAutocomplete(raw, caretOffset);
  }

  /**
   * Inserts text at the caret as plain text. execCommand('insertText') is
   * deprecated but still the only way to keep native undo (Ctrl+Z) working,
   * and it fires `input` like typing would. The manual path is a fallback.
   */
  function insertPlainText(text: string) {
    const container = inputTextareaRef;
    if (!container || !text) return;

    if (document.execCommand('insertText', false, text)) return;

    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return;

    const range = selection.getRangeAt(0);
    if (!container.contains(range.commonAncestorContainer)) return;

    range.deleteContents();
    const fragment = document.createDocumentFragment();
    text.split('\n').forEach((line, i) => {
      if (i > 0) fragment.append(document.createElement('br'));
      if (line) fragment.append(document.createTextNode(line));
    });
    const last = fragment.lastChild;
    range.insertNode(fragment);

    if (last) {
      range.setStartAfter(last);
      range.collapse(true);
      selection.removeAllRanges();
      selection.addRange(range);
    }

    handleComposerInput(container);
  }

  /** Clipboard text only: code copied from Monaco carries colored HTML. */
  function handleComposerPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData('text/plain');
    e.preventDefault();
    if (text) insertPlainText(text.replace(/\r\n?/g, '\n'));
  }

  /** Same for drag-and-drop of selected text. */
  function handleComposerDrop(e: DragEvent) {
    const text = e.dataTransfer?.getData('text/plain');
    if (!text) return;

    e.preventDefault();

    // Put the caret where the text was dropped before inserting.
    const range = document.caretRangeFromPoint?.(e.clientX, e.clientY);
    if (range) {
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    }

    insertPlainText(text.replace(/\r\n?/g, '\n'));
  }

  /**
   * Puts a draft back into the composer. Appends on a new line when the
   * user has already started typing something else, so nothing is lost.
   */
  function restoreDraftToComposer(draft: PendingDraft) {
    const container = inputTextareaRef;
    if (!container) return;

    const hasContent = (container.textContent ?? '').trim().length > 0;
    const fragment = document.createDocumentFragment();

    if (hasContent) fragment.append(document.createElement('br'));

    const files: FileSuggestion[] = [];
    draft.segments.forEach((segment, index) => {
      if (segment.type === 'text') {
        const lines = segment.text.split('\n');
        lines.forEach((line, i) => {
          if (i > 0) fragment.append(document.createElement('br'));
          if (line) fragment.append(document.createTextNode(line));
        });
      } else {
        fragment.append(createFileChip(segment.file));
        // Chips are followed by a space when inserted; keep exactly one.
        const next = draft.segments[index + 1];
        if (!(next?.type === 'text' && /^\s/.test(next.text))) {
          fragment.append(document.createTextNode(' '));
        }
        files.push(segment.file);
      }
    });

    container.append(fragment);

    setInput(readComposerText());
    setReferencedFiles((current) => [
      ...current,
      ...files.filter((f) => !current.some((c) => c.path === f.path)),
    ]);

    // Caret at the end, ready to edit and resend.
    container.focus();
    const range = document.createRange();
    range.selectNodeContents(container);
    range.collapse(false);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  function restoreDraftForDialog(dialogId: string) {
    const stored = listPendingDrafts().find((d) => d.dialogId === dialogId);
    if (!stored || stored.messageId === inFlightMessageId) return;

    const draft = takePendingDraft(dialogId);
    if (draft) queueMicrotask(() => restoreDraftToComposer(draft));
  }

  function markMessageFailed(dialogId: string, messageId: string) {
    void store.markFailed(dialogId, messageId);
  }

  /**
   * Drafts still in storage at startup were never answered: the app closed
   * or crashed mid-request. Mark their messages failed and bring the text
   * back for the dialog that's open.
   */
  async function recoverDraftsAfterRestart() {
    const drafts = listPendingDrafts();
    if (drafts.length === 0) return;

    let answered = new Set<string>();
    try {
      answered = new Set(await chatApi.messagesAnswered(drafts.map((d) => d.messageId)));
    } catch (err) {
      console.error('Draft recovery check failed:', err);
      return;
    }

    for (const draft of drafts) {
      if (answered.has(draft.messageId)) {
        // The answer made it into the database before the draft was cleared.
        clearPendingDraft(draft.dialogId, draft.messageId);
      } else {
        markMessageFailed(draft.dialogId, draft.messageId);
      }
    }

    const active = activeDialogId();
    if (active) restoreDraftForDialog(active);
  }

  function getTextOffset(root: Node, node: Node, nodeOffset: number): number {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let offset = 0;
    let current = walker.nextNode() as Text | null;

    while (current) {
      if (current === node) {
        return offset + Math.min(nodeOffset, current.data.length);
      }
      offset += current.data.length;
      current = walker.nextNode() as Text | null;
    }

    return offset;
  }

  function findTextOffset(root: Node, offset: number): { node: Text; offset: number } | null {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let remaining = offset;
    let node = walker.nextNode() as Text | null;

    while (node) {
      if (remaining <= node.data.length) {
        return { node, offset: remaining };
      }
      remaining -= node.data.length;
      node = walker.nextNode() as Text | null;
    }

    return null;
  }
  /*
   * -----------------------------------------------------------------------
   * AI settings
   * -----------------------------------------------------------------------
   */

  function persistAiSettings(
    nextProviders: Provider[],
    nextSelection: ModelSelection | null,
    nextPermissions: AgentPermissions = permissions()
  ) {
    void saveAiSettings({
      version: 2,
      providers: nextProviders,
      active: nextSelection,
      permissions: nextPermissions,
    }).catch((err) => console.error('Failed to save AI settings:', err));
  }

  function handleSettingsSaved(changes: { providers: Provider[]; permissions: AgentPermissions }) {
    const nextSelection = reconcileSelection(changes.providers, selection());
    setProviders(changes.providers);
    setSelection(nextSelection);
    setPermissions(changes.permissions);
    persistAiSettings(changes.providers, nextSelection, changes.permissions);
  }

  function handleModelChange(next: ModelSelection) {
    setSelection(next);
    persistAiSettings(providers(), next);
  }

  /*
   * -----------------------------------------------------------------------
   * Tool approval
   * -----------------------------------------------------------------------
   */

  /** The approval model, then its fallbacks, as far as they exist and are enabled. */
  function approvalProfiles(p: AgentPermissions): ModelProfile[] {
    const chosen: ModelSelection[] = [];
    for (const sel of [p.approvalModel, ...p.approvalFallbacks]) {
      if (sel && !chosen.some((c) => sameModel(c, sel))) chosen.push(sel);
    }
    // Only enabled models, as in the chat's model picker.
    const enabled = (sel: ModelSelection) =>
      providers()
        .find((pr) => pr.id === sel.providerId)
        ?.models.some((m) => m.id === sel.modelId && m.enabled) ?? false;
    return chosen
      .filter(enabled)
      .map((sel) => resolveProfile(providers(), sel))
      .filter((profile): profile is ModelProfile => !!profile);
  }

  /** The latest delivered user request in a chat, for the AI reviewer. */
  function latestUserRequest(dialogId: string): string {
    const list = store.messagesOf(dialogId);
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role === 'user' && !m.failed) return m.meta?.sentContent ?? m.content;
    }
    return '';
  }

  /**
   * Permission first (refused calls never reach approval), then approval
   * when the settings require it: by the user, or by the approval model.
   * Returns the refusal to send back to the model (null: run it) and the
   * decision to show on the tool block.
   */
  async function authorizeToolCall(
    dialogId: string,
    tc: ChatToolCall
  ): Promise<{
    refusal: string | null;
    approval?: ToolApproval;
    /** Partially accepted edit: the arguments to run instead. */
    argsOverride?: string;
    /** Added to the tool result for the model. */
    resultNote?: string;
  }> {
    const p = permissions();
    const verdict = checkToolCall(tc.name, tc.arguments, p);
    if (!verdict.permitted) {
      return { refusal: `Error: blocked by the user's permission settings. ${verdict.reason}` };
    }
    if (!verdict.approval) return { refusal: null };

    const summary = describeToolCall(tc.name, tc.arguments);
    const command = tc.name === 'run_command' ? toolArg(tc.arguments, 'command') : undefined;
    // Edits are diffed once: the AI reviewer reads the diff, the user
    // reviews it in the editor.
    const prepared = tc.name === 'write_file' ? await prepareEditReview(tc.arguments) : null;
    // Writing the same content changes nothing: no need to ask.
    if (prepared?.kind === 'unchanged') return { refusal: null };
    const edit = prepared?.kind === 'diff' ? prepared.review : null;
    let manualReason: string | null = null;

    if (verdict.approval === 'ai') {
      const models = approvalProfiles(p);
      const editPath = tc.name === 'write_file' ? toolArg(tc.arguments, 'path') : undefined;

      // Shapes no reviewer model may approve on its own (see permissions.ts).
      if (command !== undefined && isRiskyCommand(command)) {
        manualReason =
          'Commands that push, delete, deploy, escalate privileges, run inline scripts or touch secrets always need your approval.';
      } else if (editPath !== undefined && isSecretPath(editPath)) {
        manualReason = 'Edits to credential or secret files always need your approval.';
      } else if (models.length === 0) {
        manualReason = 'No AI reviewer model is set, so this needs your approval.';
      } else {
        setToolNote(tc.id, `Reviewing with ${models[0].label}…`);
        const primary = models[0];
        const review = await reviewWithAi(
          models,
          {
            group:
              tc.name === 'run_command' ? 'commands' : tc.name === 'write_file' ? 'edits' : 'tools',
            userRequest: latestUserRequest(dialogId),
            project: props.rootPath() ?? '',
            toolName: tc.name,
            summary,
            argsJson: tc.arguments,
            editPath,
            editDiff: edit ? compactDiff(edit.hunks) : undefined,
            isNewFile: prepared?.kind === 'new',
          },
          // The reviewer model follows the setting; a Jev fallback can only
          // be asked with typed input.
          (profile) => (profile === primary ? p.approvalTypedInput : isJevModel(profile.model))
        );
        setToolNote(tc.id);

        if (review.decided) {
          const approval: ToolApproval = {
            by: 'ai',
            approved: review.approved,
            model: review.model,
            reason: review.reason,
          };
          return review.approved
            ? { refusal: null, approval }
            : {
                refusal: `Error: rejected by the user's AI reviewer (${review.model}): ${review.reason} Do not retry the same action; adjust the approach or ask the user.`,
                approval,
              };
        }
        if (review.unsure) {
          manualReason = `${review.reason} This needs your approval.`;
        } else {
          console.warn('AI approval gave no decision:', review.errors);
          manualReason = 'The AI reviewer couldn’t answer, so this needs your approval.';
        }
      }
    }

    // Manual: the request shows in the chat (and, for an edit of an existing
    // file, in the editor as changes to accept or reject one by one).
    const note =
      [manualReason, prepared?.kind === 'new' ? 'New file: expand to see its content.' : null]
        .filter(Boolean)
        .join(' ') || undefined;

    const decision = await requestApproval({
      toolCallId: tc.id,
      dialogId,
      toolName: tc.name,
      summary,
      command,
      note,
      edit: edit ?? undefined,
    });

    if (decision.kind === 'allow') {
      return { refusal: null, approval: { by: 'user', approved: true } };
    }
    if (decision.kind === 'deny') {
      return {
        refusal: edit
          ? `Error: the user rejected every change to ${edit.relPath}; the file was not modified. Do not retry the same edit; ask the user or take another approach.`
          : `Error: the user denied this "${tc.name}" call. Do not retry it; continue without it or ask the user how to proceed.`,
        approval: { by: 'user', approved: false },
      };
    }

    // Some changes accepted: write the original with only those applied.
    const args = parseToolArgsObject(tc.arguments);
    const total = decision.accepted.length + decision.rejected.length;
    return {
      refusal: null,
      approval: {
        by: 'user',
        approved: true,
        reason: `Accepted ${decision.accepted.length} of ${total} changes.`,
      },
      argsOverride: JSON.stringify({ ...args, content: decision.content }),
      resultNote: describeRejectedHunks(edit?.relPath ?? 'the file', decision.rejected, total),
    };
  }

  /** One string argument of a tool call. */
  function toolArg(argsJson: string, key: string): string | undefined {
    const value = parseToolArgsObject(argsJson)[key];
    return typeof value === 'string' ? value : undefined;
  }

  function parseToolArgsObject(argsJson: string): Record<string, unknown> {
    try {
      const parsed = argsJson.trim() ? JSON.parse(argsJson) : {};
      return parsed && typeof parsed === 'object' ? parsed : {};
    } catch {
      return {};
    }
  }

  /**
   * The diff of a write_file call against the file on disk. A new file has
   * nothing to compare against: it's approved as a whole.
   */
  async function prepareEditReview(
    argsJson: string
  ): Promise<
    { kind: 'diff'; review: EditReview } | { kind: 'new' } | { kind: 'unchanged' } | null
  > {
    const root = props.rootPath();
    const path = toolArg(argsJson, 'path');
    const content = toolArg(argsJson, 'content');
    if (!root || !path || content === undefined) return null;
    const absPath = toAbsolutePath(path, root);
    let current: string;
    try {
      current = await readFileContent(absPath);
    } catch {
      return { kind: 'new' };
    }
    const review = buildEditReview(absPath, path, current, content);
    return review ? { kind: 'diff', review } : { kind: 'unchanged' };
  }

  /** Tells the model which of its changes were not applied. */
  function describeRejectedHunks(path: string, rejected: ReviewHunk[], total: number): string {
    const preview = (lines: string[]) => {
      const shown = lines.slice(0, 3).map((l) => `    ${l}`);
      if (lines.length > 3) shown.push(`    … (${lines.length - 3} more lines)`);
      return shown.join('\n');
    };
    const items = rejected.map((h) => {
      const where = `around original line ${h.origStart}`;
      if (h.added.length === 0)
        return `- ${where}: deleting these lines (kept):\n${preview(h.deleted)}`;
      if (h.deleted.length === 0)
        return `- ${where}: inserting these lines (not added):\n${preview(h.added)}`;
      return `- ${where}: replacing\n${preview(h.deleted)}\n  with (not applied):\n${preview(h.added)}`;
    });
    return [
      '',
      `The user reviewed this edit and accepted ${total - rejected.length} of ${total} changes; the file now has only those. Rejected changes:`,
      ...items,
      `Read ${path} again before editing it further, and don't redo the rejected changes unless the user asks.`,
    ].join('\n');
  }

  /*
   * -----------------------------------------------------------------------
   * Title generation
   * -----------------------------------------------------------------------
   */

  createEffect(
    on(
      () => [activeDialog()?.id, activeDialog()?.title, messages().length] as const,
      ([dialogId, title]) => {
        const profile = activeProfile();
        if (!dialogId || !profile || title !== NEW_CHAT_TITLE) return;
        if (titlesInFlight.has(dialogId)) return;
        if (!messages().some((m) => m.role === 'user' && !m.failed && m.content.trim())) return;

        titlesInFlight.add(dialogId);
        // Runs alongside the answer; independent of the conversation.
        void generateTitleForDialog(dialogId, profile)
          .catch((err) => console.error('Chat title generation failed:', err))
          .finally(() => titlesInFlight.delete(dialogId));
      }
    )
  );

  async function generateTitleForDialog(dialogId: string, profile: ModelProfile) {
    const chatMessages = store
      .messagesOf(dialogId)
      .filter(
        (msg) =>
          (msg.role === 'user' || msg.role === 'assistant') &&
          !msg.toolCalls?.length &&
          !msg.error &&
          !msg.failed &&
          !msg.pending &&
          msg.content.trim()
      )
      .slice(0, 6);

    if (!chatMessages.some((m) => m.role === 'user')) return;

    const title = await generateChatTitle(profile, chatMessages);
    if (!title || !isMounted) return;

    // The user may have renamed it meanwhile; only replace the placeholder.
    const current =
      store.state.active?.id === dialogId
        ? store.state.active
        : store.state.list.items.find((d) => d.id === dialogId);
    if (current && current.title !== NEW_CHAT_TITLE) return;

    await store.renameDialog(dialogId, title);
  }

  /*
   * -----------------------------------------------------------------------
   * Agent
   * -----------------------------------------------------------------------
   */

  /** Wait this long without any output before saying so. */
  const STILL_WAITING_MS = 15_000;

  /**
   * One request to the model. The backend reads the chat's history from the
   * database, so everything up to this point must already be stored. It
   * also retries failed attempts on its own (src-tauri/src/chat_request.rs),
   * reporting each retry, then succeeds or fails for good.
   */
  async function runAgentTurn(
    dialogId: string,
    opts: {
      /** The request's user message as sent (with editor context). */
      currentUserContent?: string;
      step?: number;
      /** Ask the model to continue its interrupted last reply. */
      continueReply?: boolean;
      /**
       * Called once the first turn's outcome is known: true when the model
       * answered (fully or partly), false when nothing came back.
       */
      onSettled?: (ok: boolean) => void;
    } = {}
  ): Promise<void> {
    const step = opts.step ?? 0;
    const profile = activeProfile();

    if (!profile || !isMounted || stopRequested) {
      opts.onSettled?.(false);
      return;
    }

    const pendingMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'assistant',
      content: '',
      pending: true,
      finalized: false,
    };
    // Unfinished placeholders are skipped when the history is built.
    await store.appendMessages(dialogId, [pendingMsg]);

    const channel = new Channel<ChatStreamEvent>();
    const requestId = crypto.randomUUID();
    currentRequestId = requestId;

    let assistantText = '';
    const toolCallsMap = new Map<string, ChatToolCall>();
    let pendingRender = false;
    let failure: ChatError | null = null;
    let gotOutput = false;
    /** Set once we stop listening to this request (Stop wasn't confirmed). */
    let detached = false;
    // The command can return before its last events (see untilStreamEnd).
    const stream = streamEndSignal();

    const setStatus = (status: ChatMessage['status']) =>
      store.patchMessage(dialogId, pendingMsg.id, { status });

    // "Still waiting" once a while has passed without any output.
    const waitingTimer = window.setTimeout(() => {
      if (!gotOutput && !stopRequested && isMounted)
        setStatus({ text: 'Still waiting for a response…' });
    }, STILL_WAITING_MS);

    const markOutput = () => {
      if (gotOutput) return;
      gotOutput = true;
      window.clearTimeout(waitingTimer);
      setStatus(undefined);
    };

    // One store update per animation frame, and a checkpoint every couple
    // of seconds so a crash mid-reply loses little.
    const flushAssistantText = () => {
      pendingRender = false;
      if (!isMounted) return;
      store.patchMessage(dialogId, pendingMsg.id, { content: assistantText });
      store.checkpoint(dialogId, pendingMsg.id);
    };

    const publishToolCalls = () => {
      store.patchMessage(dialogId, pendingMsg.id, {
        toolCalls: Array.from(toolCallsMap.values(), (tc) => ({ ...tc })),
      });
    };

    channel.onmessage = (event) => {
      if (!isMounted || detached) return;

      switch (event.type) {
        case 'Delta':
          markOutput();
          assistantText += event.content;
          if (!pendingRender) {
            pendingRender = true;
            rafId = requestAnimationFrame(() => {
              rafId = null;
              flushAssistantText();
            });
          }
          break;

        case 'Done':
          stream.end();
          break;

        case 'Retrying':
          window.clearTimeout(waitingTimer);
          if (stopRequested) break;
          setStatus({
            text: `${retryReason(event.reason)} Retrying (attempt ${event.attempt} of ${event.max_attempts})`,
            until: Date.now() + event.delay_ms,
          });
          break;

        case 'Error':
          failure = event.error;
          stream.end();
          break;

        case 'ToolCallStart':
          markOutput();
          toolCallsMap.set(event.id, { id: event.id, name: event.name, arguments: '' });
          publishToolCalls();
          break;

        case 'ToolCallArgumentsDelta': {
          const tc = toolCallsMap.get(event.id);
          if (tc) {
            tc.arguments += event.arguments_delta;
            publishToolCalls();
          }
          break;
        }

        case 'ToolCallComplete':
          toolCallsMap.set(event.id, {
            id: event.id,
            name: event.name,
            arguments: event.arguments,
            // Sent back unchanged in the next request (Gemini 3 requires it).
            ...(event.extra_content !== undefined ? { extraContent: event.extra_content } : {}),
          });
          publishToolCalls();
          break;
      }
    };

    const root = props.rootPath();

    // File tools need an open project; web tools are always available, the
    // screenshot tool only for vision models, and none for "no tools" models.
    const tools = agentToolsFor({
      hasProject: !!root,
      vision: profile.vision,
      noTools: profile.noTools,
      permissions: permissions(),
    });

    let abandon!: () => void;
    const abandoned = new Promise<'abandoned'>((resolve) => {
      abandon = () => resolve('abandoned');
    });
    abandonCurrent = abandon;
    currentPending = { dialogId, messageId: pendingMsg.id };

    try {
      if (stopRequested) {
        // Stop was pressed while this turn was being prepared: don't send.
        failure = stoppedError();
      } else {
        const outcome = await Promise.race([
          sendChatMessage({
            profile,
            dialogId,
            currentUserContent: opts.currentUserContent,
            continueReply: opts.continueReply,
            assistantMessageId: pendingMsg.id,
            requestId,
            channel,
            tools,
          })
            .then(() => untilStreamEnd(stream.ended))
            .then(() => 'done' as const),
          abandoned,
        ]);
        if (outcome === 'abandoned') {
          // The backend didn't confirm the stop in time. Stop waiting for it
          // and ignore anything this request still sends.
          detached = true;
          failure = stoppedError();
        }
      }
    } catch (err) {
      // Only problems before the request (e.g. building the history) land here.
      failure = {
        kind: 'provider',
        message: String(err),
        retryable: false,
        retryAfterMs: null,
        attempts: 0,
        status: null,
        details: String(err),
        partial: false,
      };
    } finally {
      window.clearTimeout(waitingTimer);
      if (abandonCurrent === abandon) abandonCurrent = null;
      if (currentPending?.messageId === pendingMsg.id) currentPending = null;
      if (currentRequestId === requestId) currentRequestId = null;
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
      setStatus(undefined);
    }

    const toolCalls = Array.from(toolCallsMap.values());
    const error = failure as ChatError | null;

    if (error) {
      const stored = toStoredFailure(error);
      if (assistantText.trim()) {
        // Part of the reply arrived: keep it (it stays in the history) and
        // let the user continue. Incomplete tool calls are dropped.
        await store.finalizeMessage(dialogId, pendingMsg.id, {
          content: assistantText,
          toolCalls: [],
          meta: { interrupted: stored },
        });
        opts.onSettled?.(true);
      } else {
        await store.finalizeMessage(dialogId, pendingMsg.id, {
          content: '',
          toolCalls: [],
          error: describeFailure(stored),
          meta: { failure: stored },
        });
        opts.onSettled?.(false);
      }
      return;
    }

    await store.finalizeMessage(dialogId, pendingMsg.id, { content: assistantText, toolCalls });
    opts.onSettled?.(true);

    // Normal final answer, or the user pressed Stop.
    if (toolCalls.length === 0 || stopRequested) return;

    // Safety limit.
    if (step >= MAX_AGENT_STEPS) {
      await store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'assistant',
          content: '',
          error: `Stopped after ${MAX_AGENT_STEPS} tool-call steps to avoid a runaway loop.`,
          finalized: true,
        },
      ]);
      return;
    }

    // Execute requested tools; each result is stored before the next turn,
    // which reads the history from the database.
    // Every requested call gets a result, even after Stop: the provider
    // rejects a history with tool calls left unanswered.
    const stoppedResult = (tc: ChatToolCall) =>
      store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'tool',
          content: 'Error: stopped by the user before this ran.',
          tool_call_id: tc.id,
          finalized: true,
        },
      ]);

    for (const tc of toolCalls) {
      if (stopRequested) {
        await stoppedResult(tc);
        continue;
      }
      const { refusal, approval, argsOverride, resultNote } = await authorizeToolCall(dialogId, tc);
      // Stop may have been pressed while approval was pending.
      if (stopRequested && (refusal || !approval)) {
        await stoppedResult(tc);
        continue;
      }
      const args = argsOverride ?? tc.arguments;
      const result = refusal
        ? { content: refusal, attachments: undefined }
        : await executeAgentTool(root, tc.name, args);
      if (resultNote && !result.content.startsWith('Error:')) result.content += resultNote;

      // Open tabs of the file show what was written.
      if (tc.name === 'write_file' && !refusal && root && !result.content.startsWith('Error:')) {
        const path = toolArg(args, 'path');
        const content = toolArg(args, 'content');
        if (path && content !== undefined) notifyFileWritten(toAbsolutePath(path, root), content);
      }

      const resultMeta = toolResultMeta(tc.name, result.content);
      await store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'tool',
          content: result.content,
          attachments: result.attachments,
          tool_call_id: tc.id,
          meta: approval ? { ...resultMeta, approval } : resultMeta,
          finalized: true,
        },
      ]);
    }

    if (stopRequested) return;
    await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, step: step + 1 });
  }

  /**
   * Runs an agent request with the shared "sending" state: the send button
   * becomes Stop, the chat stays open in memory, writes are flushed after.
   */
  async function runRequest(dialogId: string, run: () => Promise<void>) {
    stopRequested = false;
    window.clearTimeout(stopTimer);
    setSending(true);
    store.setBusy(dialogId, true);
    try {
      await run();
    } finally {
      window.clearTimeout(stopTimer);
      store.setBusy(dialogId, false);
      if (isMounted) setSending(false);
      await flushDialogPersistence();
    }
  }

  /** Stop: cancels the running request and any further tool steps. */
  function stopRequest() {
    stopRequested = true;
    // Waiting approvals count as denied.
    denyAll();
    if (currentRequestId) {
      void cancelChatRequest(currentRequestId).catch((err) => console.error('Cancel failed:', err));
    }
    if (currentPending) {
      store.patchMessage(currentPending.dialogId, currentPending.messageId, {
        status: { text: 'Stopping…' },
      });
    }
    // Normally the request ends within a moment. If the backend doesn't
    // confirm, stop waiting for it anyway: Stop must always work.
    window.clearTimeout(stopTimer);
    stopTimer = window.setTimeout(() => abandonCurrent?.(), STOP_GRACE_MS);
  }

  /**
   * The latest delivered user message as it was sent to the model, with its
   * editor context and file references. The backend puts it in place of the
   * stored (clean) text, exactly as on the first attempt.
   */
  function lastUserWireContent(dialogId: string): string | undefined {
    const list = store.messagesOf(dialogId);
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role !== 'user' || m.failed) continue;
      if (m.meta?.sentContent) return m.meta.sentContent;
      // Sent before the wire copy was stored: rebuild it from the current editor.
      return buildWireContent(m.content, []);
    }
    return undefined;
  }

  /** Retry after a failed turn (the user's message was delivered earlier). */
  function retryLastTurn() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    listController?.followLatest();
    const currentUserContent = lastUserWireContent(dialogId);
    void runRequest(dialogId, () => runAgentTurn(dialogId, { currentUserContent }));
  }

  /** Continue an interrupted reply. */
  function continueReply() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    listController?.followLatest();
    const currentUserContent = lastUserWireContent(dialogId);
    void runRequest(dialogId, () =>
      runAgentTurn(dialogId, { continueReply: true, currentUserContent })
    );
  }

  /*
   * -----------------------------------------------------------------------
   * Sending a user message
   * -----------------------------------------------------------------------
   */

  /**
   * The user message as sent to the model: active editor context and
   * explicit file references first, then the text. Undefined when there is
   * nothing at all (the backend then sends the stored text).
   */
  function buildWireContent(text: string, files: FileSuggestion[]): string | undefined {
    const contextParts: string[] = [];

    const contextHeader = buildContextHeader(props.activeFileContext());
    if (contextHeader) contextParts.push(contextHeader);

    if (files.length > 0) {
      contextParts.push(
        [
          'The user explicitly referenced these project files:',
          ...files.map((file) => `- ${file.path}`),
          '',
          'Treat these as explicit file references from the user. Use the available file tools to inspect them when relevant.',
        ].join('\n')
      );
    }

    // A message of only file references still gets the reference context.
    const parts = [
      ...(contextParts.length ? [contextParts.join('\n\n')] : []),
      ...(text ? [text] : []),
    ];
    return parts.length > 0 ? parts.join('\n\n') : undefined;
  }

  async function handleSend() {
    const text = input().trim();
    const profile = activeProfile();
    const dialog = activeDialog();
    const selectedFiles = referencedFiles();

    /*
     * A message containing only referenced files is valid.
     *
     * For example:
     *
     *   [ChatPanel.tsx]
     *
     * can be sent without requiring additional text.
     */
    if (!profile || !dialog || sending() || (!text && selectedFiles.length === 0)) {
      return;
    }

    /*
     * The displayed/persisted text stays clean. The wire copy, with the
     * active editor context and file references, is what the model gets; it
     * is stored in the message's meta so a retry or "continue" later sends
     * the same thing instead of the bare text.
     */
    const wireContent = buildWireContent(text, selectedFiles);

    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
      finalized: true,
      ...(wireContent && wireContent !== text ? { meta: { sentContent: wireContent } } : {}),
    };

    /*
     * Save the composer before anything else: from here until the model
     * answers, this is the only copy of what the user typed.
     */
    savePendingDraft({
      dialogId: dialog.id,
      messageId: userMsg.id,
      segments: serializeComposer(),
      createdAt: new Date().toISOString(),
    });
    inFlightMessageId = userMsg.id;

    // Sending always brings the conversation back into view.
    listController?.followLatest();
    setFocusMessageId(null);

    // Clear right away; the draft brings it back if the request fails.
    clearChatInput();
    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    await runRequest(dialog.id, async () => {
      try {
        // Stored first: the backend builds the request from the database.
        await store.appendMessages(dialog.id, [userMsg]);

        await runAgentTurn(dialog.id, {
          currentUserContent: wireContent,
          onSettled: (ok) => {
            inFlightMessageId = null;

            if (ok) {
              clearPendingDraft(dialog.id, userMsg.id);
              return;
            }

            markMessageFailed(dialog.id, userMsg.id);

            // Restore now if the user is still in this dialog; otherwise the
            // draft waits in storage until they switch back to it.
            if (isMounted && activeDialogId() === dialog.id) {
              restoreDraftForDialog(dialog.id);
            }
          },
        });
      } finally {
        inFlightMessageId = null;
      }
    });
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (fileSuggestionsOpen()) {
      const suggestions = fileSuggestions();

      if (e.key === 'ArrowDown') {
        e.preventDefault();

        if (suggestions.length > 0) {
          setSelectedSuggestionIndex((index) => Math.min(index + 1, suggestions.length - 1));
        }

        return;
      }

      if (e.key === 'ArrowUp') {
        e.preventDefault();

        if (suggestions.length > 0) {
          setSelectedSuggestionIndex((index) => Math.max(index - 1, 0));
        }

        return;
      }

      if (e.key === 'Enter' && !e.shiftKey) {
        if (suggestions.length > 0) {
          e.preventDefault();

          const suggestion = suggestions[selectedSuggestionIndex()];

          if (suggestion) {
            acceptFileSuggestion(suggestion);
          }

          return;
        }
      }

      if (e.key === 'Escape') {
        e.preventDefault();

        setFileSuggestionsOpen(false);
        setFileTriggerStart(null);
        setFileQuery('');
        setSelectedSuggestionIndex(0);

        return;
      }
    }

    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  }

  const title = createMemo(() => {
    const dialog = activeDialog();

    if (!dialog) {
      return activeProfile() ? `AI Chat — ${activeProfile()!.label}` : 'AI Chat';
    }

    return dialog.title;
  });

  /*
   * -----------------------------------------------------------------------
   * Render
   * -----------------------------------------------------------------------
   */

  return (
    <div class="chat-panel">
      <div class="chat-header">
        <button
          class="chat-history-button"
          onClick={() => setHistoryOpen((value) => !value)}
          title="Chat history"
        >
          ☰
        </button>

        <span class="chat-title" title={title()}>
          {title()}
        </span>

        <div class="chat-controls">
          <button class="icon-button" onClick={() => startNewChat()} title="New chat">
            ＋
          </button>

          <button
            class="icon-button"
            onClick={() => setSettingsPage('menu')}
            title="AI Chat settings"
            aria-label="AI Chat settings"
          >
            ⚙
          </button>
        </div>
      </div>
      <Show when={historyOpen()}>
        <div class="chat-history">
          <div class="chat-history-header">
            <span>Chats</span>

            <button class="chat-new-button" onClick={() => startNewChat()}>
              New chat
            </button>
          </div>

          <ChatList
            rows={historyRows()}
            activeId={activeDialogId()}
            query={historyQuery()}
            onQueryChange={setHistoryQuery}
            noMatches={!!searchQuery().trim() && searchRows()?.length === 0}
            loading={loadingHistory() || (store.state.list.loading && !store.state.list.loaded)}
            onSelect={(row) => void selectRow(row)}
            onDelete={(dialog) => void confirmAndDeleteDialog(dialog)}
            onNearEnd={() => {
              if (!searchQuery().trim()) void store.loadMoreDialogs();
            }}
          />
        </div>
      </Show>
      <Show when={historyLoadError() ?? store.state.error}>
        {(problem) => (
          <div class="chat-history-error" role="alert" title={problem()}>
            {historyLoadError()
              ? "Chat history couldn't be loaded. Restart the app to try again."
              : "A chat change couldn't be saved."}
            <Show when={!historyLoadError()}>
              <button class="link-button" onClick={() => store.clearError()}>
                Dismiss
              </button>
            </Show>
          </div>
        )}
      </Show>
      <Show when={!props.rootPath()}>
        <div class="chat-tools-hint">Open a folder to let the model read and edit your files.</div>
      </Show>
      <MessageList
        dialogId={activeDialogId()}
        chat={activeChat()}
        sending={sending()}
        focusMessageId={focusMessageId()}
        onLoadOlder={() =>
          activeDialogId() ? store.loadOlder(activeDialogId()!) : Promise.resolve(0)
        }
        onLoadNewer={() =>
          activeDialogId() ? store.loadNewer(activeDialogId()!) : Promise.resolve(0)
        }
        onReloadLatest={async () => {
          const active = activeDialog();
          setFocusMessageId(null);
          if (active) await store.openChat(active, { reload: true });
        }}
        controller={(c) => (listController = c)}
        toolNotes={toolNotes()}
        onRetry={retryLastTurn}
        onContinue={continueReply}
      />
      <div class="chat-input-row">
        <div class="chat-input-container">
          <div
            ref={inputTextareaRef as unknown as HTMLDivElement}
            class="chat-input-contenteditable"
            contenteditable
            onInput={(e) => handleComposerInput(e.currentTarget)}
            onPaste={handleComposerPaste}
            onDrop={handleComposerDrop}
            onKeyDown={handleKeyDown}
            data-placeholder={
              activeProfile() ? 'Message the model...' : 'Add a model in settings first'
            }
          />

          <Show when={fileSuggestionsOpen() && fileSuggestions().length > 0}>
            <div class="file-suggestions" role="listbox">
              <For each={fileSuggestions()}>
                {(suggestion, index) => (
                  <button
                    type="button"
                    class="file-suggestion"
                    classList={{
                      selected: index() === selectedSuggestionIndex(),
                    }}
                    role="option"
                    aria-selected={index() === selectedSuggestionIndex()}
                    onMouseDown={(e) => {
                      e.preventDefault();
                    }}
                    onClick={() => acceptFileSuggestion(suggestion)}
                  >
                    <span class="file-suggestion-name">{suggestion.name}</span>
                    <span class="file-suggestion-path">./{suggestion.relativePath}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>

        <Show
          when={sending()}
          fallback={
            <button
              onClick={() => void handleSend()}
              disabled={
                !activeProfile() ||
                !activeDialog() ||
                (!input().trim() && referencedFiles().length === 0)
              }
              class="chat-send-button"
              title="Send"
              aria-label="Send message"
            >
              <svg
                viewBox="0 0 24 24"
                width="14"
                height="14"
                fill="currentColor"
                aria-hidden="true"
              >
                <path d="M2.01 21 23 12 2.01 3 2 10l15 2-15 2z" />
              </svg>
            </button>
          }
        >
          <button
            onClick={stopRequest}
            class="chat-send-button stop"
            title="Stop"
            aria-label="Stop the response"
          >
            <svg viewBox="0 0 24 24" width="12" height="12" fill="currentColor" aria-hidden="true">
              <rect x="5" y="5" width="14" height="14" rx="2" />
            </svg>
          </button>
        </Show>
      </div>
      <div class="chat-model-select">
        <ModelPicker
          providers={providers()}
          selection={selection()}
          onSelect={handleModelChange}
          onOpenSettings={() => setSettingsPage('providers')}
        />
      </div>
      <Show when={settingsPage()} keyed>
        {(page) => (
          <AiSettingsModal
            providers={providers()}
            permissions={permissions()}
            initialPage={page}
            onSave={handleSettingsSaved}
            onClose={() => setSettingsPage(null)}
          />
        )}
      </Show>
    </div>
  );
}
