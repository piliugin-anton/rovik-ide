import { invoke } from '@tauri-apps/api/core';
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
  untrack,
  createRoot,
} from 'solid-js';
import type {
  ChatMessage,
  ChatToolCall,
  DialogSummary,
  FileSuggestion,
  ModelProfile,
  OpenAIChatOptions,
  ModelSelection,
  Provider,
  TreeRow,
} from '../types';
import type { ActiveFileContext } from '../lib/activeContext';
import { buildContextHeader } from '../lib/activeContext';
import {
  lineRangeLabel,
  referenceKey,
  referenceLabel,
  selectionReference,
} from '../lib/fileReferences';
import { onAddToChat, takeChatReferences } from '../lib/editorBridge';
import {
  loadAiSettings,
  reconcileSelection,
  resolveProfile,
  saveAiSettings,
} from '../lib/aiSettings';
import { chatStore } from '../lib/chatStore';
import { chatApi } from '../lib/chatApi';
import { highlightTitle } from '../lib/chatSearch';
import { generateChatTitle } from '../lib/chatTitle';
import { estimateContext } from '../lib/aiChat';
import { DEFAULT_CONTEXT_TOKENS } from '../lib/contextBudget';
import ContextMeter, { type ContextUsage } from './ContextMeter';
import { chatMode, implementationRequest, setChatMode, type ChatMode } from '../lib/chatMode';
import { answerQuestions, pendingQuestionsIn } from '../lib/questions';
import { notify } from '../lib/notifications';
import ModeSelector from './ModeSelector';
import EffortSelector from './EffortSelector';
import ContextControls from './ContextControls';
import { contextModeFor, ensureContextMode } from '../lib/contextMode';
import {
  formatsToSend,
  loadEfforts,
  mergeLearned,
  reasoningStatus,
  saveEfforts,
  type Effort,
} from '../lib/reasoning';
import TodoPanel from './TodoPanel';
import { latestChecklist } from '../lib/todo';
import { onImplementPlanRequest, relativePlanPath, takeImplementPlanRequest } from '../lib/plans';
import QuestionPanel from './QuestionPanel';
import {
  configureAgent,
  initAgentRuns,
  isRunning,
  localDate,
  onMessageUnanswered,
  onReasoningLearned,
  onShowChat,
  setRunEffort,
  startRun,
  stopRun,
  toolNotes,
  type AgentConfig,
  type StartKind,
} from '../lib/agentRuns';
import AiSettingsModal, { type AiSettingsPage } from './AiSettingsModal';
import { clearVfsSettingsRequest, vfsSettingsRequested } from '../lib/virtualFilesStartup';
import ModelPicker from './ModelPicker';
import { confirmDialog } from './ConfirmDialog';
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
import { DEFAULT_PERMISSIONS, sameModel, type AgentPermissions } from '../lib/permissions';
import { readFileContent } from '../lib/commands';

const IS_MAC = navigator.platform.toUpperCase().includes('MAC');

const NEW_CHAT_TITLE = 'New Chat';

/** Chats whose interrupted tool calls were offered again this session: once each. */
const resumed = new Set<string>();

interface ChatProps {
  rootPath: Accessor<string | null>;
  activeFileContext: Accessor<ActiveFileContext | null>;
  files: Accessor<TreeRow[]>;
  /**
   * The open project, whose chats are shown (lib/projects.ts, resolved by
   * App): undefined while it's being resolved, null with no folder open.
   */
  projectId: Accessor<string | null | undefined>;
}

export default function ChatPanel(props: ChatProps) {
  const [providers, setProviders] = createSignal<Provider[]>([]);
  const [selection, setSelection] = createSignal<ModelSelection | null>(null);
  const [permissions, setPermissions] = createSignal<AgentPermissions>(DEFAULT_PERMISSIONS);

  // Chat list page + open chats (the app's, kept while the panel is away);
  // everything else stays in the database.
  const store = chatStore;
  const activeDialogId = () => store.state.activeId;

  const [input, setInput] = createSignal('');
  /** The open settings page, or null when the settings modal is closed. */
  const [settingsPage, setSettingsPage] = createSignal<AiSettingsPage | null>(null);
  // A virtual files notification's "Show steps" (lib/virtualFilesStartup.ts).
  createEffect(() => {
    if (!vfsSettingsRequested()) return;
    clearVfsSettingsRequest();
    setSettingsPage('vfs');
  });
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [historyQuery, setHistoryQuery] = createSignal('');
  /** Lags historyQuery slightly so fast typing doesn't search on every key. */
  const [searchQuery, setSearchQuery] = createSignal('');
  /** The open chat's agent run is going on (it runs in the backend). */
  const sending = () => isRunning(activeDialogId());
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

  /*
   * -----------------------------------------------------------------------
   * Context used (the bar above the input)
   * -----------------------------------------------------------------------
   */

  const [contextUsage, setContextUsage] = createSignal<ContextUsage | null>(null);

  let isMounted = true;
  let inputTextareaRef: HTMLTextAreaElement | undefined;

  // Provider + model resolved into what the send path needs.
  const activeProfile = createMemo(() => resolveProfile(providers(), selection()));

  /*
   * Reasoning effort (lib/reasoning.ts): chosen per model in the chat, sent
   * only in the forms the model's provider accepts, which is learned from
   * the replies and kept with the model in the settings.
   */
  const [efforts, setEfforts] = createSignal<Record<string, Effort>>(loadEfforts());
  const effortFor = (profileId: string | undefined): Effort =>
    (profileId && efforts()[profileId]) || 'default';
  function setEffort(effort: Effort) {
    const profile = activeProfile();
    if (!profile) return;
    const next = { ...efforts(), [profile.id]: effort };
    setEfforts(next);
    saveEfforts(next);
    // Runs with this model follow it; this chat's, if stuck, right away.
    setRunEffort(profile.id, reasoningOptions(profile), activeDialogId());
  }
  /** The effort selector shows unless the model is known not to take one. */
  const showEffort = createMemo(() => {
    const p = activeProfile();
    return !!p && reasoningStatus(p.reasoning) !== 'no';
  });

  /** The effort for a request: nothing for "Default", or when no form is worth sending. */
  function reasoningOptions(profile: ModelProfile): OpenAIChatOptions | undefined {
    const effort = effortFor(profile.id);
    if (effort === 'default') return undefined;
    const formats = formatsToSend(profile.reasoning);
    if (formats.length === 0) return undefined;
    return { reasoning_effort: effort, reasoning_formats: formats };
  }

  /** Stores what a reply showed about a model's reasoning support. */
  function learnReasoning(
    profileId: string,
    outcome: { accepted?: string[]; rejected?: string[]; unresolved?: string[]; reasons?: boolean }
  ) {
    if (outcome.accepted?.length === 0) delete outcome.accepted;
    if (outcome.rejected?.length === 0) delete outcome.rejected;
    if (outcome.unresolved?.length === 0) delete outcome.unresolved;
    const split = profileId.indexOf('::');
    const providerId = profileId.slice(0, split);
    const modelId = profileId.slice(split + 2);
    let changed = false;
    const next = providers().map((p) => {
      if (p.id !== providerId) return p;
      return {
        ...p,
        models: p.models.map((m) => {
          if (m.id !== modelId) return m;
          const setting = m.reasoning ?? { choice: 'auto' as const };
          let learned = setting.learned;
          if (outcome.accepted || outcome.rejected || outcome.unresolved) {
            learned = mergeLearned(
              learned,
              outcome.accepted ?? [],
              outcome.rejected ?? [],
              outcome.unresolved ?? []
            );
          }
          if (outcome.reasons && !learned?.reasons) {
            learned = {
              ...(learned ?? { accepted: [], rejected: [], at: Date.now() }),
              reasons: true,
            };
          }
          if (learned === setting.learned) return m;
          changed = true;
          return { ...m, reasoning: { ...setting, learned } };
        }),
      };
    });
    if (!changed) return;
    setProviders(next);
    persistAiSettings(next, selection());
  }

  const activeDialog = () => store.state.active;
  const activeChat = () => {
    const id = store.state.activeId;
    return id ? store.state.open[id] : undefined;
  };
  const messages = (): ChatMessage[] => activeChat()?.messages ?? [];

  /*
   * Re-estimated whenever what the next request would send changes: another
   * chat or model, a message added or finished, a run starting or ending.
   * Debounced: a run adds messages quickly.
   */
  let contextTimer: number | undefined;
  let contextRequest = 0;
  createEffect(
    on(
      () => {
        const list = messages();
        return [
          store.state.activeId,
          activeProfile(),
          list.length,
          list[list.length - 1]?.finalized,
          sending(),
          props.rootPath(),
          efforts(),
          chatMode(),
          permissions(),
        ] as const;
      },
      ([dialogId, profile]) => {
        window.clearTimeout(contextTimer);
        if (!dialogId || !profile) {
          setContextUsage(null);
          return;
        }
        const request = ++contextRequest;
        contextTimer = window.setTimeout(async () => {
          try {
            // Tools and the project's prompt section are counted by the backend.
            const estimate = await estimateContext({
              profile,
              dialogId,
              root: props.rootPath(),
              mode: chatMode(),
              reasoningEffort: reasoningOptions(profile)?.reasoning_effort,
            });
            if (request !== contextRequest) return;
            const known = !!profile.contextTokens && profile.contextTokens > 0;
            setContextUsage({
              promptTokens: estimate.promptTokens,
              windowTokens: known ? profile.contextTokens! : DEFAULT_CONTEXT_TOKENS,
              windowGuessed: !known,
              fullHistoryTokens: estimate.fullHistoryTokens,
              historyTokens: estimate.historyTokens,
              replyReserveTokens: estimate.replyReserveTokens,
              shortened:
                estimate.outdated > 0 ||
                estimate.droppedTurns > 0 ||
                estimate.fullHistoryTokens > estimate.historyTokens,
            });
          } catch (err) {
            console.error('Context estimate failed:', err);
          }
        }, 400);
      }
    )
  );
  onCleanup(() => window.clearTimeout(contextTimer));

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
        .search(query, store.projectId())
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

  /**
   * The editor's current selection as a reference ("App.tsx:12-14"). The
   * editor keeps its selection while focus is in the composer, so this is
   * whatever was selected before switching to the chat.
   */
  const selectionSuggestion = createMemo<FileSuggestion | null>(() => {
    const ctx = props.activeFileContext();
    if (!ctx?.selection) return null;
    const id = props.files().find((f) => f.path === ctx.path)?.id ?? ctx.path;
    return selectionReference(ctx.path, props.rootPath(), ctx.selection, id);
  });

  const fileSuggestions = createMemo<FileSuggestion[]>(() => {
    if (!fileSuggestionsOpen()) return [];

    const query = fileQuery().trim().toLowerCase();

    // The selection comes first, so "@" + Enter mentions it.
    const selection = selectionSuggestion();
    const selectionMatches =
      selection &&
      (!query ||
        selection.name.toLowerCase().includes(query) ||
        selection.relativePath.toLowerCase().includes(query));

    return [
      ...(selectionMatches ? [selection] : []),
      ...referenceableFiles()
        .filter((file) => {
          if (!query) return true;
          return (
            file.name.toLowerCase().includes(query) ||
            file.relativePath.toLowerCase().includes(query)
          );
        })
        .sort((a, b) => {
          // Prefer filename matches over path-only matches.
          const aNameMatch = a.name.toLowerCase().includes(query);
          const bNameMatch = b.name.toLowerCase().includes(query);
          if (aNameMatch !== bNameMatch) return aNameMatch ? -1 : 1;
          return a.relativePath.localeCompare(b.relativePath);
        })
        .slice(0, 50),
    ];
  });

  /*
   * Every file in the project, for "@": the backend lists them all
   * (.gitignore respected). The file tree only knows folders it has loaded,
   * so in a newly opened project it offered just the root's files. Fetched
   * when "@" opens, at most every few seconds, merged with the tree's.
   */
  const [projectFiles, setProjectFiles] = createSignal<{
    root: string;
    files: string[];
    at: number;
  } | null>(null);
  let projectFilesLoading = false;
  function refreshProjectFiles() {
    const root = props.rootPath();
    const cached = projectFiles();
    if (!root || projectFilesLoading) return;
    if (cached && cached.root === root && Date.now() - cached.at < 5000) return;
    projectFilesLoading = true;
    invoke<{ files: string[]; truncated: boolean }>('list_project_files', { root })
      .then((r) => setProjectFiles({ root, files: r.files, at: Date.now() }))
      .catch((err) => console.error('Listing project files failed:', err))
      .finally(() => (projectFilesLoading = false));
  }
  createEffect(() => {
    if (fileSuggestionsOpen()) refreshProjectFiles();
  });

  const referenceableFiles = createMemo<FileSuggestion[]>(() => {
    const root = props.rootPath();
    const byPath = new Map<string, FileSuggestion>();
    // The tree's rows first: they carry the ids the rest of the app uses.
    for (const file of props.files()) {
      if (file.isDir || file.ignored) continue;
      byPath.set(file.path, {
        id: file.id,
        path: file.path,
        name: file.name,
        relativePath: toRelativePath(file.path, root),
      });
    }
    const listed = projectFiles();
    if (listed && listed.root === root) {
      for (const path of listed.files) {
        if (byPath.has(path)) continue;
        byPath.set(path, {
          id: path,
          path,
          name: path.split(/[\\/]/).pop() ?? path,
          relativePath: toRelativePath(path, root),
        });
      }
    }
    return [...byPath.values()];
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
        // This project's chats (or, with no folder open, the ones with none).
        await store.switchProject(await projectReady());
      } catch (err) {
        console.error('Failed to load chats:', err);
        setHistoryLoadError(String(err));
        return;
      }
      if (!isMounted) return;

      // Back to the panel: the chat that was open stays open.
      if (!store.state.activeId) await openNewestChat();

      // Runs going on are known before drafts are judged unanswered.
      await initAgentRuns();
      await recoverDraftsAfterRestart();
    } finally {
      if (isMounted) {
        setLoadingHistory(false);
        // "Add to chat" may have opened this panel: its references waited
        // until the chat was loaded (opening one resets the composer).
        insertPendingReferences();
        takePlanFromEditor();
      }
    }
  });

  /** The project, once App has resolved it. */
  function projectReady(): Promise<string | null> {
    const now = props.projectId();
    if (now !== undefined) return Promise.resolve(now);
    return new Promise((resolve) => {
      createRoot((dispose) => {
        createEffect(() => {
          const id = props.projectId();
          if (id === undefined) return;
          dispose();
          resolve(id);
        });
      });
    });
  }

  async function openNewestChat() {
    const first = store.state.list.items[0];
    if (first) await store.openChat(first);
    else createNewDialog();
  }

  // Another project (another folder, or a copy given its own history): its chats.
  createEffect(
    on(
      () => props.projectId(),
      (id) => {
        if (id === undefined || id === store.projectId() || loadingHistory()) return;
        void (async () => {
          setLoadingHistory(true);
          try {
            await store.switchProject(id);
            if (isMounted && props.projectId() === id) await openNewestChat();
          } catch (err) {
            setHistoryLoadError(String(err));
          } finally {
            if (isMounted) setLoadingHistory(false);
          }
        })();
      },
      { defer: true }
    )
  );

  const stopAddToChat = onAddToChat(() => {
    if (!loadingHistory()) insertPendingReferences();
  });
  onCleanup(stopAddToChat);

  function insertPendingReferences() {
    for (const reference of takeChatReferences()) insertReferenceChip(reference);
  }

  /*
   * -----------------------------------------------------------------------
   * Cleanup
   * -----------------------------------------------------------------------
   */

  onCleanup(() => {
    isMounted = false;
  });

  /*
   * -----------------------------------------------------------------------
   * Persistence
   * -----------------------------------------------------------------------
   */

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
      !store.isRunning(dialogId) &&
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
    if (store.isRunning(id)) await stopRun(id);
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
    knownReferences.set(referenceKey(suggestion), suggestion);
    const chip = document.createElement('span');
    chip.className = 'chat-file-reference';
    chip.contentEditable = 'false';
    chip.dataset.path = suggestion.path;
    chip.dataset.ref = referenceKey(suggestion);
    // The line range sits outside the name, which truncates: "VeryLongNa…:12-14".
    const lines = suggestion.lines
      ? `<span class="chat-file-reference-lines">:${lineRangeLabel(suggestion.lines)}</span>`
      : '';
    const title = suggestion.lines
      ? `${suggestion.relativePath}, lines ${lineRangeLabel(suggestion.lines)}`
      : suggestion.relativePath;
    chip.innerHTML = `<span class="chat-file-reference-name" title="${escapeHtml(title)}">${escapeHtml(suggestion.name)}</span>${lines}
    <button
      type="button"
      class="chat-file-reference-remove"
      aria-label="Remove ${escapeHtml(referenceLabel(suggestion))}"
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
      removeReferencedFile(referenceKey(suggestion));
      chip.remove();
      setInput(readComposerText());
      recordComposerHistory('other');
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
     * Delete the '@' and the query typed after it. Located from their known
     * offsets, not the caret: if focus left the input (Tab onto the list),
     * refocusing puts the caret at the start in WebKit, and a range "from
     * the '@' to the caret" then removed nothing, leaving "@ind" behind.
     */
    const endLocation = findTextOffset(container, triggerStart + 1 + fileQuery().length);
    const deleteRange = document.createRange();
    deleteRange.setStart(startLocation.node, startLocation.offset);
    if (endLocation) deleteRange.setEnd(endLocation.node, endLocation.offset);
    else deleteRange.setEnd(selection.focusNode ?? container, selection.focusOffset);
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

    const key = referenceKey(suggestion);
    setReferencedFiles((current) =>
      current.some((file) => referenceKey(file) === key) ? current : [...current, suggestion]
    );
    recordComposerHistory('other');

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

  /**
   * Adds a reference chip from outside the composer ("Add to chat" in the
   * editor): at the caret when the composer has it, otherwise at the end.
   * A reference that's already there isn't added twice.
   */
  function insertReferenceChip(reference: FileSuggestion) {
    const container = inputTextareaRef;
    if (!container) return;
    const key = referenceKey(reference);
    const alreadyThere = composerReferences().some((r) => referenceKey(r) === key);

    const selection = window.getSelection();
    let range: Range;
    const caret = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
    if (caret && container.contains(caret.commonAncestorContainer)) {
      range = caret;
      range.deleteContents();
    } else {
      // The end of the last line; WebKit wraps lines in <div>s and keeps a
      // placeholder <br> at the end, which would push the chip to a new line.
      let target: Node = container;
      while (target.lastChild instanceof HTMLDivElement) target = target.lastChild;
      if (target.lastChild?.nodeName === 'BR') target.lastChild.remove();
      range = document.createRange();
      range.selectNodeContents(target);
      range.collapse(false);
    }

    if (!alreadyThere) {
      const preceding = document.createRange();
      preceding.selectNodeContents(container);
      preceding.setEnd(range.startContainer, range.startOffset);
      const before = preceding.toString();
      const fragment = document.createDocumentFragment();
      if (before && !/\s$/.test(before)) fragment.append(document.createTextNode(' '));
      fragment.append(createFileChip(reference));
      const space = document.createTextNode(' ');
      fragment.append(space);
      range.insertNode(fragment);
      range.setStartAfter(space);
      range.collapse(true);
      setReferencedFiles((current) =>
        current.some((f) => referenceKey(f) === key) ? current : [...current, reference]
      );
      setInput(readComposerText());
    }

    setFileSuggestionsOpen(false);
    container.focus();
    selection?.removeAllRanges();
    selection?.addRange(range);
    recordComposerHistory('other');
  }

  /** File references in the composer, in order, once each. */
  function composerReferences(): FileSuggestion[] {
    const seen = new Set<string>();
    const out: FileSuggestion[] = [];
    for (const segment of serializeComposer()) {
      if (segment.type !== 'file') continue;
      const key = referenceKey(segment.file);
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(segment.file);
    }
    return out;
  }

  function removeReferencedFile(key: string) {
    setReferencedFiles((current) => current.filter((file) => referenceKey(file) !== key));

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
    resetComposerHistory();
  }

  /*
   * Keys that move the caret, while the "@" file list is open, move through
   * the list instead and never reach the text. A listener on the input
   * itself, in the capture phase, so it runs before anything else (the
   * component's own handler goes through Solid's document-level delegation,
   * and WebKitGTK didn't always respect that). If the webview moves the
   * caret anyway, it's put back.
   */
  const CARET_KEYS = new Set([
    'ArrowUp',
    'ArrowDown',
    'ArrowLeft',
    'ArrowRight',
    'Home',
    'End',
    'PageUp',
    'PageDown',
  ]);

  function handleSuggestionKeys(e: KeyboardEvent) {
    if (!fileSuggestionsOpen() || e.ctrlKey || e.metaKey || e.altKey) return;
    // Tab completes with the selected file, as in editors and terminals,
    // instead of moving focus out of the input (onto the list).
    if (e.key === 'Tab') {
      e.preventDefault();
      e.stopPropagation();
      const suggestion = fileSuggestions()[selectedSuggestionIndex()];
      if (suggestion && !e.shiftKey) acceptFileSuggestion(suggestion);
      return;
    }
    if (!CARET_KEYS.has(e.key)) return;
    e.preventDefault();
    e.stopPropagation();
    const count = fileSuggestions().length;
    if (count > 0) {
      const last = count - 1;
      const move: Record<string, (i: number) => number> = {
        ArrowDown: (i) => Math.min(i + 1, last),
        ArrowUp: (i) => Math.max(i - 1, 0),
        PageDown: (i) => Math.min(i + 5, last),
        PageUp: (i) => Math.max(i - 5, 0),
        Home: () => 0,
        End: () => last,
      };
      const step = move[e.key];
      if (step) setSelectedSuggestionIndex(step);
    }
    keepCaret();
  }

  /** Puts the caret back if something still moved it during this key. */
  function keepCaret() {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return;
    const saved = sel.getRangeAt(0).cloneRange();
    requestAnimationFrame(() => {
      const now = window.getSelection();
      if (!now || now.rangeCount === 0) return;
      const r = now.getRangeAt(0);
      if (
        r.startContainer !== saved.startContainer ||
        r.startOffset !== saved.startOffset ||
        !r.collapsed
      ) {
        now.removeAllRanges();
        now.addRange(saved);
      }
    });
  }

  onMount(() => {
    const el = inputTextareaRef;
    if (!el) return;
    el.addEventListener('keydown', handleSuggestionKeys, true);
    onCleanup(() => el.removeEventListener('keydown', handleSuggestionKeys, true));
  });

  /*
   * -----------------------------------------------------------------------
   * Undo / redo in the composer
   * -----------------------------------------------------------------------
   *
   * The composer keeps its own history instead of relying on the webview's:
   * the app changes its content itself (reference chips, clearing after
   * sending, drafts), which native contenteditable undo doesn't follow, and
   * WebKitGTK's undo there isn't reliable. Snapshots of the content and the
   * caret; typing is grouped into word-sized steps (a space or a pause
   * starts a new one), and chips, pastes and deletions are steps of their own.
   */
  interface ComposerSnapshot {
    html: string;
    caret: number;
  }
  let composerHistory: ComposerSnapshot[] = [];
  let composerHistoryAt = -1;
  let lastStepKind: 'typing' | 'deleting' | 'other' = 'other';
  let lastStepTime = 0;
  let pastingText = false;
  /** Every reference chip ever made, so a restored snapshot can rebuild them. */
  const knownReferences = new Map<string, FileSuggestion>();
  const HISTORY_STEPS = 200;
  const GROUP_WITHIN_MS = 1000;

  function composerSnapshot(): ComposerSnapshot | null {
    const el = inputTextareaRef;
    if (!el) return null;
    const sel = window.getSelection();
    const caret =
      sel && sel.focusNode && el.contains(sel.focusNode)
        ? getTextOffset(el, sel.focusNode, sel.focusOffset)
        : (el.textContent ?? '').length;
    return { html: el.innerHTML, caret };
  }

  function resetComposerHistory() {
    composerHistory = [composerSnapshot() ?? { html: '', caret: 0 }];
    composerHistoryAt = 0;
    lastStepKind = 'other';
  }

  function recordComposerHistory(kind: 'typing' | 'deleting' | 'other') {
    const snap = composerSnapshot();
    if (!snap) return;
    if (composerHistory.length === 0) composerHistory = [{ html: '', caret: 0 }];
    const current = composerHistory[composerHistoryAt];
    if (current && current.html === snap.html) {
      current.caret = snap.caret;
      return;
    }
    // A new change drops whatever could have been redone.
    composerHistory = composerHistory.slice(0, composerHistoryAt + 1);
    const now = Date.now();
    const grouped =
      kind !== 'other' &&
      kind === lastStepKind &&
      now - lastStepTime < GROUP_WITHIN_MS &&
      composerHistoryAt > 0;
    if (grouped) {
      composerHistory[composerHistoryAt] = snap;
    } else {
      composerHistory.push(snap);
      if (composerHistory.length > HISTORY_STEPS) composerHistory.shift();
      composerHistoryAt = composerHistory.length - 1;
    }
    // Typing a space ends the word: the next letter starts a new step.
    lastStepKind =
      kind === 'typing' && /\s$/.test(snap.html.replace(/<[^>]*>/g, '').replace(/&nbsp;/g, ' '))
        ? 'other'
        : kind;
    lastStepTime = now;
  }

  function applyComposerSnapshot(snap: ComposerSnapshot) {
    const el = inputTextareaRef;
    if (!el) return;
    el.innerHTML = snap.html;
    // Chips come back with their remove buttons (markup alone loses them).
    for (const old of [...el.querySelectorAll<HTMLElement>('.chat-file-reference')]) {
      if (old.dataset.pasteId) {
        const text = knownPastes.get(old.dataset.pasteId);
        if (text !== undefined) old.replaceWith(createPastedChip(text, old.dataset.pasteId));
        continue;
      }
      const ref = knownReferences.get(old.dataset.ref ?? '');
      if (ref) old.replaceWith(createFileChip(ref));
    }
    el.focus();
    const at = findTextOffset(el, snap.caret);
    const sel = window.getSelection();
    if (sel) {
      const range = document.createRange();
      if (at) range.setStart(at.node, at.offset);
      else range.selectNodeContents(el);
      range.collapse(!at ? false : true);
      sel.removeAllRanges();
      sel.addRange(range);
    }
    setFileSuggestionsOpen(false);
    setInput(readComposerText());
    setReferencedFiles(composerReferences());
    lastStepKind = 'other';
  }

  function undoComposer() {
    if (composerHistory.length === 0) resetComposerHistory();
    // Text typed since the last step is saved first, so it can be redone.
    recordComposerHistory('other');
    if (composerHistoryAt <= 0) return;
    composerHistoryAt--;
    applyComposerSnapshot(composerHistory[composerHistoryAt]);
  }

  function redoComposer() {
    if (composerHistoryAt >= composerHistory.length - 1) return;
    composerHistoryAt++;
    applyComposerSnapshot(composerHistory[composerHistoryAt]);
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

        if (child.classList.contains('chat-pasted-chip')) {
          const text = knownPastes.get(child.dataset.pasteId ?? '');
          if (text !== undefined) segments.push({ type: 'pasted', text });
          return;
        }
        if (child.classList.contains('chat-file-reference')) {
          const key = child.dataset.ref ?? child.dataset.path ?? '';
          const file = referencedFiles().find((f) => referenceKey(f) === key);
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
      .map((segment) =>
        segment.type === 'text'
          ? segment.text
          : segment.type === 'pasted'
            ? `\n<pasted_text lines="${segment.text.split('\n').length}">\n${segment.text}\n</pasted_text>\n`
            : referenceLabel(segment.file)
      )
      .join('');
  }

  function handleComposerInput(target: HTMLElement, inputType?: string, data?: string | null) {
    /*
     * Once the composer is visually empty, drop whatever markup is left
     * (stray <br>, empty styled <span>s). Otherwise WebKit keeps applying the
     * last node's style to new text, and the :empty placeholder never shows.
     */
    if (!target.textContent && !target.querySelector('.chat-file-reference')) {
      target.innerHTML = '';
    }

    setInput(readComposerText());
    recordComposerHistory(
      pastingText
        ? 'other'
        : // A space belongs to the word it ends; the next letter starts a new step.
          inputType === 'insertText' && data
          ? 'typing'
          : inputType?.startsWith('delete')
            ? 'deleting'
            : 'other'
    );

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

    // A paste is its own undo step, not part of the word being typed.
    pastingText = true;
    try {
      if (document.execCommand('insertText', false, text)) return;
    } finally {
      pastingText = false;
    }

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
    if (!text) return;
    const clean = text.replace(/\r\n?/g, '\n');
    // Long text as a chip: inserting thousands of lines into the editable
    // input was slow, and every keystroke after it slower still.
    if (clean.length > PASTE_CHIP_CHARS || clean.split('\n').length > PASTE_CHIP_LINES) {
      insertPastedChip(clean);
    } else {
      insertPlainText(clean);
    }
  }

  /*
   * Long pastes: a "Pasted text" chip in the input, the text itself in
   * memory (knownPastes). Sent wrapped in <pasted_text> (readComposerText),
   * shown collapsed in the chat (MessageContent).
   */
  const PASTE_CHIP_CHARS = 2000;
  const PASTE_CHIP_LINES = 30;
  const knownPastes = new Map<string, string>();

  function createPastedChip(text: string, id: string = crypto.randomUUID()): HTMLSpanElement {
    knownPastes.set(id, text);
    const lines = text.split('\n').length;
    const chip = document.createElement('span');
    chip.className = 'chat-file-reference chat-pasted-chip';
    chip.contentEditable = 'false';
    chip.dataset.pasteId = id;
    chip.title = text.length > 400 ? `${text.slice(0, 400)}…` : text;
    chip.innerHTML = `<span class="chat-file-reference-name">Pasted text</span><span class="chat-file-reference-lines">&nbsp;· ${lines.toLocaleString()} line${lines === 1 ? '' : 's'}</span><button type="button" class="chat-file-reference-remove" aria-label="Remove pasted text">\u00d7</button>`;
    chip.querySelector('button')?.addEventListener('mousedown', (e) => e.preventDefault());
    chip.querySelector('button')?.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      chip.remove();
      setInput(readComposerText());
      recordComposerHistory('other');
    });
    return chip;
  }

  function insertPastedChip(text: string) {
    const container = inputTextareaRef;
    if (!container) return;
    container.focus();
    const selection = window.getSelection();
    let range: Range;
    if (
      selection &&
      selection.rangeCount > 0 &&
      container.contains(selection.getRangeAt(0).commonAncestorContainer)
    ) {
      range = selection.getRangeAt(0);
      range.deleteContents();
    } else {
      range = document.createRange();
      range.selectNodeContents(container);
      range.collapse(false);
    }
    const chip = createPastedChip(text);
    const space = document.createTextNode(' ');
    range.insertNode(space);
    range.insertNode(chip);
    range.setStartAfter(space);
    range.collapse(true);
    selection?.removeAllRanges();
    selection?.addRange(range);
    setInput(readComposerText());
    recordComposerHistory('other');
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
      } else if (segment.type === 'pasted') {
        fragment.append(createPastedChip(segment.text), document.createTextNode(' '));
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
    // The restored draft is where undo starts.
    resetComposerHistory();
    setReferencedFiles((current) => [
      ...current,
      ...files.filter((f) => !current.some((c) => referenceKey(c) === referenceKey(f))),
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
    // A run is going on: its message may still be answered.
    if (!stored || store.isRunning(dialogId)) return;

    const draft = takePendingDraft(dialogId);
    if (draft) queueMicrotask(() => restoreDraftToComposer(draft));
  }

  function markMessageFailed(dialogId: string, messageId: string) {
    void store.markFailed(dialogId, messageId);
  }

  // A sent message got no answer: its draft comes back while its chat is open.
  onCleanup(
    onMessageUnanswered((dialogId, messageId) => {
      if (!isMounted || activeDialogId() !== dialogId) return;
      if (!listPendingDrafts().some((d) => d.dialogId === dialogId && d.messageId === messageId))
        return;
      const draft = takePendingDraft(dialogId);
      if (draft) queueMicrotask(() => restoreDraftToComposer(draft));
    })
  );

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
      // Its run is still going (the page was reloaded): it may yet be answered.
      if (store.isRunning(draft.dialogId)) continue;
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
   * Agent settings
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
          !msg.meta?.harness &&
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
   *
   * Runs happen in the backend (src-tauri/src/agent_run, lib/agentRuns.ts):
   * the panel starts and stops them, and what they store arrives in the chat
   * store whether the panel is mounted or not.
   */

  /** The settings runs follow; sent again whenever they change. */
  const agentConfig = (): AgentConfig => ({
    permissions: permissions(),
    approvalProfiles: approvalProfiles(permissions()),
  });
  createEffect(() => configureAgent(agentConfig()));

  // What replies show about the model's reasoning is kept with it.
  onCleanup(onReasoningLearned((learned) => learnReasoning(learned.profileId, learned)));
  // A question's notification: "Show".
  onCleanup(onShowChat(() => listController?.followLatest()));

  /**
   * Starts a run in a chat. The backend reads the chat from the database,
   * so writes still queued go first. False when it didn't start (no model,
   * or the backend refused: shown).
   */
  async function beginRun(
    dialogId: string,
    start: StartKind,
    currentUserContent?: string
  ): Promise<boolean> {
    const profile = activeProfile();
    if (!profile) return false;
    listController?.followLatest();
    try {
      await store.flush();
      await startRun({
        dialogId,
        start,
        profile,
        options: reasoningOptions(profile),
        mode: chatMode(),
        contextMode: ensureContextMode(dialogId),
        root: props.rootPath(),
        currentUserContent,
        localDate: localDate(),
        config: agentConfig(),
      });
      return true;
    } catch (err) {
      console.error('Agent run not started:', err);
      notify({
        kind: 'error',
        title: "The agent couldn't start",
        message: String(err),
        timeout: 12_000,
      });
      return false;
    }
  }

  /** Sends a user message (shown at once; the run stores it first thing). */
  async function sendMessage(
    dialogId: string,
    message: { id: string; content: string; meta: ChatMessage['meta'] },
    wireContent: string | undefined
  ): Promise<boolean> {
    store.addLocal(dialogId, { ...message, role: 'user', finalized: true });
    const started = await beginRun(dialogId, { kind: 'send', message }, wireContent);
    // Never stored: shown as failed.
    if (!started) store.applyPatch(dialogId, message.id, { failed: true });
    return started;
  }

  /** The mode of the request a run belongs to: its starting message's. */
  function runModeFor(dialogId: string): ChatMode {
    const list = store.messagesOf(dialogId);
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role === 'user' && !m.meta?.harness) return m.meta?.mode ?? 'agent';
    }
    return chatMode();
  }

  /**
   * "Implement this plan" under the plan in the chat: the plan file's current
   * text (edits included), or the reply itself if it has no file.
   */
  async function implementPlan() {
    const dialogId = activeDialogId();
    if (!dialogId) return;
    const last = store.messagesOf(dialogId).find((m) => m.id === planMessageId());
    if (!last) return;
    const file = last.meta?.planFile;
    const text = file ? await readFileContent(file).catch(() => null) : null;
    startImplementation(dialogId, text ?? last.content, text !== null ? file : undefined);
  }

  /**
   * Agent mode, carrying out a plan. The plan goes in full with the request,
   * which keeps it in view for the whole run (the current request is never
   * dropped from the context); the chat shows a short line.
   */
  function startImplementation(dialogId: string, plan: string, file?: string) {
    if (!activeProfile() || isRunning(dialogId)) return;
    setChatMode('agent');
    const rel = file ? relativePlanPath(file, props.rootPath()) : undefined;
    const wire = implementationRequest(plan, rel);
    void sendMessage(
      dialogId,
      {
        id: crypto.randomUUID(),
        content: rel ? `Implement the plan in \`${rel}\`.` : 'Implement the plan above.',
        meta: { mode: 'agent', sentContent: wire },
      },
      wire
    );
  }

  /** "Implement this plan" on a plan file in the editor (App sends it). */
  function takePlanFromEditor() {
    const dialogId = activeDialogId();
    if (!dialogId || sending() || loadingHistory()) return;
    const request = takeImplementPlanRequest();
    if (request) startImplementation(dialogId, request.content, request.path);
  }
  const stopPlanRequests = onImplementPlanRequest(() => takePlanFromEditor());
  onCleanup(stopPlanRequests);

  /** The running request's checklist, for the panel. */
  const runChecklist = createMemo(() => latestChecklist(messages()));
  /** The plan file the running request implements, if any. */
  const runPlanFile = createMemo(() => {
    const list = messages();
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role === 'user' && !m.meta?.harness) {
        return /^Implement the plan in `([^`]+)`/.exec(m.content)?.[1];
      }
    }
    return undefined;
  });

  /** A question waiting in the open chat: the composer answers it. */
  const waitingQuestion = () => {
    const id = store.state.activeId;
    return id ? pendingQuestionsIn(id) : undefined;
  };

  /** The finished plan an "Implement this plan" button goes under, if any. */
  const planMessageId = createMemo(() => {
    const dialogId = store.state.activeId;
    if (!dialogId || sending()) return null;
    const list = messages();
    const last = list[list.length - 1];
    if (!last || last.role !== 'assistant' || last.pending || last.error || last.toolCalls?.length)
      return null;
    // Only once a plan was actually saved (submit_plan): a reply alone isn't one.
    if (!last.meta?.planFile) return null;
    return runModeFor(dialogId) === 'plan' ? last.id : null;
  });

  /** "Run checks" on a verification record that waited for approval. */
  function runApprovedChecks() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    const list = store.messagesOf(dialogId);
    const record = list[list.length - 1];
    if (record?.meta?.verification?.status !== 'needs-approval') return;
    void beginRun(
      dialogId,
      { kind: 'runChecks', recordId: record.id },
      lastUserWireContent(dialogId)
    );
  }

  /** Continue a run that paused at a checkpoint or a loop check. */
  function continueRun() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    void beginRun(dialogId, { kind: 'continueRun' }, lastUserWireContent(dialogId));
  }

  /*
   * -----------------------------------------------------------------------
   * Interrupted tool calls
   * -----------------------------------------------------------------------
   *
   * When the app closes while an approval waits (or mid-way through a
   * turn's tool calls), the stored chat ends with tool calls that never ran
   * and have no result. When such a chat is opened, the run picks up where
   * it stopped: each of those calls asks the user again (manually, whatever
   * the settings say, since the user wasn't there to decide), runs if
   * allowed, and the agent continues.
   */

  /** Tool calls of the chat's last agent step that have no result yet. */
  function unansweredToolCalls(list: ChatMessage[]): ChatToolCall[] {
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role === 'tool') continue;
      if (m.role !== 'assistant' || !m.finalized || m.pending || m.error || !m.toolCalls?.length) {
        return [];
      }
      const answered = new Set(
        list
          .slice(i + 1)
          .filter((x) => x.role === 'tool')
          .map((x) => x.tool_call_id)
      );
      return m.toolCalls.filter((tc) => !answered.has(tc.id));
    }
    return [];
  }

  createEffect(() => {
    const dialogId = activeDialogId();
    const chat = activeChat();
    // Wait until the newest messages are loaded and nothing is running.
    if (!dialogId || !chat || chat.hasNewer || sending() || !activeProfile()) return;
    if (resumed.has(dialogId)) return;
    if (unansweredToolCalls(chat.messages).length === 0) return;
    resumed.add(dialogId);
    untrack(() => void beginRun(dialogId, { kind: 'resume' }, lastUserWireContent(dialogId)));
  });

  /** Stop: the run ends as soon as it can; what it waits on is answered "no". */
  function stopRequest() {
    const dialogId = activeDialogId();
    if (dialogId) void stopRun(dialogId).catch((err) => console.error('Stop failed:', err));
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
      // The IDE's own messages (check failures) aren't the user's request.
      if (m.role !== 'user' || m.failed || m.meta?.harness) continue;
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
    void beginRun(dialogId, { kind: 'retry' }, lastUserWireContent(dialogId));
  }

  /** Continue an interrupted reply. */
  function continueReply() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    void beginRun(dialogId, { kind: 'continueReply' }, lastUserWireContent(dialogId));
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
      const hasLines = files.some((file) => file.lines);
      contextParts.push(
        [
          'The user explicitly referenced these project files:',
          ...files.map((file) =>
            file.lines
              ? `- ${file.relativePath} (lines ${file.lines.start}-${file.lines.end}, shown in the message as ${referenceLabel(file)})`
              : `- ${file.relativePath}`
          ),
          '',
          'Treat these as explicit file references from the user. Use the available file tools to inspect them when relevant.',
          ...(hasLines
            ? [
                'A reference with lines means the user is talking about exactly those lines. Read them with read_file using that start_line and end_line.',
                'To change them, use edit_file with that same start_line and end_line, quoting the current text as old_text: the edit then cannot change anything outside those lines. Do not rewrite the file with write_file.',
              ]
            : []),
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
    // A question is waiting: what's typed is the answer, not a new request.
    const waiting = activeDialogId() ? pendingQuestionsIn(activeDialogId()!) : undefined;
    if (waiting) {
      const reply = readComposerText().trim();
      if (!reply) return;
      clearChatInput();
      answerQuestions(waiting.toolCallId, { reply });
      return;
    }
    const text = input().trim();
    const profile = activeProfile();
    const dialog = activeDialog();
    // From the chips actually in the composer: one deleted with Backspace
    // stays in referencedFiles() and would still be sent.
    const selectedFiles = composerReferences();

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
    const message = {
      id: crypto.randomUUID(),
      content: text,
      meta: {
        mode: chatMode(),
        ...(wireContent && wireContent !== text ? { sentContent: wireContent } : {}),
      },
    };

    /*
     * Save the composer before anything else: from here until the model
     * answers, this is the only copy of what the user typed. The run clears
     * it once the message is answered (lib/agentRuns.ts), or it comes back.
     */
    savePendingDraft({
      dialogId: dialog.id,
      messageId: message.id,
      segments: serializeComposer(),
      createdAt: new Date().toISOString(),
    });

    // Sending always brings the conversation back into view.
    setFocusMessageId(null);

    // Clear right away; the draft brings it back if the request fails.
    clearChatInput();
    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    if (!(await sendMessage(dialog.id, message, wireContent))) {
      if (activeDialogId() === dialog.id) restoreDraftForDialog(dialog.id);
      else clearPendingDraft(dialog.id, message.id);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Undo / redo in the composer (composerHistory).
    const mod = IS_MAC ? e.metaKey : e.ctrlKey;
    const key = e.key.toLowerCase();
    if (mod && !e.altKey && key === 'z') {
      e.preventDefault();
      if (e.shiftKey) redoComposer();
      else undoComposer();
      return;
    }
    if (mod && !e.altKey && !IS_MAC && key === 'y') {
      e.preventDefault();
      redoComposer();
      return;
    }

    // Arrow keys etc. while the list is open: handleSuggestionKeys.
    if (fileSuggestionsOpen()) {
      const suggestions = fileSuggestions();

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
        onContinueRun={continueRun}
        onRunChecks={runApprovedChecks}
        onOpenProjectChecks={() => setSettingsPage('project')}
        planMessageId={planMessageId()}
        onImplementPlan={implementPlan}
      />
      {/* The agent's checklist while it works (read-only). */}
      <Show when={sending() && !waitingQuestion() && runChecklist()}>
        {(items) => <TodoPanel items={items()} plan={runPlanFile()} />}
      </Show>
      {/* The agent's questions, one at a time, above the input. */}
      <Show when={waitingQuestion()} keyed>
        {(pending) => <QuestionPanel pending={pending} />}
      </Show>
      {/* In CLM mode the context row shows the size of the agent's own context. */}
      <Show when={contextModeFor(activeDialogId()) !== 'clm' && contextUsage()}>
        {(usage) => <ContextMeter usage={usage()} />}
      </Show>
      <div class="chat-input-row">
        <div class="chat-input-container">
          <div
            ref={inputTextareaRef as unknown as HTMLDivElement}
            class="chat-input-contenteditable"
            contenteditable
            onInput={(e) =>
              handleComposerInput(
                e.currentTarget,
                (e as InputEvent).inputType,
                (e as InputEvent).data
              )
            }
            // The webview's own undo (e.g. Edit menu on macOS) goes to ours too.
            onBeforeInput={(e) => {
              const type = (e as InputEvent).inputType;
              if (type === 'historyUndo' || type === 'historyRedo') {
                e.preventDefault();
                if (type === 'historyUndo') undoComposer();
                else redoComposer();
              }
            }}
            onPaste={handleComposerPaste}
            onDrop={handleComposerDrop}
            onKeyDown={handleKeyDown}
            data-placeholder={
              !activeProfile()
                ? 'Add a model in settings first'
                : waitingQuestion()
                  ? 'Answer the question above, or type your own answer…'
                  : chatMode() === 'plan'
                    ? 'Describe what to plan…'
                    : 'Message the model...'
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
                    // Never focusable: the input keeps focus and its caret.
                    tabIndex={-1}
                    onMouseDown={(e) => {
                      e.preventDefault();
                    }}
                    onClick={() => acceptFileSuggestion(suggestion)}
                  >
                    <span class="file-suggestion-head">
                      <span class="file-suggestion-name">{referenceLabel(suggestion)}</span>
                      <Show when={suggestion.lines}>
                        <span class="file-suggestion-tag">Selection</span>
                      </Show>
                    </span>
                    <span class="file-suggestion-path">./{suggestion.relativePath}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>

        <Show
          when={sending() && !waitingQuestion()}
          fallback={
            <button
              onClick={() => void handleSend()}
              disabled={
                !activeProfile() ||
                !activeDialog() ||
                (!input().trim() && referencedFiles().length === 0)
              }
              class="chat-send-button"
              title={waitingQuestion() ? 'Send as the answer' : 'Send'}
              aria-label={waitingQuestion() ? 'Send as the answer' : 'Send message'}
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
        <div class="chat-model-select-row">
          <ModeSelector />
        </div>
        <div class="chat-model-select-row">
          <ModelPicker
            providers={providers()}
            selection={selection()}
            onSelect={handleModelChange}
            onOpenSettings={() => setSettingsPage('providers')}
          />
          <Show when={showEffort()}>
            <EffortSelector value={effortFor(activeProfile()?.id)} onChange={setEffort} />
          </Show>
        </div>
        <ContextControls
          dialogId={activeDialogId()}
          sending={sending()}
          onOpenVirtualFiles={() => setSettingsPage('vfs')}
        />
      </div>
      <Show when={settingsPage()} keyed>
        {(page) => (
          <AiSettingsModal
            providers={providers()}
            permissions={permissions()}
            initialPage={page}
            rootPath={props.rootPath()}
            onSave={handleSettingsSaved}
            onClose={() => setSettingsPage(null)}
          />
        )}
      </Show>
    </div>
  );
}
