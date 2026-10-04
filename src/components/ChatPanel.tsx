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
  OpenAIChatOptions,
  ModelSelection,
  Provider,
  ToolApproval,
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
import { createChatStore } from '../lib/chatStore';
import { chatApi } from '../lib/chatApi';
import { highlightTitle } from '../lib/chatSearch';
import { generateChatTitle } from '../lib/chatTitle';
import {
  cancelChatRequest,
  estimateContext,
  sendChatMessage,
  streamEndSignal,
  untilStreamEnd,
} from '../lib/aiChat';
import { DEFAULT_CONTEXT_TOKENS } from '../lib/contextBudget';
import ContextMeter, { type ContextUsage } from './ContextMeter';
import { describeFailure, retryReason, toStoredFailure } from '../lib/chatErrors';
import { agentToolsFor, editFileArgs, executeAgentTool } from '../lib/tools';
import { AgentRunGuard } from '../lib/agentGuard';
import { recordAgentEdit } from '../lib/agentChanges';
import {
  chatMode,
  MAX_QUESTIONS_PER_RUN,
  PLAN_BLOCKED_TOOLS,
  PLAN_MODE_INSTRUCTIONS,
  implementationRequest,
  setChatMode,
  type ChatMode,
} from '../lib/chatMode';
import {
  answerQuestions,
  askUser,
  describeOutcome,
  parseQuestions,
  pendingQuestionsIn,
  stopQuestions,
} from '../lib/questions';
import { dismiss, notify } from '../lib/notifications';
import ModeSelector from './ModeSelector';
import EffortSelector from './EffortSelector';
import {
  formatsToSend,
  loadEfforts,
  mergeLearned,
  reasoningStatus,
  saveEfforts,
  type Effort,
} from '../lib/reasoning';
import TodoPanel from './TodoPanel';
import { describeTodo, isSettled, latestChecklist, parseTodo } from '../lib/todo';
import {
  onImplementPlanRequest,
  planProblems,
  relativePlanPath,
  savePlanFile,
  takeImplementPlanRequest,
} from '../lib/plans';
import { findRepetition } from '../lib/repetition';
import QuestionPanel from './QuestionPanel';
import { logAgent } from '../lib/agentLog';
import {
  detectProject,
  effectiveChecks,
  loadProjectSettings,
  projectContext,
  type ProjectCheck,
} from '../lib/projectProfile';
import {
  failureReport,
  isFailed,
  MAX_VERIFY_ROUNDS,
  runChecks,
  type VerificationMeta,
} from '../lib/verification';

/**
 * One agent run: a user message (or a Continue) and every turn after it.
 * Created once, then passed down the turns.
 */
interface AgentRun {
  guard: AgentRunGuard;
  /** The mode of the message that started the run (lib/chatMode.ts). */
  mode: ChatMode;
  /** ask_user calls so far (Plan mode caps them). */
  questionsAsked: number;
  /** The project's checks and the system-prompt section about it. */
  checks: ProjectCheck[];
  projectContext: string;
  /** Project-relative paths written in this run (for the gate and reports). */
  changedFiles: Set<string>;
  /** Files changed since the gate last ran: there is something to check. */
  writesSinceVerify: boolean;
  verifyRounds: number;
  /** The last round failed and was sent back to the agent. */
  lastVerifyFailed: boolean;
  /** Unfinished checklist steps were pointed out once (checkChecklist). */
  checklistNudged: boolean;
  /** Plan mode: the current plan, shown to the model so it revises it. */
  planContext: string;
  /** A plan written in a reply instead of submitted was pointed out once. */
  planNudged: boolean;
  /** Replies stopped for repeating themselves (lib/repetition.ts). */
  repetitionStops: number;
  startedAt: number;
  /** Set once the run's log line is written. */
  ended: boolean;
}
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
import { readFileContent, toolEditFile } from '../lib/commands';

const IS_MAC = navigator.platform.toUpperCase().includes('MAC');

const NEW_CHAT_TITLE = 'New Chat';

/**
 * Chats with an agent run in progress, across ChatPanel instances: the
 * panel unmounts when the sidebar switches tabs while its run goes on.
 */
const runningDialogs = new Set<string>();

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

  /*
   * -----------------------------------------------------------------------
   * Context used (the bar above the input)
   * -----------------------------------------------------------------------
   */

  /** The open project's "## Project" prompt section, as the last run sent it. */
  const [projectContextText, setProjectContextText] = createSignal('');
  const [contextUsage, setContextUsage] = createSignal<ContextUsage | null>(null);

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

  /*
   * Reasoning effort (lib/reasoning.ts): chosen per model in the chat, sent
   * only in the forms the model's provider accepts, which is learned from
   * the replies and kept with the model in the settings.
   */
  const [efforts, setEfforts] = createSignal<Record<string, Effort>>(loadEfforts());
  const effortFor = (profileId: string | undefined): Effort =>
    (profileId && efforts()[profileId]) || 'default';
  function setEffort(effort: Effort) {
    const id = activeProfile()?.id;
    if (!id) return;
    const next = { ...efforts(), [id]: effort };
    setEfforts(next);
    saveEfforts(next);
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
    outcome: { accepted?: string[]; rejected?: string[]; reasons?: boolean }
  ) {
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
          if (outcome.accepted || outcome.rejected) {
            learned = mergeLearned(learned, outcome.accepted ?? [], outcome.rejected ?? []);
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

  // The project section of the prompt, for the estimate before any run.
  createEffect(
    on(
      () => [props.rootPath(), permissions().verifyChanges] as const,
      ([root, verify]) => {
        if (!root) {
          setProjectContextText('');
          return;
        }
        Promise.all([detectProject(root), loadProjectSettings(root)])
          .then(([profile, settings]) => {
            if (props.rootPath() !== root) return;
            setProjectContextText(
              projectContext(profile, effectiveChecks(profile, settings), verify)
            );
          })
          .catch((err) => console.error('Project detection failed:', err));
      }
    )
  );

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
          projectContextText(),
          props.rootPath(),
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
            const tools = agentToolsFor({
              hasProject: !!props.rootPath(),
              vision: profile.vision,
              noTools: profile.noTools,
              permissions: permissions(),
              mode: chatMode(),
            });
            const extraChars =
              (tools.length ? JSON.stringify(tools).length : 0) + projectContextText().length;
            const estimate = await estimateContext({ profile, dialogId, extraChars });
            if (request !== contextRequest) return;
            const known = !!profile.contextTokens && profile.contextTokens > 0;
            setContextUsage({
              promptTokens: estimate.promptTokens,
              windowTokens: known ? profile.contextTokens! : DEFAULT_CONTEXT_TOKENS,
              windowGuessed: !known,
              fullHistoryTokens: estimate.fullHistoryTokens,
              historyTokens: estimate.historyTokens,
              shortened:
                estimate.compacted > 0 ||
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
      if (isMounted) {
        setLoadingHistory(false);
        // "Add to chat" may have opened this panel: its references waited
        // until the chat was loaded (opening one resets the composer).
        insertPendingReferences();
        takePlanFromEditor();
      }
    }
  });

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
      if (m.role === 'user' && !m.failed && !m.meta?.harness) {
        return m.meta?.sentContent ?? m.content;
      }
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
    tc: ChatToolCall,
    /** Resuming an interrupted call: ask the user, with this note. */
    resumeNote?: string
  ): Promise<{
    refusal: string | null;
    approval?: ToolApproval;
    /** Partially accepted edit: the arguments to run instead. */
    argsOverride?: string;
    /** Run as this tool instead (a partly accepted edit_file is written whole). */
    toolOverride?: string;
    /** Added to the tool result for the model. */
    resultNote?: string;
  }> {
    const p = permissions();
    const verdict = checkToolCall(tc.name, tc.arguments, p);
    if (!verdict.permitted) {
      return { refusal: `Error: blocked by the user's permission settings. ${verdict.reason}` };
    }
    // An interrupted call is always confirmed by the user.
    const approver = resumeNote ? 'user' : verdict.approval;
    if (!approver) return { refusal: null };

    const summary = describeToolCall(tc.name, tc.arguments);
    const command = tc.name === 'run_command' ? toolArg(tc.arguments, 'command') : undefined;
    // Edits are diffed once: the AI reviewer reads the diff, the user
    // reviews it in the editor.
    const isFileEdit = tc.name === 'write_file' || tc.name === 'edit_file';
    const prepared = isFileEdit ? await prepareEditReview(tc.name, tc.arguments) : null;
    // Writing the same content changes nothing: no need to ask.
    if (prepared?.kind === 'unchanged') return { refusal: null };
    // An edit_file that can't apply (text not found, outside its lines):
    // the model gets the reason; there's nothing for anyone to approve.
    if (prepared?.kind === 'invalid') return { refusal: prepared.error };
    const edit = prepared?.kind === 'diff' ? prepared.review : null;
    let manualReason: string | null = resumeNote ?? null;

    if (approver === 'ai') {
      const models = approvalProfiles(p);
      const editPath = isFileEdit ? toolArg(tc.arguments, 'path') : undefined;

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
            reviewId: tc.id,
            group: tc.name === 'run_command' ? 'commands' : isFileEdit ? 'edits' : 'tools',
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
    const path = typeof args.path === 'string' ? args.path : '';
    return {
      refusal: null,
      approval: {
        by: 'user',
        approved: true,
        reason: `Accepted ${decision.accepted.length} of ${total} changes.`,
      },
      // edit_file's partial result is a whole new file: written as one.
      argsOverride:
        tc.name === 'edit_file'
          ? JSON.stringify({ path, content: decision.content })
          : JSON.stringify({ ...args, content: decision.content }),
      toolOverride: tc.name === 'edit_file' ? 'write_file' : undefined,
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
   * The diff of a write_file or edit_file call against the file on disk. A
   * new file has nothing to compare against: it's approved as a whole.
   * edit_file's result is computed (not written) by the backend first.
   */
  async function prepareEditReview(
    toolName: string,
    argsJson: string
  ): Promise<
    | { kind: 'diff'; review: EditReview }
    | { kind: 'new' }
    | { kind: 'unchanged' }
    | { kind: 'invalid'; error: string }
    | null
  > {
    const root = props.rootPath();
    const path = toolArg(argsJson, 'path');
    if (!root || !path) return null;
    let content = toolArg(argsJson, 'content');
    if (toolName === 'edit_file') {
      const edit = editFileArgs(parseToolArgsObject(argsJson));
      if (typeof edit === 'string') return { kind: 'invalid', error: edit };
      try {
        content = (await toolEditFile(root, edit, true)).content;
      } catch (err) {
        return { kind: 'invalid', error: `Error: ${String(err)}` };
      }
    }
    if (content === undefined) return null;
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
   */

  /** Wait this long without any output before saying so. */
  const STILL_WAITING_MS = 15_000;
  /** Mid-reply silence before "Still generating" shows. */
  const REPLY_IDLE_MS = 2_500;

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
      /** The run this turn belongs to; the first turn of a run starts one. */
      run?: AgentRun;
      /** Ask the model to continue its interrupted last reply. */
      continueReply?: boolean;
      /**
       * Called once the first turn's outcome is known: true when the model
       * answered (fully or partly), false when nothing came back.
       */
      onSettled?: (ok: boolean) => void;
    } = {}
  ): Promise<void> {
    const run = opts.run ?? (await startRun(dialogId));
    const guard = run.guard;
    const profile = activeProfile();

    if (!profile || !isMounted || stopRequested) {
      opts.onSettled?.(false);
      endRun(dialogId, run, stopRequested ? 'stopped' : 'no-model');
      return;
    }
    const replyStartedAt = Date.now();
    /** The model's reasoning for this reply, shown apart from it. */
    let reasoningText = '';
    let reasoningStartedAt = 0;
    let reasoningEndedAt = 0;
    let reasoningSeen = false;
    let pendingReasoningRender = false;
    /** The message's meta with the reasoning added, for finalizing it. */
    const withReasoning = (meta?: ChatMessage['meta']): ChatMessage['meta'] | undefined =>
      reasoningText
        ? {
            ...(meta ?? {}),
            reasoning: {
              text: reasoningText,
              ms: (reasoningEndedAt || Date.now()) - reasoningStartedAt,
            },
          }
        : meta;
    /** Where a repeating reply was cut (lib/repetition.ts), once noticed. */
    let repetitionCut: number | null = null;
    let repetitionCheckedAt = 0;
    let finishReason: string | null = null;
    let usage: Record<string, unknown> | null = null;

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

    /*
     * Output that stops mid-reply. Some providers send a tool call only once
     * it's complete, so while the model writes a whole file nothing arrives
     * and the chat looked finished after "Let me fix the test file:". Say
     * it's still going, with the time since the last output.
     */
    let lastOutputAt = Date.now();
    let idleShown = false;
    const noteOutput = () => {
      lastOutputAt = Date.now();
      if (idleShown) {
        idleShown = false;
        setStatus(undefined);
      }
    };
    const idleTimer = window.setInterval(() => {
      if (!gotOutput || idleShown || stopRequested || detached) return;
      if (Date.now() - lastOutputAt < REPLY_IDLE_MS) return;
      idleShown = true;
      setStatus({ text: 'Still generating', since: lastOutputAt });
    }, 500);

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
      if (event.type === 'Delta' || event.type === 'Reasoning' || event.type.startsWith('ToolCall'))
        noteOutput();

      switch (event.type) {
        case 'Reasoning':
          markOutput();
          if (!reasoningText) reasoningStartedAt = Date.now();
          reasoningText += event.content;
          if (!reasoningSeen) {
            reasoningSeen = true;
            learnReasoning(profile.id, { reasons: true });
          }
          if (!pendingReasoningRender) {
            pendingReasoningRender = true;
            requestAnimationFrame(() => {
              pendingReasoningRender = false;
              if (!isMounted) return;
              const msg = store.messagesOf(dialogId).find((m) => m.id === pendingMsg.id);
              store.patchMessage(dialogId, pendingMsg.id, {
                meta: { ...(msg?.meta ?? {}), reasoning: { text: reasoningText } },
              });
            });
          }
          break;
        case 'ReasoningSupport':
          learnReasoning(profile.id, { accepted: event.accepted, rejected: event.rejected });
          break;
        case 'Delta':
          markOutput();
          // The reply started: the thinking before it is over.
          if (reasoningText && !reasoningEndedAt) reasoningEndedAt = Date.now();
          assistantText += event.content;
          // Every couple of kilobytes: is it stuck repeating itself?
          if (!repetitionCut && assistantText.length - repetitionCheckedAt > 1500) {
            repetitionCheckedAt = assistantText.length;
            const cut = findRepetition(assistantText);
            if (cut !== null) {
              repetitionCut = cut;
              void cancelChatRequest(requestId);
            }
          }
          if (!pendingRender) {
            pendingRender = true;
            rafId = requestAnimationFrame(() => {
              rafId = null;
              flushAssistantText();
            });
          }
          break;

        case 'Finished':
          finishReason = event.finish_reason;
          usage = event.usage;
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
      mode: run.mode,
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
            projectContext:
              run.mode === 'plan'
                ? [run.projectContext, PLAN_MODE_INSTRUCTIONS, run.planContext]
                    .filter(Boolean)
                    .join('\n\n')
                : run.projectContext,
            options: reasoningOptions(profile),
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
      window.clearInterval(idleTimer);
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

    logAgent({
      event: 'reply',
      dialogId,
      model: profile.model,
      finishReason,
      toolCalls: error ? 0 : toolCalls.length,
      textChars: assistantText.length,
      usage,
      ms: Date.now() - replyStartedAt,
      ...(error ? { error: error.kind } : {}),
    });

    // Stopped for repeating itself: keep the reply up to where it started,
    // tell the model once, and go on; a second time, the run ends.
    if (repetitionCut !== null) {
      run.repetitionStops++;
      await store.finalizeMessage(dialogId, pendingMsg.id, {
        content: `${assistantText.slice(0, repetitionCut).trimEnd()}\n\n_[Stopped here: the reply kept repeating itself.]_`,
        toolCalls: [],
        meta: withReasoning(),
      });
      opts.onSettled?.(true);
      logAgent({
        event: 'run',
        dialogId,
        model: profile.model,
        outcome: 'repetition',
        toolCalls: run.guard.toolCalls,
        filesChanged: run.changedFiles.size,
        verifyRounds: run.verifyRounds,
        ms: 0,
      });
      if (run.repetitionStops >= 2 || stopRequested) {
        notify({
          kind: 'warning',
          title: 'The model kept repeating itself',
          message:
            'Its reply was stopped twice, so the run ended. Try again, or a different model.',
          timeout: 12_000,
        });
        endRun(dialogId, run, 'repetition');
        return;
      }
      await store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'user',
          content:
            '[Automatic check by the IDE, not written by the user]\n' +
            'Your last reply started repeating the same text over and over, so it was stopped. ' +
            "Don't repeat anything. " +
            (run.mode === 'plan'
              ? 'If the plan is ready, submit it once with submit_plan and reply in a sentence or two.'
              : 'Continue with the task from where you were.'),
          finalized: true,
          meta: { harness: 'checklist' },
        },
      ]);
      await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, run });
      return;
    }

    if (error) {
      const stored = toStoredFailure(error);
      if (assistantText.trim()) {
        // Part of the reply arrived: keep it (it stays in the history) and
        // let the user continue. Incomplete tool calls are dropped.
        await store.finalizeMessage(dialogId, pendingMsg.id, {
          content: assistantText,
          toolCalls: [],
          meta: withReasoning({ interrupted: stored }),
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
      endRun(dialogId, run, error.kind === 'cancelled' ? 'stopped' : 'error');
      return;
    }

    // The reply's meta so far (with its reasoning): finalizing keeps only what it's given.
    const finishedMeta = withReasoning(
      store.messagesOf(dialogId).find((m) => m.id === pendingMsg.id)?.meta
    );
    await store.finalizeMessage(dialogId, pendingMsg.id, {
      content: assistantText,
      toolCalls,
      meta: finishedMeta,
    });
    opts.onSettled?.(true);

    if (stopRequested) {
      endRun(dialogId, run, 'stopped');
      return;
    }

    // The model says it's done: the checks decide (verifyAtEnd).
    if (toolCalls.length === 0) {
      if (run.mode === 'plan') {
        if (await finishPlanReply(dialogId, pendingMsg.id, assistantText, run)) {
          await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, run });
          return;
        }
      } else if (await checkChecklist(dialogId, run)) {
        await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, run });
        return;
      }
      const outcome = await verifyAtEnd(dialogId, run);
      if (outcome === 'continue') {
        await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, run });
        return;
      }
      endRun(dialogId, run, outcome);
      return;
    }

    await executeToolCalls(dialogId, toolCalls, undefined, run);
    if (stopRequested) {
      endRun(dialogId, run, 'stopped');
      return;
    }

    // Every tool call has its result, so the chat can wait here as long as
    // it likes: Continue picks up with the next request (continueRun).
    const pause = guard.pause();
    if (pause) {
      await store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'assistant',
          // Empty, no tool calls: kept out of what the model is sent.
          content: '',
          finalized: true,
          meta: { paused: pause },
        },
      ]);
      endRun(dialogId, run, `paused-${pause.kind}`);
      return;
    }

    await runAgentTurn(dialogId, { currentUserContent: opts.currentUserContent, run });
  }

  /**
   * Starts a run: the loop guard, and the project's checks and profile for
   * the system prompt (detected fresh each run: a few file reads).
   */
  async function startRun(dialogId: string, startAtToolCalls = 0): Promise<AgentRun> {
    const mode = runModeFor(dialogId);
    const p = permissions();
    const root = props.rootPath();
    let checks: ProjectCheck[] = [];
    let context = '';
    if (root) {
      try {
        const [profile, settings] = await Promise.all([
          detectProject(root),
          loadProjectSettings(root),
        ]);
        checks = effectiveChecks(profile, settings);
        context = projectContext(profile, checks, p.verifyChanges);
      } catch (err) {
        console.error('Project detection failed:', err);
      }
    }
    setProjectContextText(context);
    return {
      guard: new AgentRunGuard(p.toolCallCheckpoint, startAtToolCalls),
      mode,
      questionsAsked: 0,
      checklistNudged: false,
      planContext: mode === 'plan' ? await currentPlanContext(dialogId) : '',
      planNudged: false,
      repetitionStops: 0,
      checks,
      projectContext: context,
      changedFiles: new Set(),
      writesSinceVerify: false,
      verifyRounds: 0,
      lastVerifyFailed: false,
      startedAt: Date.now(),
      ended: false,
    };
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
   * ask_user: shows the questions in the chat and waits for the answer
   * (lib/questions.ts). Only in Plan mode, a few per run; the answer is
   * the tool's result, kept with the questions in its meta for the card.
   */
  async function answerAskUser(dialogId: string, tc: ChatToolCall, mode: ChatMode, run?: AgentRun) {
    const finish = (content: string, meta?: ChatMessage['meta']) =>
      store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'tool',
          content,
          tool_call_id: tc.id,
          finalized: true,
          meta,
        },
      ]);
    const log = (questions: number, answers: string[]) =>
      logAgent({
        event: 'question',
        dialogId,
        model: activeProfile()?.model ?? '',
        questions,
        answers,
      });

    if (mode !== 'plan') {
      await finish(
        'Error: asking the user is only available in Plan mode. Decide yourself and say what you assumed.'
      );
      return;
    }
    if (run && run.questionsAsked >= MAX_QUESTIONS_PER_RUN) {
      log(0, ['over-limit']);
      await finish(
        `You've already asked ${run.questionsAsked} times in this task. Decide the rest yourself ` +
          'and list your assumptions under "Decisions" in the plan.'
      );
      return;
    }
    const questions = parseQuestions(parseToolArgsObject(tc.arguments));
    if (questions.length === 0) {
      await finish(
        'Error: no question to ask. Pass questions: [{ question, options: [{ label }] }].'
      );
      return;
    }
    if (run) run.questionsAsked++;

    // In the chat; and in the corner, in case the user is working elsewhere.
    const note = notify({
      id: `question-${tc.id}`,
      title:
        questions.length === 1
          ? 'The agent has a question'
          : `The agent has ${questions.length} questions`,
      message: questions[0].question,
      actions: [{ label: 'Show', primary: true, run: () => listController?.followLatest() }],
    });
    const outcome = await askUser({ toolCallId: tc.id, dialogId, questions });
    note.close();
    dismiss(`question-${tc.id}`);

    log(
      questions.length,
      'answers' in outcome
        ? outcome.answers.map((a) => a.kind)
        : 'reply' in outcome
          ? ['reply']
          : ['stopped']
    );
    if ('stopped' in outcome) stopRequest();
    await finish(describeOutcome(questions, outcome), { questions: { questions, outcome } });
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
    const profile = activeProfile();
    if (!profile || sending()) return;
    setChatMode('agent');
    const rel = file ? relativePlanPath(file, props.rootPath()) : undefined;
    const wire = implementationRequest(plan, rel);
    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: rel ? `Implement the plan in \`${rel}\`.` : 'Implement the plan above.',
      finalized: true,
      meta: { mode: 'agent', sentContent: wire },
    };
    listController?.followLatest();
    void runRequest(dialogId, async () => {
      await store.appendMessages(dialogId, [userMsg]);
      await runAgentTurn(dialogId, { currentUserContent: wire });
    });
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

  /** The chat's plan file: the latest one a plan was saved to. */
  function chatPlanFile(dialogId: string): string | undefined {
    return [...store.messagesOf(dialogId)].reverse().find((m) => m.meta?.planFile)?.meta?.planFile;
  }

  /** Plan mode: the current plan (as the file is now), for the model to revise. */
  async function currentPlanContext(dialogId: string): Promise<string> {
    const file = chatPlanFile(dialogId);
    if (!file) return '';
    const text = await readFileContent(file).catch(() => null);
    if (!text?.trim()) return '';
    return [
      '## The current plan',
      `Saved in ${relativePlanPath(file, props.rootPath())}; the user may have edited it. This is the version to revise.`,
      '',
      '<current_plan>',
      text.trim(),
      '</current_plan>',
    ].join('\n');
  }

  /**
   * submit_plan: the plan is checked (planProblems) and, if it holds up,
   * saved to the chat's plan file (a revision replaces it). Problems go back
   * to the model to fix. Only this changes the file: a reply never does.
   */
  async function submitPlan(dialogId: string, tc: ChatToolCall) {
    const finish = (content: string, meta?: ChatMessage['meta']) =>
      store.appendMessages(dialogId, [
        {
          id: crypto.randomUUID(),
          role: 'tool',
          content,
          tool_call_id: tc.id,
          finalized: true,
          meta,
        },
      ]);
    const plan = toolArg(tc.arguments, 'plan') ?? '';
    const problems = planProblems(plan);
    if (problems.length) {
      await finish(
        `Error: the plan wasn't saved. Fix this and call submit_plan again with the complete plan:\n- ${problems.join('\n- ')}`
      );
      return;
    }
    const root = props.rootPath();
    if (!root) {
      await finish("Error: no project is open, so the plan can't be saved.");
      return;
    }
    const request =
      [...store.messagesOf(dialogId)].reverse().find((m) => m.role === 'user' && !m.meta?.harness)
        ?.content ?? 'plan';
    try {
      const path = await savePlanFile({ root, request, plan, existing: chatPlanFile(dialogId) });
      await finish(`Saved the plan to ${relativePlanPath(path, root)}.`, { planFile: path });
    } catch (err) {
      await finish(`Error: the plan couldn't be saved: ${String(err)}`);
    }
  }

  /**
   * The end of a Plan-mode reply. If a plan was submitted in this request,
   * the reply gets its file (for "Implement"). If none was, but the reply
   * reads like a plan, the model is asked once to submit it properly.
   * True: it was asked, the run goes on.
   */
  async function finishPlanReply(dialogId: string, messageId: string, text: string, run: AgentRun) {
    const list = store.messagesOf(dialogId);
    let submitted: string | undefined;
    for (let i = list.length - 1; i >= 0; i--) {
      const m = list[i];
      if (m.role === 'user' && !m.meta?.harness) break;
      if (m.role === 'tool' && m.meta?.planFile) {
        submitted = m.meta.planFile;
        break;
      }
    }
    if (submitted) {
      const msg = list.find((m) => m.id === messageId);
      await store.updateMeta(dialogId, messageId, { ...(msg?.meta ?? {}), planFile: submitted });
      return false;
    }
    const looksLikePlan = /^##\s+.*\bsteps?\b/im.test(text) && /^\s*\d+[.)]\s/m.test(text);
    if (!looksLikePlan || run.planNudged) return false;
    run.planNudged = true;
    await store.appendMessages(dialogId, [
      {
        id: crypto.randomUUID(),
        role: 'user',
        content:
          '[Automatic check by the IDE, not written by the user]\n' +
          "You wrote the plan in your reply, so it wasn't saved. Submit it with submit_plan (the complete plan, " +
          "once), then reply in a sentence or two. Don't write it out again in the reply.",
        finalized: true,
        meta: { harness: 'checklist' },
      },
    ]);
    return true;
  }

  /**
   * The checklist at the end of a run: unfinished steps are pointed out once,
   * and the run goes on. True: the agent was told, continue.
   */
  async function checkChecklist(dialogId: string, run: AgentRun): Promise<boolean> {
    if (run.checklistNudged) return false;
    const items = latestChecklist(store.messagesOf(dialogId));
    const open = (items ?? []).filter((i) => !isSettled(i));
    if (open.length === 0) return false;
    run.checklistNudged = true;
    await store.appendMessages(dialogId, [
      {
        id: crypto.randomUUID(),
        role: 'user',
        content:
          '[Automatic check by the IDE, not written by the user]\n' +
          `Your checklist still has unfinished steps: ${open.map((i) => `"${i.title}"`).join(', ')}. ` +
          'Finish them, or update the checklist marking each skipped or changed with the reason, then finish.',
        finalized: true,
        meta: { harness: 'checklist' },
      },
    ]);
    return true;
  }

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

  /** The run's diagnostics line (lib/agentLog.ts), once. */
  function endRun(dialogId: string, run: AgentRun, outcome: string) {
    if (run.ended) return;
    run.ended = true;
    logAgent({
      event: 'run',
      dialogId,
      model: activeProfile()?.model ?? '',
      outcome,
      toolCalls: run.guard.toolCalls,
      filesChanged: run.changedFiles.size,
      verifyRounds: run.verifyRounds,
      ms: Date.now() - run.startedAt,
    });
  }

  /*
   * -----------------------------------------------------------------------
   * Verification gate
   * -----------------------------------------------------------------------
   *
   * When the model finishes a run in which it changed files, the IDE runs
   * the project's checks. Failures go back to the model as a message it must
   * act on, up to MAX_VERIFY_ROUNDS times; the record of each round shows in
   * the chat (an empty assistant message with meta.verification, which is
   * never sent to the model).
   */

  /** Appends a verification record and returns a way to update it. */
  async function addVerificationRecord(dialogId: string, meta: VerificationMeta) {
    const id = crypto.randomUUID();
    await store.appendMessages(dialogId, [
      { id, role: 'assistant', content: '', finalized: true, meta: { verification: meta } },
    ]);
    // Saved on every change, not just shown: the record has to read the
    // same after a restart.
    return (next: VerificationMeta) => store.updateMeta(dialogId, id, { verification: next });
  }

  /**
   * Runs the gate at the end of a run. 'continue' means the failures were
   * sent to the model and the run goes on; anything else is the outcome.
   */
  async function verifyAtEnd(
    dialogId: string,
    run: AgentRun,
    opts: {
      approved?: boolean;
      /** The record that waited for approval: updated in place, not a new one. */
      recordId?: string;
    } = {}
  ): Promise<string> {
    const p = permissions();
    const root = props.rootPath();
    if (!p.verifyChanges || !root) return 'done';
    const changedFiles = [...run.changedFiles];
    const base = { round: run.verifyRounds, maxRounds: MAX_VERIFY_ROUNDS, changedFiles };

    if (!run.writesSinceVerify) {
      // Failures were sent back, and the model finished without touching
      // anything: it decided not to (or couldn't). Say so; don't loop.
      if (run.lastVerifyFailed) {
        await addVerificationRecord(dialogId, { ...base, status: 'unresolved', checks: [] });
        return 'verify-unresolved';
      }
      return 'done';
    }

    const checks = run.checks.filter((c) => c.enabled && c.command.trim());
    if (checks.length === 0) {
      await addVerificationRecord(dialogId, {
        ...base,
        status: 'unverified',
        checks: [],
        reason: 'no checks are set up for this project',
      });
      run.writesSinceVerify = false;
      return 'unverified';
    }

    // Command approval is on: the gate's commands need the user's go too.
    if (p.commandApproval.required && !opts.approved) {
      await addVerificationRecord(dialogId, {
        ...base,
        status: 'needs-approval',
        checks: checks.map((c) => ({ kind: c.kind, command: c.command, status: 'pending' })),
      });
      return 'verify-needs-approval';
    }

    run.verifyRounds++;
    run.writesSinceVerify = false;
    const round = run.verifyRounds;
    let meta: VerificationMeta = { ...base, round, status: 'running', checks: [] };
    const update = opts.recordId
      ? ((id: string) => {
          const set = (next: VerificationMeta) =>
            store.updateMeta(dialogId, id, { verification: next });
          void set(meta);
          return set;
        })(opts.recordId)
      : await addVerificationRecord(dialogId, meta);

    const results = await runChecks(root, checks, p, {
      changedFiles,
      shouldStop: () => stopRequested || !isMounted,
      onProgress: (progress) => {
        meta = { ...meta, checks: progress };
        update(meta);
      },
    });

    const failed = results.some(isFailed);
    const ran = results.some((r) => r.status === 'passed' || r.status === 'failed');
    const status: VerificationMeta['status'] = stopRequested
      ? 'stopped'
      : !ran
        ? 'unverified'
        : !failed
          ? 'passed'
          : round >= MAX_VERIFY_ROUNDS
            ? 'gave-up'
            : 'failed';
    meta = {
      ...meta,
      checks: results,
      status,
      ...(status === 'unverified' ? { reason: 'none of the checks were allowed to run' } : {}),
    };
    update(meta);
    logAgent({
      event: 'verify',
      dialogId,
      round,
      status,
      checks: results.map((r) => ({
        kind: r.kind,
        command: r.command,
        status: r.status,
        exitCode: r.exitCode,
        ms: r.durationMs,
      })),
    });

    run.lastVerifyFailed = failed;
    if (status !== 'failed') {
      return status === 'passed' ? 'verified' : `verify-${status}`;
    }

    // Back to the model. User-role, so every provider accepts it after an
    // assistant reply; marked as the IDE's, so it's never shown as the
    // user's message or replaced by the request's editor context.
    await store.appendMessages(dialogId, [
      {
        id: crypto.randomUUID(),
        role: 'user',
        content: failureReport(results, changedFiles, round),
        finalized: true,
        meta: { harness: 'verification' },
      },
    ]);
    return 'continue';
  }

  /** "Run checks" on a record that waited for approval. */
  function runApprovedChecks() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    const list = store.messagesOf(dialogId);
    const record = list[list.length - 1];
    const pending = record?.meta?.verification;
    if (!pending || pending.status !== 'needs-approval') return;
    listController?.followLatest();
    const currentUserContent = lastUserWireContent(dialogId);
    void runRequest(dialogId, async () => {
      const run = await startRun(dialogId);
      for (const f of pending.changedFiles) run.changedFiles.add(f);
      run.writesSinceVerify = true;
      run.verifyRounds = pending.round;
      // The waiting record becomes the running one, then the results.
      const outcome = await verifyAtEnd(dialogId, run, { approved: true, recordId: record.id });
      if (outcome === 'continue') {
        await runAgentTurn(dialogId, { currentUserContent, run });
      } else {
        endRun(dialogId, run, outcome);
      }
    });
  }

  /** Continue a run that paused at a checkpoint or a loop check. */
  function continueRun() {
    const dialogId = activeDialogId();
    if (!dialogId || sending()) return;
    const list = store.messagesOf(dialogId);
    const last = list[list.length - 1];
    const paused = last?.meta?.paused;
    listController?.followLatest();
    // Counting goes on from where it paused; loop evidence starts fresh.
    const currentUserContent = lastUserWireContent(dialogId);
    void runRequest(dialogId, async () => {
      const run = await startRun(dialogId, paused?.toolCalls ?? 0);
      await runAgentTurn(dialogId, { currentUserContent, run });
    });
  }

  /**
   * Authorizes, runs and records each tool call; each result is stored
   * before the next turn, which reads the history from the database.
   * Every call gets a result, even after Stop: providers reject a history
   * with tool calls left unanswered.
   */
  async function executeToolCalls(
    dialogId: string,
    toolCalls: ChatToolCall[],
    /** Interrupted calls being resumed: always asked manually, with this note. */
    resumeNote?: string,
    /** The run: loop checks record each finished call, writes are tracked. */
    run?: AgentRun
  ) {
    const guard = run?.guard;
    const root = props.rootPath();
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

    const mode = run?.mode ?? runModeFor(dialogId);
    for (const tc of toolCalls) {
      if (stopRequested) {
        await stoppedResult(tc);
        continue;
      }
      // The plan (Plan mode): checked, then saved; problems go back to the model.
      if (tc.name === 'submit_plan') {
        await submitPlan(dialogId, tc);
        continue;
      }
      // The checklist: shown in the panel, never asks, isn't work the loop
      // checks or the pause limit count.
      if (tc.name === 'todo') {
        const items = parseTodo(parseToolArgsObject(tc.arguments));
        await store.appendMessages(dialogId, [
          {
            id: crypto.randomUUID(),
            role: 'tool',
            content: describeTodo(items),
            tool_call_id: tc.id,
            finalized: true,
          },
        ]);
        continue;
      }
      // Questions to the user: answered in the chat, no approval involved
      // (a resumed one after a restart is simply asked again).
      if (tc.name === 'ask_user') {
        await answerAskUser(dialogId, tc, mode, run);
        continue;
      }
      // Plan mode doesn't offer these; a model that calls them anyway is told why.
      if (mode === 'plan' && PLAN_BLOCKED_TOOLS.has(tc.name)) {
        await store.appendMessages(dialogId, [
          {
            id: crypto.randomUUID(),
            role: 'tool',
            content:
              `Error: ${tc.name} isn't available in Plan mode, which doesn't change anything. ` +
              'Finish the plan; the user implements it in Agent mode.',
            tool_call_id: tc.id,
            finalized: true,
          },
        ]);
        continue;
      }
      const { refusal, approval, argsOverride, toolOverride, resultNote } = await authorizeToolCall(
        dialogId,
        tc,
        resumeNote
      );
      // Stop may have been pressed while approval was pending.
      if (stopRequested && (refusal || !approval)) {
        await stoppedResult(tc);
        continue;
      }
      const args = argsOverride ?? tc.arguments;
      const toolName = toolOverride ?? tc.name;
      const writes = !refusal && (toolName === 'write_file' || toolName === 'edit_file');
      // The file before a write, so the guard can tell an edit undoing earlier ones.
      const writePath = writes ? toolArg(args, 'path') : undefined;
      const before =
        writePath && root
          ? await readFileContent(toAbsolutePath(writePath, root)).catch(() => null)
          : null;
      const result = refusal
        ? { content: refusal, attachments: undefined, written: undefined }
        : await executeAgentTool(root, toolName, args);
      if (resultNote && !result.content.startsWith('Error:')) result.content += resultNote;

      const writeFailed = result.content.startsWith('Error:');
      const after =
        result.written?.content ??
        (toolName === 'write_file' ? toolArg(args, 'content') : undefined);
      // An edit the user didn't review shows in the editor until they save.
      if (writes && !writeFailed && writePath && root && after !== undefined) {
        if (approval?.by !== 'user') {
          recordAgentEdit(toAbsolutePath(writePath, root), before, after);
        }
      }

      if (guard) {
        const failed = writeFailed;
        const verdict = guard.record({
          name: tc.name,
          argsJson: tc.arguments,
          result: result.content,
          write:
            writes && !failed && writePath && after !== undefined
              ? { path: writePath, before, after }
              : undefined,
        });
        // The model reads the note with the result; a stop pauses the run
        // once this turn's calls are done (runAgentTurn).
        if (verdict.kind !== 'ok') result.content += verdict.note;
      }
      if (run && writes && writePath && !result.content.startsWith('Error:')) {
        run.changedFiles.add(writePath);
        run.writesSinceVerify = true;
      }

      // Open tabs of the file show what was written.
      if (!refusal && root && !result.content.startsWith('Error:')) {
        if (result.written) {
          notifyFileWritten(toAbsolutePath(result.written.path, root), result.written.content);
        } else if ((toolOverride ?? tc.name) === 'write_file') {
          const path = toolArg(args, 'path');
          const content = toolArg(args, 'content');
          if (path && content !== undefined) notifyFileWritten(toAbsolutePath(path, root), content);
        }
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
  }

  /*
   * -----------------------------------------------------------------------
   * Interrupted tool calls
   * -----------------------------------------------------------------------
   *
   * Approvals live in memory. When the app closes while one waits (or
   * mid-way through a turn's tool calls), the stored chat ends with tool
   * calls that never ran and have no result. When such a chat is opened,
   * the run picks up where it stopped: each of those calls asks the user
   * again (manually, whatever the settings say, since the user wasn't
   * there to decide), runs if allowed, and the agent continues.
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

  /** Chats already resumed in this session: each is offered once. */
  const resumed = new Set<string>();

  createEffect(() => {
    const dialogId = activeDialogId();
    const chat = activeChat();
    // Wait until the newest messages are loaded and nothing is running.
    if (!dialogId || !chat || chat.hasNewer || chat.busy || sending()) return;
    if (resumed.has(dialogId) || runningDialogs.has(dialogId)) return;
    const calls = unansweredToolCalls(chat.messages);
    if (calls.length === 0) return;
    resumed.add(dialogId);
    untrack(() => void resumeInterrupted(dialogId, calls));
  });

  async function resumeInterrupted(dialogId: string, calls: ChatToolCall[]) {
    listController?.followLatest();
    await runRequest(dialogId, async () => {
      await executeToolCalls(
        dialogId,
        calls,
        'Interrupted: the app was closed before this ran. Allow it to continue the task.'
      );
      if (stopRequested || !isMounted) return;
      await runAgentTurn(dialogId, { currentUserContent: lastUserWireContent(dialogId) });
    });
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
    runningDialogs.add(dialogId);
    try {
      await run();
    } finally {
      runningDialogs.delete(dialogId);
      window.clearTimeout(stopTimer);
      store.setBusy(dialogId, false);
      if (isMounted) setSending(false);
      await flushDialogPersistence();
    }
  }

  /** Stop: cancels the running request and any further tool steps. */
  function stopRequest() {
    stopRequested = true;
    // Waiting approvals count as denied, waiting questions as stopped.
    denyAll();
    stopQuestions();
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

    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
      finalized: true,
      meta: {
        mode: chatMode(),
        ...(wireContent && wireContent !== text ? { sentContent: wireContent } : {}),
      },
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
      <Show when={contextUsage()}>{(usage) => <ContextMeter usage={usage()} />}</Show>
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
