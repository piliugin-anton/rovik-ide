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
  TreeRow,
} from '../types';
import type { ActiveFileContext } from '../lib/activeContext';
import { buildContextHeader } from '../lib/activeContext';
import { loadAiSettings, saveAiSettings } from '../lib/aiSettings';
import { loadChatHistory, saveChatHistory } from '../lib/chatHistory';
import { generateChatTitle } from '../lib/chatTitle';
import { sendChatMessage } from '../lib/aiChat';
import { AGENT_TOOL_SCHEMAS, executeAgentTool } from '../lib/tools';
import ModelSettingsPanel from './ModelSettingsPanel';
import ToolCallBlock from './ToolCallBlock';
import { isBlank } from '../lib/textUtils';
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
  const [profiles, setProfiles] = createSignal<ModelProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = createSignal<string | null>(null);

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
  let rafId: number | null = null;
  let isMounted = true;
  let messagesRef: HTMLDivElement | undefined;
  let inputTextareaRef: HTMLTextAreaElement | undefined;

  const activeProfile = createMemo(
    () => profiles().find((p) => p.id === activeProfileId()) ?? null
  );

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

      setProfiles(settings.profiles);
      setActiveProfileId(settings.activeProfileId ?? settings.profiles[0]?.id ?? null);

      setDialogs(history);

      if (history.length > 0) {
        setActiveDialogId(history[0].id);
      } else {
        createNewDialog();
      }
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
    setReferencedFiles([]);
    setInput('');

    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
  }

  async function deleteDialog(id: string) {
    const remaining = dialogs().filter((dialog) => dialog.id !== id);

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

    /*
     * Insert the chip.
     */
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

    // Pull the plain text version of the container's contents (including the space)
    const updatedText = container.innerText || container.textContent;
    setInput(updatedText);

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

  function clearChatInput() {
    setInput('');
    setReferencedFiles([]);

    if (inputTextareaRef) {
      inputTextareaRef.innerHTML = '';
      inputTextareaRef.focus();
    }
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

  function persistAiSettings(nextProfiles: ModelProfile[], nextActiveId: string | null) {
    void saveAiSettings({
      profiles: nextProfiles,
      activeProfileId: nextActiveId,
    });
  }

  function handleProfilesSaved(nextProfiles: ModelProfile[]) {
    setProfiles(nextProfiles);

    let nextActive = activeProfileId();

    if (!nextProfiles.some((p) => p.id === nextActive)) {
      nextActive = nextProfiles[0]?.id ?? null;
      setActiveProfileId(nextActive);
    }

    persistAiSettings(nextProfiles, nextActive);
  }

  function handleModelChange(id: string) {
    setActiveProfileId(id);
    persistAiSettings(profiles(), id);
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

  async function runAgentTurn(
    history: ChatMessage[],
    step: number,
    dialogId: string
  ): Promise<void> {
    const profile = activeProfile();

    if (!profile || !isMounted) {
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

    let clearInput = true;
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
          clearInput = false;
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

      if (clearInput) {
        clearChatInput();
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

      return;
    } finally {
      if (currentChannel === channel) {
        currentChannel = null;
      }
    }

    if (streamError) {
      return;
    }

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
    const historyBase = dialog.messages;

    /*
     * Put the clean message into the persisted conversation.
     */
    updateDialogMessages(dialog.id, (messages) => [...messages, userMsg]);

    setInput('');

    /*
     * References have now been consumed by this message.
     */
    setReferencedFiles([]);

    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    setSending(true);

    try {
      await runAgentTurn([...historyBase, wireUserMsg], 0, dialog.id);
    } finally {
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
                      }}
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
            onInput={(e) => {
              const target = e.currentTarget;
              const text = target.textContent ?? '';
              setInput(text);

              const sel = window.getSelection();
              const caretOffset =
                sel && sel.focusNode
                  ? getTextOffset(target, sel.focusNode, sel.focusOffset)
                  : text.length;

              updateFileAutocomplete(text, caretOffset);
            }}
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
        <select
          value={activeProfileId() ?? ''}
          onChange={(e) => handleModelChange(e.currentTarget.value)}
          disabled={profiles().length === 0}
        >
          <Show when={profiles().length > 0} fallback={<option value="">No models saved</option>}>
            <For each={profiles()}>{(p) => <option value={p.id}>{p.label}</option>}</For>
          </Show>
        </select>
      </div>
      <Show when={settingsOpen()}>
        <ModelSettingsPanel
          profiles={profiles()}
          onSave={handleProfilesSaved}
          onClose={() => setSettingsOpen(false)}
        />
      </Show>
    </div>
  );
}
