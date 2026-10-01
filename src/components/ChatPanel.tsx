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
  ChatToolCall,
  DialogSummary,
  FileSuggestion,
  ModelProfile,
  ModelSelection,
  Provider,
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
import { sendChatMessage } from '../lib/aiChat';
import { agentToolsFor, executeAgentTool } from '../lib/tools';
import ModelSettingsPanel from './ModelSettingsPanel';
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
import { toRelativePath } from '../lib/fileTools';

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

  // Chat list page + open chats; everything else stays in the database.
  const store = createChatStore();
  const activeDialogId = () => store.state.activeId;

  const [input, setInput] = createSignal('');
  const [settingsOpen, setSettingsOpen] = createSignal(false);
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

  const [isGeneratingTitle, setIsGeneratingTitle] = createSignal(false);

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

  function persistAiSettings(nextProviders: Provider[], nextSelection: ModelSelection | null) {
    void saveAiSettings({ version: 2, providers: nextProviders, active: nextSelection });
  }

  function handleProvidersSaved(nextProviders: Provider[]) {
    const nextSelection = reconcileSelection(nextProviders, selection());
    setProviders(nextProviders);
    setSelection(nextSelection);
    persistAiSettings(nextProviders, nextSelection);
  }

  function handleModelChange(next: ModelSelection) {
    setSelection(next);
    persistAiSettings(providers(), next);
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
        if (!dialogId || !profile || title !== NEW_CHAT_TITLE || isGeneratingTitle()) return;
        if (!messages().some((m) => m.role === 'user' && m.content.trim())) return;

        setIsGeneratingTitle(true);
        // Runs alongside the answer; independent of the conversation.
        void generateTitleForDialog(dialogId, profile)
          .catch(() => {})
          .finally(() => setIsGeneratingTitle(false));
      }
    )
  );

  async function generateTitleForDialog(dialogId: string, profile: ModelProfile) {
    const chatMessages = store
      .messagesOf(dialogId)
      .filter((msg) => !msg.toolCalls?.length && !msg.tool_call_id && !msg.error && !msg.pending)
      .slice(0, 6);

    if (chatMessages.length === 0) return;

    const title = await generateChatTitle(profile, chatMessages);
    if (!title || !isMounted) return;

    await store.renameDialog(dialogId, title);
  }

  /*
   * -----------------------------------------------------------------------
   * Agent
   * -----------------------------------------------------------------------
   */

  /**
   * One request to the model. The backend reads the chat's history from the
   * database, so everything up to this point must already be stored.
   *
   * @param currentUserContent The request's user message as sent to the
   *   model (with editor context); stored messages keep the clean text.
   * @param onSettled Called once this turn's outcome is known: true when the
   *   model answered without error (text or tool calls), false otherwise.
   *   Only the first turn passes it; later tool-loop steps don't affect
   *   whether the user's message was delivered.
   */
  async function runAgentTurn(
    dialogId: string,
    currentUserContent: string | undefined,
    step: number,
    onSettled?: (ok: boolean) => void
  ): Promise<void> {
    const profile = activeProfile();

    if (!profile || !isMounted) {
      onSettled?.(false);
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
    currentChannel = channel;

    let assistantText = '';
    const toolCallsMap = new Map<string, ChatToolCall>();
    let pendingRender = false;
    let streamError: string | null = null;

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
      if (!isMounted) return;

      switch (event.type) {
        case 'Delta':
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
          break;

        case 'Error':
          streamError = event.message;
          break;

        case 'ToolCallStart':
          toolCallsMap.set(event.id, { id: event.id, name: event.name, arguments: '' });
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
          });
          publishToolCalls();
          break;
      }
    };

    const root = props.rootPath();

    // File tools need an open project; web tools are always available, and
    // the screenshot tool only for models marked as accepting images.
    const tools = agentToolsFor({ hasProject: !!root, vision: profile.vision });

    try {
      await sendChatMessage({ profile, dialogId, currentUserContent, channel, tools });
    } catch (err) {
      streamError = String(err);
    } finally {
      if (currentChannel === channel) currentChannel = null;
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
    }

    const toolCalls = Array.from(toolCallsMap.values());

    if (streamError) {
      await store.finalizeMessage(dialogId, pendingMsg.id, {
        content: assistantText,
        error: streamError,
      });
      onSettled?.(false);
      return;
    }

    await store.finalizeMessage(dialogId, pendingMsg.id, { content: assistantText, toolCalls });
    onSettled?.(true);

    // Normal final answer.
    if (toolCalls.length === 0) return;

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
    for (const tc of toolCalls) {
      const result = await executeAgentTool(root, tc.name, tc.arguments);
      await store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'tool',
          content: result.content,
          attachments: result.attachments,
          tool_call_id: tc.id,
          meta: toolResultMeta(tc.name, result.content),
          finalized: true,
        },
      ]);
    }

    await runAgentTurn(dialogId, currentUserContent, step + 1);
  }

  /*
   * -----------------------------------------------------------------------
   * Sending a user message
   * -----------------------------------------------------------------------
   */

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

    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
      finalized: true,
    };

    /*
     * The displayed/persisted user message stays clean.
     *
     * The selected file references and active editor context are only
     * added to the wire copy sent to the model.
     */
    const contextParts: string[] = [];

    const contextHeader = buildContextHeader(props.activeFileContext());

    if (contextHeader) {
      contextParts.push(contextHeader);
    }

    if (selectedFiles.length > 0) {
      contextParts.push(
        [
          'The user explicitly referenced these project files:',
          ...selectedFiles.map((file) => `- ${file.path}`),
          '',
          'Treat these as explicit file references from the user. Use the available file tools to inspect them when relevant.',
        ].join('\n')
      );
    }

    /*
     * If the user sends only file references, provide a small wire-level
     * description instead of sending an empty user message to the model.
     *
     * The visible/persisted message remains empty in that case, but the
     * actual model message contains the reference context.
     */
    const wireContentParts: string[] = [];

    if (contextParts.length > 0) {
      wireContentParts.push(contextParts.join('\n\n'));
    }

    if (text) {
      wireContentParts.push(text);
    }

    const wireContent = wireContentParts.length > 0 ? wireContentParts.join('\n\n') : undefined;

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

    setSending(true);
    store.setBusy(dialog.id, true);

    try {
      // Stored first: the backend builds the request from the database.
      await store.appendMessages(dialog.id, [userMsg]);

      await runAgentTurn(dialog.id, wireContent, 0, (ok) => {
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
      });
    } finally {
      inFlightMessageId = null;
      store.setBusy(dialog.id, false);
      if (isMounted) setSending(false);
      await flushDialogPersistence();
    }
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
          <button class="icon-button" onClick={() => createNewDialog()} title="New chat">
            ＋
          </button>

          <button class="icon-button" onClick={() => setSettingsOpen(true)} title="Model settings">
            ⚙
          </button>
        </div>
      </div>
      <Show when={historyOpen()}>
        <div class="chat-history">
          <div class="chat-history-header">
            <span>Chats</span>

            <button class="chat-new-button" onClick={() => createNewDialog()}>
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

        <button
          onClick={() => void handleSend()}
          disabled={
            !activeProfile() ||
            !activeDialog() ||
            sending() ||
            (!input().trim() && referencedFiles().length === 0)
          }
          class="chat-send-button"
        >
          <svg xmlns="http://w3.org" viewBox="0 0 24 24" width="14" height="14" fill="currentColor">
            <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
          </svg>
        </button>
      </div>
      <div class="chat-model-select">
        <ModelPicker
          providers={providers()}
          selection={selection()}
          onSelect={handleModelChange}
          onOpenSettings={() => setSettingsOpen(true)}
        />
      </div>
      <Show when={settingsOpen()}>
        <ModelSettingsPanel
          providers={providers()}
          onSave={handleProvidersSaved}
          onClose={() => setSettingsOpen(false)}
        />
      </Show>
    </div>
  );
}
