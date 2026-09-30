import {
  Accessor,
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import { Channel } from '@tauri-apps/api/core';
import type {
  ChatDialog,
  ChatMessage,
  ChatStreamEvent,
  ChatToolCall,
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
import { loadChatHistory, saveChatHistory } from '../lib/chatHistory';
import { generateChatTitle } from '../lib/chatTitle';
import { sendChatMessage } from '../lib/aiChat';
import { AGENT_TOOL_SCHEMAS, executeAgentTool } from '../lib/tools';
import ModelSettingsPanel from './ModelSettingsPanel';
import ModelPicker from './ModelPicker';
import ToolCallBlock from './ToolCallBlock';
import { isBlank } from '../lib/textUtils';
import {
  clearPendingDraft,
  listPendingDrafts,
  prunePendingDrafts,
  savePendingDraft,
  takePendingDraft,
  type DraftSegment,
  type PendingDraft,
} from '../lib/pendingDraft';
import MessageContent from './MessageContent';
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

  const [dialogs, setDialogs] = createSignal<ChatDialog[]>([]);
  const [activeDialogId, setActiveDialogId] = createSignal<string | null>(null);

  const [input, setInput] = createSignal('');
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [sending, setSending] = createSignal(false);
  const [loadingHistory, setLoadingHistory] = createSignal(true);

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
  let messagesRef: HTMLDivElement | undefined;
  let inputTextareaRef: HTMLTextAreaElement | undefined;

  // Provider + model resolved into what the send path needs.
  const activeProfile = createMemo(() => resolveProfile(providers(), selection()));

  const activeDialog = createMemo(() => dialogs().find((d) => d.id === activeDialogId()) ?? null);

  const messages = createMemo<ChatMessage[]>(() => activeDialog()?.messages ?? []);

  // tool_call_id -> result content, so each call can render with its result inline.
  const toolResults = createMemo(() => {
    const map = new Map<string, string>();
    for (const msg of messages()) {
      if (msg.role === 'tool' && msg.tool_call_id) map.set(msg.tool_call_id, msg.content);
    }
    return map;
  });

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
      const [settings, history] = await Promise.all([loadAiSettings(), loadChatHistory()]);

      if (!isMounted) return;

      setProviders(settings.providers);
      setSelection(reconcileSelection(settings.providers, settings.active));

      setDialogs(history);

      if (history.length > 0) {
        setActiveDialogId(history[0].id);
      } else {
        createNewDialog();
      }

      recoverDraftsAfterRestart();
    } finally {
      if (isMounted) {
        setLoadingHistory(false);
      }
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

  let saveTimer: number | null = null;

  function persistDialogs(nextDialogs: ChatDialog[]) {
    setDialogs(nextDialogs);

    /*
     * Debounce writes while the model is streaming.
     *
     * Without this, every token would cause a filesystem write.
     */
    if (saveTimer !== null) {
      window.clearTimeout(saveTimer);
    }

    saveTimer = window.setTimeout(() => {
      saveTimer = null;

      void saveChatHistory(nextDialogs).catch((err) => {
        console.error('Failed to persist chat history:', err);
      });
    }, 150);
  }

  async function flushDialogPersistence() {
    if (saveTimer !== null) {
      window.clearTimeout(saveTimer);
      saveTimer = null;
    }

    try {
      await saveChatHistory(dialogs());
    } catch (err) {
      console.error('Failed to persist chat history:', err);
    }
  }

  onCleanup(() => {
    if (saveTimer !== null) {
      window.clearTimeout(saveTimer);
      saveTimer = null;
    }

    void saveChatHistory(dialogs());
  });

  /*
   * -----------------------------------------------------------------------
   * Dialog helpers
   * -----------------------------------------------------------------------
   */

  function createNewDialog(): ChatDialog {
    const now = Date.now();

    const dialog: ChatDialog = {
      id: crypto.randomUUID(),
      title: NEW_CHAT_TITLE,
      messages: [],
      createdAt: now,
      updatedAt: now,
    };

    setDialogs((prev) => [dialog, ...prev]);
    setActiveDialogId(dialog.id);
    setHistoryOpen(false);

    /*
     * References belong to the current composer, not to the new dialog.
     */
    setReferencedFiles([]);

    void saveChatHistory([dialog, ...dialogs()]);

    return dialog;
  }

  function selectDialog(id: string) {
    setActiveDialogId(id);
    setHistoryOpen(false);

    /*
     * References are composer state, so don't carry them between dialogs.
     */
    clearChatInput(false);

    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');

    restoreDraftForDialog(id);
  }

  async function deleteDialog(id: string) {
    const remaining = dialogs().filter((dialog) => dialog.id !== id);

    takePendingDraft(id);

    setDialogs(remaining);

    if (activeDialogId() === id) {
      if (remaining.length > 0) {
        setActiveDialogId(remaining[0].id);
      } else {
        const dialog = createNewDialog();
        setActiveDialogId(dialog.id);
      }
    }

    await saveChatHistory(remaining.length > 0 ? remaining : dialogs());
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
    updateDialogMessages(dialogId, (messages) =>
      messages.map((m) => (m.id === messageId ? { ...m, failed: true } : m))
    );
  }

  /**
   * Drafts still in storage at startup were never answered: the app closed
   * or crashed mid-request. Mark their messages failed and bring the text
   * back for the dialog that's open.
   */
  function recoverDraftsAfterRestart() {
    prunePendingDrafts(new Set(dialogs().map((d) => d.id)));

    for (const draft of listPendingDrafts()) {
      const dialog = dialogs().find((d) => d.id === draft.dialogId);
      const index = dialog?.messages.findIndex((m) => m.id === draft.messageId) ?? -1;

      if (dialog && index >= 0) {
        const answered = dialog.messages
          .slice(index + 1)
          .some((m) => m.role === 'assistant' && !m.error && !m.pending);

        if (answered) {
          // The answer made it into history before the draft was cleared.
          clearPendingDraft(draft.dialogId, draft.messageId);
          continue;
        }
        markMessageFailed(dialog.id, draft.messageId);
      }
    }

    const active = activeDialogId();
    if (active) restoreDraftForDialog(active);
  }

  /*
   * -----------------------------------------------------------------------
   * Dialog state helpers
   * -----------------------------------------------------------------------
   */

  /**
   * Update any part of a dialog by its stable ID.
   *
   * This should be used by asynchronous work such as agent turns,
   * because the user can switch active dialogs while that work is running.
   */
  function updateDialog(id: string, updater: (dialog: ChatDialog) => ChatDialog) {
    const next = dialogs().map((dialog) => (dialog.id === id ? updater(dialog) : dialog));

    persistDialogs(next);
  }

  /**
   * Update the messages belonging to a specific dialog.
   *
   * Deliberately takes dialogId instead of using activeDialogId().
   * Agent operations must continue targeting the dialog that started
   * the operation even if the user switches to another dialog.
   */
  function updateDialogMessages(
    dialogId: string,
    updater: (messages: ChatMessage[]) => ChatMessage[]
  ) {
    updateDialog(dialogId, (dialog) => ({
      ...dialog,
      messages: updater(dialog.messages),
      updatedAt: Date.now(),
    }));
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
   * Auto-scroll
   * -----------------------------------------------------------------------
   */

  createEffect(() => {
    messages();

    const dialog = activeDialog();
    const profile = activeProfile();

    if (!dialog || !profile) return;

    queueMicrotask(async () => {
      if (dialog.title === NEW_CHAT_TITLE && !isGeneratingTitle()) {
        setIsGeneratingTitle(true);

        try {
          /*
           * Start title generation in parallel with the actual answer.
           *
           * It is completely independent from the main conversation.
           */
          await generateTitleForDialog(dialog.id, profile);
        } catch {
          // Optionally log title generation errors.
        } finally {
          setIsGeneratingTitle(false);
        }
      }

      if (messagesRef) {
        messagesRef.scrollTop = messagesRef.scrollHeight;
      }
    });
  });

  /*
   * -----------------------------------------------------------------------
   * Title generation
   * -----------------------------------------------------------------------
   */

  async function generateTitleForDialog(dialogId: string, profile: ModelProfile) {
    const chatMessages = messages().filter(
      (msg) => !msg.toolCalls && !msg.tool_call_id && !msg.error && !msg.pending
    );

    if (chatMessages.length === 0) return;

    const title = await generateChatTitle(profile, chatMessages);

    if (!title || !isMounted) {
      return;
    }

    updateDialog(dialogId, (dialog) => ({
      ...dialog,
      title,
      updatedAt: Date.now(),
    }));
  }

  /*
   * -----------------------------------------------------------------------
   * Agent
   * -----------------------------------------------------------------------
   */

  /**
   * @param onSettled Called once this turn's outcome is known: true when the
   *   model answered without error (text or tool calls), false otherwise.
   *   Only the first turn passes it; later tool-loop steps don't affect
   *   whether the user's message was delivered.
   */
  async function runAgentTurn(
    history: ChatMessage[],
    step: number,
    dialogId: string,
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
      toolCalls: [],
    };

    updateDialogMessages(dialogId, (messages) => [...messages, pendingMsg]);

    const channel = new Channel<ChatStreamEvent>();
    currentChannel = channel;

    let assistantText = '';
    const toolCallsMap = new Map<string, ChatToolCall>();

    let pendingRender = false;
    let streamError: string | null = null;

    const flushAssistantText = () => {
      pendingRender = false;

      if (!isMounted) return;

      updateDialogMessages(dialogId, (messages) =>
        messages.map((message) =>
          message.id === pendingMsg.id
            ? {
                ...message,
                content: assistantText,
              }
            : message
        )
      );
    };

    channel.onmessage = (event) => {
      if (!isMounted) return;

      switch (event.type) {
        case 'Delta': {
          assistantText += event.content;

          if (!pendingRender) {
            pendingRender = true;

            rafId = requestAnimationFrame(() => {
              rafId = null;
              flushAssistantText();
            });
          }

          break;
        }

        case 'Done': {
          updateDialogMessages(dialogId, (messages) =>
            messages.map((message) =>
              message.id === pendingMsg.id
                ? {
                    ...message,
                    pending: false,
                  }
                : message
            )
          );

          break;
        }

        case 'Error': {
          streamError = event.message;

          updateDialogMessages(dialogId, (messages) =>
            messages.map((message) =>
              message.id === pendingMsg.id
                ? {
                    ...message,
                    pending: false,
                    error: event.message,
                  }
                : message
            )
          );

          break;
        }

        case 'ToolCallStart': {
          toolCallsMap.set(event.id, {
            id: event.id,
            name: event.name,
            arguments: '',
          });

          break;
        }

        case 'ToolCallArgumentsDelta': {
          const tc = toolCallsMap.get(event.id);

          if (tc) {
            tc.arguments += event.arguments_delta;

            updateDialogMessages(dialogId, (messages) =>
              messages.map((message) =>
                message.id === pendingMsg.id
                  ? {
                      ...message,
                      toolCalls: Array.from(toolCallsMap.values()),
                    }
                  : message
              )
            );
          }

          break;
        }

        case 'ToolCallComplete': {
          toolCallsMap.set(event.id, {
            id: event.id,
            name: event.name,
            arguments: event.arguments,
          });

          updateDialogMessages(dialogId, (messages) =>
            messages.map((message) =>
              message.id === pendingMsg.id
                ? {
                    ...message,
                    toolCalls: Array.from(toolCallsMap.values()),
                  }
                : message
            )
          );

          break;
        }
      }
    };

    const root = props.rootPath();

    const tools = root ? (AGENT_TOOL_SCHEMAS as unknown as unknown[]) : undefined;

    try {
      await sendChatMessage({
        profile,
        messages: history,
        channel,
        tools,
      });
    } catch (err) {
      if (isMounted) {
        updateDialogMessages(dialogId, (messages) =>
          messages.map((message) =>
            message.id === pendingMsg.id
              ? {
                  ...message,
                  pending: false,
                  error: String(err),
                }
              : message
          )
        );
      }

      onSettled?.(false);
      return;
    } finally {
      if (currentChannel === channel) {
        currentChannel = null;
      }
    }

    if (streamError) {
      onSettled?.(false);
      return;
    }

    onSettled?.(true);

    const toolCalls = Array.from(toolCallsMap.values());

    /*
     * Normal final answer.
     */
    if (toolCalls.length === 0 || !root) {
      updateDialogMessages(dialogId, (messages) =>
        messages.map((message) =>
          message.id === pendingMsg.id
            ? {
                ...message,
                pending: false,
              }
            : message
        )
      );

      await flushDialogPersistence();

      return;
    }

    /*
     * Safety limit.
     */
    if (step >= MAX_AGENT_STEPS) {
      updateDialogMessages(dialogId, (messages) => [
        ...messages.map((message) =>
          message.id === pendingMsg.id
            ? {
                ...message,
                pending: false,
              }
            : message
        ),
        {
          id: crypto.randomUUID(),
          role: 'assistant',
          content: '',
          error: `Stopped after ${MAX_AGENT_STEPS} tool-call steps to avoid a runaway loop.`,
        },
      ]);

      await flushDialogPersistence();

      return;
    }

    /*
     * Lock in the assistant message that requested the tools.
     */
    const assistantMsg: ChatMessage = {
      ...pendingMsg,
      content: assistantText,
      toolCalls,
      pending: false,
    };

    updateDialogMessages(dialogId, (messages) =>
      messages.map((message) => (message.id === pendingMsg.id ? assistantMsg : message))
    );

    /*
     * Execute requested tools.
     */
    const toolResultMsgs: ChatMessage[] = [];

    for (const tc of toolCalls) {
      const result = await executeAgentTool(root, tc.name, tc.arguments);

      const toolMsg: ChatMessage = {
        id: crypto.randomUUID(),
        role: 'tool',
        content: result,
        tool_call_id: tc.id,
      };

      toolResultMsgs.push(toolMsg);

      updateDialogMessages(dialogId, (messages) => [...messages, toolMsg]);
    }

    /*
     * IMPORTANT:
     *
     * Do not use the Solid signal immediately after updating it as the
     * source of truth. Construct the next model history explicitly.
     */
    const nextHistory = [...history, assistantMsg, ...toolResultMsgs];

    await runAgentTurn(nextHistory, step + 1, dialogId);
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

    const wireUserMsg: ChatMessage =
      wireContentParts.length > 0
        ? {
            ...userMsg,
            content: wireContentParts.join('\n\n'),
          }
        : userMsg;

    /*
     * IMPORTANT:
     *
     * Capture the history BEFORE inserting the new user message.
     */
    /*
     * Failed messages and error placeholders stay visible in the transcript
     * but are never sent to the model again.
     */
    const historyBase = dialog.messages.filter((m) => !m.failed && !m.error && !m.pending);

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

    /*
     * Put the clean message into the persisted conversation.
     */
    updateDialogMessages(dialog.id, (messages) => [...messages, userMsg]);

    /*
     * Clear right away; the draft brings it back if the request fails.
     */
    clearChatInput();

    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    setSending(true);

    try {
      await runAgentTurn([...historyBase, wireUserMsg], 0, dialog.id, (ok) => {
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

      if (isMounted) {
        setSending(false);
      }

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

          <Show
            when={!loadingHistory()}
            fallback={<div class="chat-history-empty">Loading...</div>}
          >
            <Show
              when={dialogs().length > 0}
              fallback={<div class="chat-history-empty">No conversations yet</div>}
            >
              <div class="chat-history-list">
                <For each={dialogs()}>
                  {(dialog) => (
                    <div
                      class="chat-history-item"
                      classList={{
                        active: dialog.id === activeDialogId(),
                      }}
                    >
                      <button
                        class="chat-history-item-main"
                        onClick={() => selectDialog(dialog.id)}
                      >
                        <span class="chat-history-item-title">{dialog.title}</span>

                        <span class="chat-history-item-date">
                          {new Date(dialog.updatedAt).toLocaleString()}
                        </span>
                      </button>

                      <button
                        class="chat-history-delete"
                        title="Delete chat"
                        onClick={() => void deleteDialog(dialog.id)}
                      >
                        ×
                      </button>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </div>
      </Show>
      <Show when={!props.rootPath()}>
        <div class="chat-tools-hint">Open a folder to let the model read and edit your files.</div>
      </Show>
      <div ref={messagesRef} class="chat-messages">
        <For each={messages()} fallback={<div class="empty-state">Ask the model anything</div>}>
          {/* Tool results render inside their call's block, not as separate rows. */}
          {(m) => (
            <Show when={m.role !== 'tool'}>
              <>
                <Show when={m.pending && m.content.length === 0}>
                  <div
                    class="chat-message"
                    classList={{
                      user: m.role === 'user',
                      assistant: m.role === 'assistant',
                    }}
                  >
                    <span class="chat-pending">Thinking...</span>
                  </div>
                </Show>

                <Show when={!m.pending || m.content.length > 0}>
                  <Show when={m.error}>
                    <div
                      class="chat-message"
                      classList={{
                        user: m.role === 'user',
                        assistant: m.role === 'assistant',
                      }}
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
                      title={
                        m.failed ? 'Not delivered: text was put back into the input' : undefined
                      }
                    >
                      <MessageContent content={m.content} />
                    </div>
                  </Show>
                </Show>

                <Show when={m.toolCalls && m.toolCalls.length > 0}>
                  <div class="message-tools">
                    <For each={m.toolCalls!}>
                      {(tc) => (
                        <ToolCallBlock
                          toolCall={tc}
                          result={toolResults().get(tc.id)}
                          running={sending()}
                        />
                      )}
                    </For>
                  </div>
                </Show>
              </>
            </Show>
          )}
        </For>
      </div>
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
