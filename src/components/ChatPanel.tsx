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
  ApiRoute,
  ChatMessage,
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
  createAiSettingsHolder,
  loadAiSettings,
  reconcileSelection,
  resolveProfile,
} from '../lib/aiSettings';
import { chatStore } from '../lib/chatStore';
import { attachmentUrl, chatApi } from '../lib/chatApi';
import { highlightTitle } from '../lib/chatSearch';
import { generateChatTitle } from '../lib/chatTitle';
import { estimateContext } from '../lib/aiChat';
import { DEFAULT_CONTEXT_TOKENS } from '../lib/contextBudget';
import ContextMeter, { type ContextUsage } from './ContextMeter';
import { chatMode, implementationRequest, setChatMode, type ChatMode } from '../lib/chatMode';
import { answerQuestions, pendingQuestionsIn } from '../lib/questions';
import { notify } from '../lib/notifications';
import { formatBytes, plural } from '../lib/textUtils';
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
import IsolationBar from './IsolationBar';
import { overlayStatus } from '../lib/overlay';
import { barChanged, clearUpload, cloud, onCloudEvent, readOnlyReason } from '../lib/cloud';
import { draftWaits, recoverDrafts, resumesHere, sendKeepingInput } from '../lib/remoteGuards';
import ChatLocation from './ChatLocation';
import UploadProgress from './UploadProgress';
import { waitingChats } from '../lib/chatBadges';
import { isSettled, latestChecklist } from '../lib/todo';
import {
  dismissChecklist,
  liveChecklist,
  refreshChecklist,
  settledChecklist,
} from '../lib/checklist';
import {
  onImplementPlanRequest,
  readPlan,
  relativePlanPath,
  takeImplementPlanRequest,
} from '../lib/plans';
import QuestionPanel from './QuestionPanel';
import {
  configureAgent,
  initAgentRuns,
  isRunning,
  localDate,
  onMessageUnanswered,
  onReasoningLearned,
  onApiRouteLearned,
  onShowChat,
  setRunEffort,
  startRun,
  stopRun,
  toolNotes,
  type AgentConfig,
  type StartKind,
  type StartRequest,
} from '../lib/agentRuns';
import type { ShowTarget } from '../lib/remoteNotices';
import { aiSettingsRevision, openAppSettings } from '../lib/appSettings';
import ModelPicker from './ModelPicker';
import { confirmDialog } from './ConfirmDialog';
import ChatList, { type ChatListRow } from './ChatList';
import MessageList, { type MessageListController } from './MessageList';
import {
  clearPendingDraft,
  listPendingDrafts,
  savePendingDraft,
  takePendingDraft,
  type PendingDraft,
} from '../lib/pendingDraft';
import {
  cancelComposerDraft,
  clearComposerDraft,
  flushComposerDraft,
  loadComposerDraft,
  scheduleComposerDraft,
} from '../lib/composerDraft';
import { toRelativePath } from '../lib/fileTools';
import { DEFAULT_PERMISSIONS, sameModel, type AgentPermissions } from '../lib/permissions';
import { runUsage, usageLabel } from '../lib/subagents';
import {
  DEFAULT_SUBAGENT_SETTINGS,
  resolveSubagentConfig,
  type SubagentSettings,
} from '../lib/subagentSettings';
import { parseComposerImage, type ComposerImage } from '../lib/composerImages';
import {
  ATTACH_ACCEPT,
  attachmentKind,
  MAX_MESSAGE_ATTACHMENTS,
  modelTakes,
  unsupportedReason,
  type AttachmentKind,
} from '../lib/attachmentFormats';
import {
  attachmentSegment,
  fileName,
  parseComposerPdf,
  pastedFiles,
  pasteTakes,
  readClipboard,
  type ComposerPdf,
} from '../lib/composerAttachments';
import {
  attachmentIds,
  messageText,
  pdfsOf,
  segmentKind,
  type ChipSegment,
  type DraftSegment,
} from '../lib/composerSegments';
import {
  copiedSegments,
  copySegments,
  pastedSegments,
  writeSegments,
} from '../lib/composerClipboard';
import ChatInputContextMenu from './ChatInputContextMenu';

const IS_MAC = navigator.platform.toUpperCase().includes('MAC');

const NEW_CHAT_TITLE = 'New Chat';

/** Chats whose interrupted tool calls were offered again this session: once each. */
const resumed = new Set<string>();

/*
 * The context bar above the input (and the CLM budget in the context row)
 * is estimated async, 400ms after mounting. The panel unmounts whenever the
 * sidebar tab switches away from chat, so without a cache the details would
 * blank on every tab switch until the next estimate arrives. The last estimate
 * per chat is kept here and shown at once on remount, then refreshed.
 */
const usageCache = new Map<string, ContextUsage>();
const clmBudgetCache = new Map<string, number | null>();

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
  const [subagentSettings, setSubagentSettings] =
    createSignal<SubagentSettings>(DEFAULT_SUBAGENT_SETTINGS);
  // P5.7: after a failed load, `subagentSettings` are defaults, not what's saved.
  const aiSettings = createAiSettingsHolder({
    setProviders,
    setPermissions,
    setSubagents: setSubagentSettings,
  });

  // Chat list page + open chats (the app's, kept while the panel is away);
  // everything else stays in the database.
  const store = chatStore;
  const activeDialogId = () => store.state.activeId;

  const [input, setInput] = createSignal('');
  /** Images and PDFs in the input (stored ones; those still being added aren't counted). */
  const [attachmentCount, setAttachmentCount] = createSignal(0);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [historyQuery, setHistoryQuery] = createSignal('');
  /** Lags historyQuery slightly so fast typing doesn't search on every key. */
  const [searchQuery, setSearchQuery] = createSignal('');
  /** The open chat's agent run is going on (it runs in the backend). */
  const sending = () => isRunning(activeDialogId());
  /** Chats with a run going on or being started (spinner in the chat list). */
  const runningListIds = createMemo(() => ({
    ...store.state.running,
    ...Object.fromEntries(Object.keys(store.state.starting).map((id) => [id, true as const])),
  }));
  /** A background chat (not the open one) finished with unseen messages. */
  const hasBackgroundUnread = createMemo(() => {
    const active = activeDialogId();
    return Object.keys(store.state.unread).some((id) => id !== active);
  });
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

  const [contextUsage, setContextUsage] = createSignal<ContextUsage | null>(
    (() => {
      const id = chatStore.state.activeId;
      return (id && usageCache.get(id)) || null;
    })()
  );
  // The CLM budget the active chat's next request would get, from the
  // model's context length as it is now (the context row shows it).
  const [clmBudget, setClmBudget] = createSignal<{
    dialogId: string;
    budget: number | null;
  } | null>(
    (() => {
      const id = chatStore.state.activeId;
      return id ? { dialogId: id, budget: clmBudgetCache.get(id) ?? null } : null;
    })()
  );

  let isMounted = true;
  let inputTextareaRef: HTMLTextAreaElement | undefined;

  // This panel is mounted only while the sidebar tab is "chat": while it is,
  // the active chat's messages are on screen, so arriving agent messages in
  // it are seen (no dot); anything else gets a dot until it is opened.
  onMount(() => store.setChatVisible(true));
  onCleanup(() => store.setChatVisible(false));
  createEffect(() => {
    const id = activeDialogId();
    if (id && store.state.chatVisible) store.markRead(id);
  });
  // An isolated chat's changes: looked at again while a run goes and when
  // it ends (IsolationBar), and for a remote chat, when its result came in
  // or landed, or its project synced (lib/cloud.ts).
  const [isolationRevision, setIsolationRevision] = createSignal(0);
  createEffect(
    on(sending, (running) => {
      setIsolationRevision((n) => n + 1);
      if (!running) return;
      const timer = setInterval(() => setIsolationRevision((n) => n + 1), 3000);
      onCleanup(() => clearInterval(timer));
    })
  );
  onCleanup(
    onCloudEvent((event) => {
      if (barChanged(event, activeDialogId())) setIsolationRevision((n) => n + 1);
    })
  );
  // The chat on screen goes first in the queue for heavy work (builds,
  // tests) when several chats want the CPU at once (src-tauri/src/workload).
  createEffect(() => {
    const id = store.state.chatVisible ? (activeDialogId() ?? null) : null;
    void invoke('workload_focus', { dialogId: id }).catch(() => {});
  });

  // The Settings dialog lives in App (gear next to the project selector).
  // Its saves bump aiSettingsRevision — reload our copy (providers /
  // permissions may have changed elsewhere).
  createEffect(
    on(
      aiSettingsRevision,
      () => {
        void (async () => {
          try {
            const settings = await loadAiSettings();
            if (!isMounted) return;
            aiSettings.apply(settings);
            setSelection((sel) => reconcileSelection(settings.providers, sel ?? settings.active));
          } catch (err) {
            console.error('Failed to reload AI settings:', err);
          }
        })();
      },
      { defer: true }
    )
  );

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
    saveChatModel(activeDialogId());
  }

  /*
   * The model and effort are kept with each chat (`dialogs` table), so
   * reopening an old chat continues with what it used. Saved when either
   * is changed in a chat that has messages and when a run starts (an empty
   * chat gets it with its first message); a chat that never saved one
   * (older chats) keeps the current choice.
   */
  let chatModelRequest = 0;
  function saveChatModel(dialogId: string | null | undefined) {
    const sel = selection();
    if (!dialogId || !sel || isEmptyChat(dialogId)) return;
    // A choice made while the chat's saved one loads wins over it.
    chatModelRequest++;
    const profile = resolveProfile(providers(), sel);
    const effort = profile ? effortFor(profile.id) : null;
    void chatApi
      .setModel(dialogId, { providerId: sel.providerId, modelId: sel.modelId, effort })
      .catch((err) => console.error('Chat model not saved:', err));
  }
  createEffect(
    on([activeDialogId, () => providers().length > 0] as const, ([dialogId, loaded]) => {
      if (!dialogId || !loaded) return;
      const request = ++chatModelRequest;
      void chatApi
        .getModel(dialogId)
        .then((saved) => {
          if (!saved || request !== chatModelRequest || activeDialogId() !== dialogId) return;
          const sel = { providerId: saved.providerId, modelId: saved.modelId };
          // Only a model that still exists and is enabled.
          if (!sameModel(reconcileSelection(providers(), sel), sel)) return;
          if (!sameModel(selection(), sel)) setSelection(sel);
          const profile = resolveProfile(providers(), sel);
          if (profile && saved.effort && efforts()[profile.id] !== saved.effort) {
            setEfforts({ ...efforts(), [profile.id]: saved.effort });
          }
        })
        .catch((err) => console.error('Chat model not loaded:', err));
    })
  );

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

  /** Saves the API route a request found the model to work on. */
  function learnApiRoute(profileId: string, route: ApiRoute) {
    const split = profileId.indexOf('::');
    const providerId = profileId.slice(0, split);
    const modelId = profileId.slice(split + 2);
    let changed = false;
    const next = providers().map((p) => {
      if (p.id !== providerId) return p;
      return {
        ...p,
        models: p.models.map((m) => {
          if (m.id !== modelId || m.apiRoute === route) return m;
          changed = true;
          return { ...m, apiRoute: route };
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
   * Unsent input follows the `input` signal, wherever the change came from
   * (typing, chips, paste, undo): debounced into `composer_drafts` per chat
   * as segments, so file/paste chips come back as chips. The DOM is read
   * here (not the signal text, which flattens chips into labels).
   * Programmatic clears for a dialog switch schedule an empty save for the
   * old chat; selectRow/startNewChat cancel it after switching (see below).
   */
  createEffect(
    on(
      [input, activeDialogId],
      ([_text, dialogId]) => {
        if (!dialogId || loadingHistory() || !inputTextareaRef) return;
        scheduleComposerDraft(dialogId, serializeComposer());
      },
      { defer: true }
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
          props.rootPath(),
          efforts(),
          chatMode(),
          permissions(),
          contextModeFor(store.state.activeId),
        ] as const;
      },
      ([dialogId, profile]) => {
        window.clearTimeout(contextTimer);
        if (!dialogId || !profile) {
          // Keep the last estimate for the chat (a remount before the
          // providers load must not blank the bar); clear only with no chat.
          if (!dialogId) {
            setContextUsage(null);
            setClmBudget(null);
          }
          return;
        }
        // Switching chats/tabs: show the last estimate at once, then refresh.
        const cached = usageCache.get(dialogId);
        if (cached) {
          if (cached !== contextUsage()) setContextUsage(cached);
        } else if (contextUsage()) {
          // Another chat's bar: drop it instead of showing it stale.
          setContextUsage(null);
        }
        const cachedBudget = clmBudgetCache.get(dialogId);
        if (cachedBudget !== undefined) {
          const cur = clmBudget();
          if (!cur || cur.dialogId !== dialogId || cur.budget !== cachedBudget)
            setClmBudget({ dialogId, budget: cachedBudget });
        } else {
          const cur = clmBudget();
          if (!cur || cur.dialogId !== dialogId) setClmBudget({ dialogId, budget: null });
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
            setClmBudget({ dialogId, budget: estimate.clmBudget });
            clmBudgetCache.set(dialogId, estimate.clmBudget);
            const known = !!profile.contextTokens && profile.contextTokens > 0;
            const usage = {
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
            };
            usageCache.set(dialogId, usage);
            setContextUsage(usage);
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
   * (.gitignore respected, but .rovik/plans/ always: a plan is there to be
   * referenced, even when the project ignores .rovik/). The file tree only
   * knows folders it has loaded, so in a newly opened project it offered
   * just the root's files. Fetched when "@" opens, at most every few
   * seconds, merged with the tree's.
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
      aiSettings.apply(settings);
      setSelection(reconcileSelection(settings.providers, settings.active));

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
      setDraftsJudged(true);
      const active = activeDialogId();
      if (active) await restoreComposerDraft(active);
    } finally {
      if (isMounted) {
        setLoadingHistory(false);
        // Another folder was opened while the chats loaded: its project.
        await catchUpWithProject();
      }
      if (isMounted) {
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
    if (first) {
      await store.openChat(first);
      await restoreComposerDraft(first.id);
    } else createNewDialog();
  }

  // Another project (another folder, or a copy given its own history): its chats.
  createEffect(
    on(
      () => props.projectId(),
      (id) => {
        if (id === undefined || id === store.projectId() || loadingHistory()) return;
        void switchToProject(id);
      },
      { defer: true }
    )
  );

  /**
   * Shows the project's chats. A folder opened while this (or the first
   * load) was still going is caught up with at the end: the effect above
   * leaves a change alone while history loads, and it doesn't run again
   * for a project that is already the current one.
   */
  async function switchToProject(id: string | null) {
    setLoadingHistory(true);
    try {
      await store.switchProject(id);
      if (isMounted && props.projectId() === id) await openNewestChat();
    } catch (err) {
      setHistoryLoadError(String(err));
    } finally {
      if (isMounted) setLoadingHistory(false);
    }
    await catchUpWithProject();
  }

  /** The project changed while chats were loading: load that one instead. */
  async function catchUpWithProject() {
    const now = props.projectId();
    if (!isMounted || now === undefined || now === store.projectId() || loadingHistory()) return;
    await switchToProject(now);
  }

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
    const dialogId = activeDialogId();
    if (dialogId) void flushComposerDom(dialogId);
    isMounted = false;
  });

  /*
   * App quit kills the webview without unmounting: the debounced save may
   * still be pending and onCleanup may never run. Best-effort flush on
   * hide/close so the last keystrokes (and chips) reach the database.
   */
  const flushOnHide = () => {
    if (document.visibilityState !== 'hidden') return;
    const dialogId = activeDialogId();
    if (dialogId) void flushComposerDom(dialogId);
  };
  window.addEventListener('pagehide', flushOnHide);
  document.addEventListener('visibilitychange', flushOnHide);
  onCleanup(() => {
    window.removeEventListener('pagehide', flushOnHide);
    document.removeEventListener('visibilitychange', flushOnHide);
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
    const prevId = current?.id;
    if (prevId) void flushComposerDom(prevId);
    resetComposerState();
    const dialog = createNewDialog();
    // The reset cleared `input` while the old chat was still active,
    // scheduling an empty save for it; drop it, the flush kept it.
    if (prevId && prevId !== dialog.id) cancelComposerDraft(prevId);
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
    const prevId = activeDialogId();
    if (prevId) await flushComposerDom(prevId);
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
    await restoreComposerDraft(row.dialog.id);
    // The idle checklist panel reads the backend (whole database), not the
    // messages loaded here — refresh on open, so a chat opened from a
    // search hit or with an older checklist page shows the right panel.
    if (!isRunning(row.dialog.id)) void refreshChecklist(row.dialog.id);
    // resetComposerState above cleared `input` while the old chat was still
    // active, scheduling an empty save for it; drop it, the flush kept it.
    if (prevId && prevId !== row.dialog.id) cancelComposerDraft(prevId);
  }

  async function confirmAndDeleteDialog(dialog: DialogSummary) {
    const count = dialog.messageCount;
    // Changes of an isolated chat that haven't landed go with it.
    const unlanded = await overlayStatus(dialog.id)
      .then((s) => (s.isolated ? s.files.length : 0))
      .catch(() => 0);
    const confirmed = await confirmDialog({
      title: 'Delete this chat?',
      message: (
        <>
          <strong>{dialog.title || 'Untitled chat'}</strong>
          {count > 0 ? ` and its ${plural(count, 'message')}` : ''} will be permanently deleted.
          {unlanded > 0
            ? ` So will its ${plural(unlanded, 'changed file')} that haven’t landed in the project.`
            : ''}
        </>
      ),
      confirmLabel: 'Delete',
      danger: true,
    });
    if (confirmed) await deleteDialog(dialog.id);
  }

  async function deleteDialog(id: string) {
    // A chat that is moving can't change until the move ends (6.8): the
    // list would lose it here and keep it there.
    const place = store.summaryOf(id)?.place;
    if (place && place.kind !== 'host') {
      notify({
        kind: 'warning',
        title: 'This chat is moving',
        message: readOnlyReason(place, cloud.hosts) ?? 'Wait until the move ends.',
      });
      return;
    }
    const wasActive = activeDialogId() === id;
    // A remote chat whose host can't be reached: its deletion stops the run
    // there once it can be.
    if (store.isRunning(id)) await stopRun(id).catch(() => undefined);
    takePendingDraft(id);
    cancelComposerDraft(id);
    // A deleted chat leaves search results right away.
    setSearchRows((rows) => rows?.filter((r) => r.dialog.id !== id) ?? null);
    await store.deleteDialog(id);

    if (wasActive) {
      const next = store.state.list.items[0];
      if (next) {
        await store.openChat(next);
        await restoreComposerDraft(next.id);
      } else createNewDialog();
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

  // Quotes too: the chip puts names inside attributes, and a file may be
  // named `a" onmouseover="…`.
  function escapeHtml(text: string): string {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  /** Builds the non-editable "file reference" chip used in the composer. */
  function createFileChip(suggestion: FileSuggestion): HTMLSpanElement {
    const chip = document.createElement('span');
    chip.className = 'chat-file-reference';
    chip.contentEditable = 'false';
    chip.dataset.path = suggestion.path;
    chip.dataset.ref = referenceKey(suggestion);
    // The chip carries its own FileSuggestion: serializeComposer reads it
    // straight from the DOM instead of the referencedFiles() signal, so a
    // save scheduled mid-restore (signal not yet updated, e.g. after an app
    // restart) still stores the chip as a chip, not as its label text.
    chip.dataset.file = JSON.stringify(suggestion);
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

    wireRemove(chip, () => removeReferencedFile(referenceKey(suggestion)));
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

    syncInput();

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
      syncInput();
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
    setAttachmentCount(0);
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
    el.addEventListener('keydown', handleChipDeleteCapture, true);
    onCleanup(() => el.removeEventListener('keydown', handleSuggestionKeys, true));
    onCleanup(() => el.removeEventListener('keydown', handleChipDeleteCapture, true));
  });

  /*
   * Backspace/Delete around a chip, in the capture phase.
   *
   * Chips are contenteditable="false" islands, and the webview's native
   * deletion near them is destructive: measured in headless Chromium, with
   * the caret after a chip plus only its trailing space at the end of a
   * block, native Backspace implodes the whole block — content gone, caret
   * to the start. The component's own onKeyDown goes through Solid's
   * document-level delegation, which runs after the webview already
   * performed that deletion — preventDefault there comes too late.
   * Intercepting here (capture, on the input itself, like
   * handleSuggestionKeys) runs before the native deletion, so
   * planBackspaceDelete/planForwardDelete decide and applyComposerDelete
   * removes by script instead. Cases native handles sanely (plain text,
   * mid-line) stay native: the planners return null there.
   */
  function handleChipDeleteCapture(e: KeyboardEvent) {
    if ((e.key !== 'Backspace' && e.key !== 'Delete') || e.ctrlKey || e.metaKey || e.altKey) return;
    const sel = window.getSelection();
    if (!sel?.isCollapsed || !sel.anchorNode || !inputTextareaRef?.contains(sel.anchorNode)) return;
    const plan = e.key === 'Backspace' ? planBackspaceDelete() : planForwardDelete();
    if (plan) {
      e.preventDefault();
      e.stopPropagation();
      applyComposerDelete(plan);
    }
  }

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
    // One that was still being added then has nothing to come back as.
    for (const old of [...el.querySelectorAll<HTMLElement>('.chat-file-reference')]) {
      if (old.classList.contains('chat-loading-chip')) {
        old.remove();
        continue;
      }
      const segment = chipSegment(old);
      if (segment) old.replaceWith(chipFor(segment));
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
    syncInput();
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

  /** Reads the composer as ordered text + chip segments. */
  function serializeComposer(): DraftSegment[] {
    return inputTextareaRef ? serializeNodes(inputTextareaRef) : [];
  }

  /**
   * What a chip is, as a segment: its own payload, in its data attributes.
   * Null for one still being added, or one that can't be read.
   */
  function chipSegment(chip: HTMLElement): ChipSegment | null {
    if (chip.classList.contains('chat-image-chip')) {
      const image = parseComposerImage(chip.dataset.image);
      return image && { type: 'image', image };
    }
    if (chip.classList.contains('chat-pdf-chip')) {
      const pdf = parseComposerPdf(chip.dataset.pdf);
      return pdf && { type: 'pdf', pdf };
    }
    if (chip.classList.contains('chat-pasted-chip')) {
      const text = knownPastes.get(chip.dataset.pasteId ?? '');
      if (text === undefined) return null;
      const name = chip.dataset.name;
      return name === undefined ? { type: 'pasted', text } : { type: 'pasted', text, name };
    }
    if (chip.classList.contains('chat-loading-chip')) return null;
    // A file reference. The chip's own payload first: it survives restarts
    // and any signal staleness (e.g. a save scheduled mid-restore). The
    // referencedFiles() lookup is only for chips built before the payload
    // was embedded.
    const embedded = chip.dataset.file;
    if (embedded) {
      try {
        const file = JSON.parse(embedded) as FileSuggestion;
        if (file && typeof file.path === 'string') return { type: 'file', file };
      } catch {
        // Fall through to the signal lookup.
      }
    }
    const key = chip.dataset.ref ?? chip.dataset.path ?? '';
    const file = referencedFiles().find((f) => referenceKey(f) === key);
    return file ? { type: 'file', file } : null;
  }

  /** The nodes under `root` (the input, or a copy of its selection) as segments. */
  function serializeNodes(root: Node): DraftSegment[] {
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
          const segment = chipSegment(child);
          if (segment) segments.push(segment);
          // A reference that can't be read keeps its name, as text; an
          // attachment still being added has nothing to keep yet.
          else if (!child.matches('.chat-attachment-chip, .chat-pasted-chip')) {
            pushText(child.textContent ?? '');
          }
          return;
        }
        if (child.tagName === 'BR') {
          // A <br> that is the only/last thing in a line <div> is WebKit's
          // placeholder for an empty line; the <div> already counts as the break.
          const isLinePlaceholder =
            child.parentNode !== root &&
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

    walk(root);
    return segments;
  }

  /**
   * `input` (the message's text, with real line breaks: textContent drops
   * the ones WebKit makes out of <div>/<br>) and `attachmentCount` from the
   * composer as it is now.
   */
  function syncInput() {
    const segments = serializeComposer();
    setInput(messageText(segments));
    setAttachmentCount(attachmentIds(segments).length);
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

    syncInput();
    scheduleComposerDraftSave();
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

  /**
   * A copy of the input or of a message as it was, chips and all; else
   * clipboard text as plain text (code copied from Monaco carries colored
   * HTML), or the clipboard's files as attachments.
   */
  function handleComposerPaste(e: ClipboardEvent) {
    e.preventDefault();
    if (readOnly()) return;
    const data = e.clipboardData;
    const copied = pastedSegments(data);
    if (copied) {
      insertSegments(copied);
      return;
    }
    const text = data?.getData('text/plain') ?? '';
    const files = pastedFiles(data);
    const take = pasteTakes(text, files.length);
    if (take === 'text') pasteText(text);
    else if (take === 'files') void attachFiles(files);
    // Nothing in the event: WebKitGTK leaves an image out of it entirely (no
    // types, no files), but the clipboard API has it.
    else void pasteFromClipboard(true);
  }

  /**
   * Copy and cut in the input: the selection as segments, so its chips
   * paste back as chips (lib/composerClipboard.ts).
   */
  function handleComposerCopy(e: ClipboardEvent) {
    const range = composerRange();
    if (!range || range.collapsed || !e.clipboardData) return;
    e.preventDefault();
    writeSegments(e.clipboardData, serializeNodes(range.cloneContents()));
    if (e.type !== 'cut' || !inputTextareaRef) return;
    range.deleteContents();
    // As a deletion typed there: no `input` event fires for this one.
    handleComposerInput(inputTextareaRef);
  }

  function pasteText(text: string) {
    const clean = text.replace(/\r\n?/g, '\n');
    // Long text as a chip: inserting thousands of lines into the editable
    // input was slow, and every keystroke after it slower still.
    if (clean.length > PASTE_CHIP_CHARS || clean.split('\n').length > PASTE_CHIP_LINES) {
      insertPastedChip(clean);
    } else {
      insertPlainText(clean);
    }
  }

  /** The selection, when it's in the input. */
  function composerRange(): Range | null {
    const container = inputTextareaRef;
    const selection = window.getSelection();
    if (!container || !selection || selection.rangeCount === 0) return null;
    const range = selection.getRangeAt(0);
    return container.contains(range.commonAncestorContainer) ? range : null;
  }

  /** Focuses the input with this selection: WebKit's focus() can put the caret at the start. */
  function focusComposerAt(range: Range | null) {
    const container = inputTextareaRef;
    if (!container) return;
    container.focus();
    if (!range || !container.contains(range.commonAncestorContainer)) return;
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  /**
   * Pastes what the async clipboard API reads, as Ctrl+V would: for the
   * input's menu where the webview doesn't paste by itself (WebView2), and
   * for an image a paste event left out (`imagesOnly`).
   */
  async function pasteFromClipboard(imagesOnly = false) {
    const container = inputTextareaRef;
    if (!container) return;
    const at = composerRange()?.cloneRange() ?? null;
    let content;
    try {
      content = await readClipboard();
    } catch {
      if (!imagesOnly) {
        notify({
          kind: 'warning',
          title: "The clipboard couldn't be read",
          message: `Paste with ${IS_MAC ? '⌘V' : 'Ctrl+V'} instead.`,
          timeout: 6000,
        });
      }
      return;
    }
    if (!container.isConnected) return;
    // Back where the paste was asked for: reading can take the focus (a
    // permission prompt).
    focusComposerAt(at);
    const copied = imagesOnly ? null : copiedSegments(content.text);
    if (copied) {
      insertSegments(copied);
      return;
    }
    const take = pasteTakes(imagesOnly ? '' : content.text, content.images.length);
    if (take === 'text') pasteText(content.text);
    else if (take === 'files') await attachFiles(content.images);
  }

  /*
   * -----------------------------------------------------------------------
   * Attachments: images, PDFs and text files, pasted or picked with the
   * attach button
   * -----------------------------------------------------------------------
   *
   * Each is a chip in the input, its payload in the chip's data attributes
   * (`chipSegment`), so serializing, undo and drafts read it back from the
   * DOM. Images and PDFs are stored as attachments and the message sends
   * their ids (PDFs named in its meta); a text file is a named block of
   * text (`createPastedChip`). Only formats models read can be attached
   * (lib/attachmentFormats.ts), and only those the model is set to take.
   */

  /** A chip's ×: takes the chip out, as one undo step. */
  function wireRemove(chip: HTMLElement, onRemove?: () => void) {
    const remove = chip.querySelector('.chat-file-reference-remove');
    remove?.addEventListener('mousedown', (e) => e.preventDefault());
    remove?.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      onRemove?.();
      chip.remove();
      syncInput();
      scheduleComposerDraftSave();
      recordComposerHistory('other');
    });
  }

  const DOCUMENT_ICON = `<svg class="chat-attachment-icon" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 1.5h6l3 3v10h-9z"/><path d="M9.5 1.5v3h3"/></svg>`;

  /**
   * The chip of an image, a PDF, or a file still being added. Its label,
   * size and × are drawn by CSS from data attributes (styles.css): none of
   * it is the input's text.
   */
  function attachmentChip(
    kind: string,
    label: string,
    size: string | undefined,
    removeLabel: string
  ): HTMLSpanElement {
    const chip = document.createElement('span');
    chip.className = `chat-file-reference chat-attachment-chip ${kind}`;
    chip.contentEditable = 'false';
    chip.innerHTML = `<span class="chat-file-reference-name"></span><span class="chat-file-reference-lines"></span><button type="button" class="chat-file-reference-remove"></button>`;
    const [name, lines, remove] = Array.from(chip.children) as HTMLElement[];
    name.dataset.label = label;
    if (size) lines.dataset.size = size;
    remove.setAttribute('aria-label', removeLabel);
    remove.title = removeLabel;
    wireRemove(chip);
    return chip;
  }

  function createImageChip(image: ComposerImage): HTMLSpanElement {
    const size = image.width && image.height ? `${image.width}×${image.height}` : undefined;
    const chip = attachmentChip('chat-image-chip', 'Image', size, 'Remove image');
    chip.dataset.image = JSON.stringify(image);
    chip.title = size ? `Image, ${size}` : 'Image';
    const thumb = document.createElement('img');
    thumb.className = 'chat-image-chip-thumb';
    thumb.alt = '';
    thumb.draggable = false;
    thumb.src = attachmentUrl(image.id);
    chip.prepend(thumb);
    return chip;
  }

  function createPdfChip(pdf: ComposerPdf): HTMLSpanElement {
    const pages = plural(pdf.pages, 'page');
    const chip = attachmentChip('chat-pdf-chip', pdf.name, pages, 'Remove PDF');
    chip.dataset.pdf = JSON.stringify(pdf);
    chip.title = `${pdf.name}: PDF, ${pages}, ${formatBytes(pdf.size)}`;
    chip.insertAdjacentHTML('afterbegin', DOCUMENT_ICON);
    return chip;
  }

  /** A file being made ready and stored; its chip takes its place once it is. */
  function createLoadingChip(name: string): HTMLSpanElement {
    const chip = attachmentChip('chat-loading-chip', name || 'Image', undefined, 'Cancel');
    chip.title = `Adding ${name || 'the image'}…`;
    return chip;
  }

  /** The chip that shows a segment. */
  function chipFor(segment: ChipSegment): HTMLElement {
    switch (segment.type) {
      case 'file':
        return createFileChip(segment.file);
      case 'pasted':
        return createPastedChip(segment.text, undefined, segment.name);
      case 'image':
        return createImageChip(segment.image);
      case 'pdf':
        return createPdfChip(segment.pdf);
    }
  }

  /** Images, PDFs and text files in the input, those being added included. */
  function attachmentsInComposer(): number {
    return (
      inputTextareaRef?.querySelectorAll('.chat-attachment-chip, .chat-pasted-chip[data-name]')
        .length ?? 0
    );
  }

  /** Says that the model isn't set to take these kinds, and what to do about it. */
  function notifyNotTaken(kinds: Iterable<AttachmentKind>) {
    const profile = activeProfile();
    const list = [...new Set(kinds)].filter((kind) => kind !== 'text');
    if (!profile || list.length === 0) return;
    const what = list.map((kind) => (kind === 'image' ? 'images' : 'PDFs')).join(' or ');
    const setting = list.map((kind) => (kind === 'image' ? 'Vision' : 'PDF')).join(' and ');
    notify({
      kind: 'warning',
      title: `${profile.model} isn't set to accept ${what}`,
      message: `If it does, turn on ${setting} for it in Settings → Model providers, or pick a model that does.`,
      actions: [{ label: 'Open settings', run: () => openAppSettings('providers') }],
      timeout: 12_000,
    });
  }

  /**
   * The items the input can take: attachments of kinds the model takes, as
   * many as a message has room for; what isn't an attachment (kind null)
   * always. Says what was left out, and why.
   */
  function admit<T>(items: T[], kindOf: (item: T) => AttachmentKind | null): T[] {
    const profile = activeProfile();
    const notTaken = new Set<AttachmentKind>();
    let room = MAX_MESSAGE_ATTACHMENTS - attachmentsInComposer();
    let over = 0;
    let added = 0;
    const admitted = items.filter((item) => {
      const kind = kindOf(item);
      if (!kind) return true;
      if (profile && !modelTakes(profile, kind)) {
        notTaken.add(kind);
        return false;
      }
      if (room <= 0) {
        over++;
        return false;
      }
      room--;
      added++;
      return true;
    });
    notifyNotTaken(notTaken);
    if (over > 0) {
      notify({
        kind: 'warning',
        title: `A message can have ${MAX_MESSAGE_ATTACHMENTS} attachments at most`,
        message:
          added > 0
            ? `${plural(added, 'file')} added, the rest left out.`
            : 'Remove one to add another.',
        timeout: 6000,
      });
    }
    return admitted;
  }

  /**
   * Files pasted or picked: each checked (a format models read, a kind the
   * model takes, room in the message), then a chip for each at the caret
   * right away, filled in as each is made ready and stored. One that fails
   * goes away again and says why.
   */
  async function attachFiles(files: Blob[]) {
    const container = inputTextareaRef;
    if (!container || readOnly()) return;
    const unsupported: string[] = [];
    const known = files.flatMap((file) => {
      const like = { name: fileName(file), type: file.type };
      const kind = attachmentKind(like);
      if (kind) return [{ file, kind }];
      unsupported.push(unsupportedReason(like));
      return [];
    });
    if (unsupported.length > 0) {
      notify({
        kind: 'warning',
        title:
          unsupported.length === 1
            ? "The file can't be attached"
            : `${unsupported.length} files can't be attached`,
        message: unsupported.join(' '),
        timeout: 10_000,
      });
    }
    const taken = admit(known, (item) => item.kind);
    if (taken.length === 0) return;
    const chips = taken.map(({ file }) => createLoadingChip(fileName(file)));
    insertAtCaret(chipNodes(chips));
    let added = 0;
    await Promise.all(
      taken.map(async ({ file, kind }, i) => {
        try {
          const segment = await attachmentSegment(file, kind);
          // Removed meanwhile, or another chat opened: nothing to fill.
          if (!chips[i].isConnected) return;
          chips[i].replaceWith(chipFor(segment));
          added++;
        } catch (err) {
          // With the space it was inserted with (chipNodes).
          const after = chips[i].nextSibling;
          if (after instanceof Text && after.data.startsWith(' ')) after.deleteData(0, 1);
          chips[i].remove();
          notify({
            kind: 'error',
            title: `${fileName(file) || 'The image'} couldn't be added`,
            message: err instanceof Error ? err.message : String(err),
            timeout: 8000,
          });
        }
      })
    );
    if (!container.isConnected) return;
    syncInput();
    scheduleComposerDraftSave();
    if (added > 0) recordComposerHistory('other');
  }

  /**
   * A copy of the input or of a message, pasted: its chips come back as
   * chips, the images and PDFs among them as far as admit() lets them.
   */
  function insertSegments(segments: DraftSegment[]) {
    const admitted = admit(segments, segmentKind);
    if (admitted.length === 0) return;
    addReferencedFiles(admitted);
    insertAtCaret(segmentNodes(admitted));
    syncInput();
    scheduleComposerDraftSave();
    recordComposerHistory('other');
  }

  /** The attach button: the file picker, then what's picked is attached at the caret. */
  let fileInputRef: HTMLInputElement | undefined;
  let pickerCaret: Range | null = null;

  function pickFiles() {
    pickerCaret = composerRange()?.cloneRange() ?? null;
    fileInputRef?.click();
  }

  function filesPicked(input: HTMLInputElement) {
    const files = Array.from(input.files ?? []);
    // Picking the same file again is a change too.
    input.value = '';
    if (files.length === 0) return;
    focusComposerAt(pickerCaret);
    void attachFiles(files);
  }

  /*
   * -----------------------------------------------------------------------
   * The input's right-click menu (ChatInputContextMenu)
   * -----------------------------------------------------------------------
   *
   * App takes every right-click to keep the webview's own menu away; the
   * input stops the event before it gets there and opens this one instead.
   */
  const [inputMenu, setInputMenu] = createSignal<{
    x: number;
    y: number;
    hasSelection: boolean;
    hasContent: boolean;
  } | null>(null);
  let inputMenuEl: HTMLDivElement | undefined;
  const closeInputMenu = () => setInputMenu(null);

  function handleComposerContextMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const container = inputTextareaRef;
    if (!container) return;
    const range = composerRange();
    setInputMenu({
      x: e.clientX,
      y: e.clientY,
      hasSelection: !!range && !range.collapsed,
      hasContent: isMeaningful(container),
    });
  }

  /** Kept inside the window, once it's on screen and its size is known. */
  function setInputMenuEl(el: HTMLDivElement) {
    inputMenuEl = el;
    requestAnimationFrame(() => {
      const menu = inputMenu();
      if (!menu || inputMenuEl !== el) return;
      const x = Math.max(0, Math.min(menu.x, window.innerWidth - el.offsetWidth - 10));
      const y = Math.max(0, Math.min(menu.y, window.innerHeight - el.offsetHeight - 10));
      if (x !== menu.x || y !== menu.y) setInputMenu({ ...menu, x, y });
    });
  }

  // Closed by a press or right-click elsewhere, scrolling, any key, or the
  // window losing focus or size: the same as the app's other menus.
  createEffect(() => {
    if (!inputMenu()) return;
    const outside = (e: Event) => {
      if (!inputMenuEl?.contains(e.target as Node)) closeInputMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') e.preventDefault();
      closeInputMenu();
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('contextmenu', outside, true);
    window.addEventListener('scroll', outside, true);
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('blur', closeInputMenu);
    window.addEventListener('resize', closeInputMenu);
    onCleanup(() => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('contextmenu', outside, true);
      window.removeEventListener('scroll', outside, true);
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('blur', closeInputMenu);
      window.removeEventListener('resize', closeInputMenu);
    });
  });

  /** Cut or Copy: the webview's own, as Ctrl+X / Ctrl+C do it (handleComposerCopy). */
  function copyFromComposer(cut: boolean) {
    closeInputMenu();
    const range = composerRange();
    if (!range || range.collapsed) return;
    inputTextareaRef?.focus();
    if (document.execCommand(cut ? 'cut' : 'copy')) return;
    // Not allowed here: through the clipboard API.
    void copySegments(serializeNodes(range.cloneContents())).then((copied) => {
      if (copied) {
        if (cut) document.execCommand('delete');
        return;
      }
      notify({
        kind: 'warning',
        title: "The clipboard couldn't be written",
        message: `Use ${IS_MAC ? '⌘' : 'Ctrl+'}${cut ? 'X' : 'C'} instead.`,
        timeout: 6000,
      });
    });
  }

  /**
   * Paste: where the webview lets the page paste by itself (WebKitGTK with
   * the window's clipboard access, macOS once its own Paste button is
   * pressed), a real paste, taken as Ctrl+V's is (handleComposerPaste),
   * images included. Elsewhere (WebView2) the clipboard API reads it.
   */
  async function pasteFromMenu() {
    closeInputMenu();
    if (readOnly()) return;
    focusComposerAt(composerRange());
    if (document.queryCommandSupported('paste') && document.execCommand('paste')) return;
    await pasteFromClipboard();
  }

  function selectAllInComposer() {
    closeInputMenu();
    const container = inputTextareaRef;
    if (!container) return;
    container.focus();
    const range = document.createRange();
    range.selectNodeContents(container);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  /*
   * Long pastes and attached text files: a chip in the input ("Pasted
   * text", or the file's name), the text itself in memory (knownPastes).
   * Sent in the message wrapped in a tag (lib/textBlocks.ts), shown
   * collapsed in the chat (MessageContent).
   */
  const PASTE_CHIP_CHARS = 2000;
  const PASTE_CHIP_LINES = 30;
  const knownPastes = new Map<string, string>();

  function createPastedChip(
    text: string,
    id: string = crypto.randomUUID(),
    name?: string
  ): HTMLSpanElement {
    knownPastes.set(id, text);
    const lines = text.split('\n').length;
    const preview = text.length > 400 ? `${text.slice(0, 400)}…` : text;
    const chip = document.createElement('span');
    chip.className = 'chat-file-reference chat-pasted-chip';
    chip.contentEditable = 'false';
    chip.dataset.pasteId = id;
    if (name !== undefined) chip.dataset.name = name;
    chip.title = name === undefined ? preview : `${name}\n\n${preview}`;
    chip.innerHTML = `${name === undefined ? '' : DOCUMENT_ICON}<span class="chat-file-reference-name"></span><span class="chat-file-reference-lines">&nbsp;· ${lines.toLocaleString()} line${lines === 1 ? '' : 's'}</span><button type="button" class="chat-file-reference-remove" aria-label="Remove ${name === undefined ? 'pasted text' : 'file'}">\u00d7</button>`;
    chip.querySelector('.chat-file-reference-name')!.textContent = name ?? 'Pasted text';
    wireRemove(chip);
    return chip;
  }

  function insertPastedChip(text: string) {
    insertAtCaret(chipNodes([createPastedChip(text)]));
    syncInput();
    scheduleComposerDraftSave();
    recordComposerHistory('other');
  }

  /**
   * Puts nodes at the caret, in place of what's selected (at the end when
   * the caret isn't in the input); the caret goes after them.
   */
  function insertAtCaret(nodes: DocumentFragment) {
    const container = inputTextareaRef;
    if (!container) return;
    container.focus();
    let range = composerRange();
    if (range) {
      range.deleteContents();
    } else {
      range = document.createRange();
      range.selectNodeContents(container);
      range.collapse(false);
    }
    const last = nodes.lastChild;
    range.insertNode(nodes);
    if (last) range.setStartAfter(last);
    range.collapse(true);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  }

  /** Chips, each followed by a space, as they're inserted. */
  function chipNodes(chips: HTMLElement[]): DocumentFragment {
    const fragment = document.createDocumentFragment();
    for (const chip of chips) fragment.append(chip, document.createTextNode(' '));
    return fragment;
  }

  /** Segments as the input's nodes: text with its line breaks, chips. */
  function segmentNodes(segments: DraftSegment[]): DocumentFragment {
    const fragment = document.createDocumentFragment();
    segments.forEach((segment, index) => {
      if (segment.type === 'text') {
        segment.text.split('\n').forEach((line, i) => {
          if (i > 0) fragment.append(document.createElement('br'));
          if (line) fragment.append(document.createTextNode(line));
        });
        return;
      }
      fragment.append(chipFor(segment));
      // Chips are followed by a space when inserted; keep exactly one.
      const next = segments[index + 1];
      if (!(next?.type === 'text' && /^\s/.test(next.text))) {
        fragment.append(document.createTextNode(' '));
      }
    });
    return fragment;
  }

  /** The file references among segments, added to referencedFiles(). */
  function addReferencedFiles(segments: DraftSegment[]) {
    const files = segments.flatMap((s) => (s.type === 'file' ? [s.file] : []));
    if (files.length === 0) return;
    setReferencedFiles((current) => [
      ...current,
      ...files.filter((f) => !current.some((c) => referenceKey(c) === referenceKey(f))),
    ]);
  }

  /** Same for drag-and-drop of selected text. */
  function handleComposerDrop(e: DragEvent) {
    if (readOnly()) {
      e.preventDefault();
      return;
    }
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
    if (hasContent) container.append(document.createElement('br'));
    container.append(segmentNodes(draft.segments));

    // Signal first: syncInput()/serializeComposer run synchronously
    // below, and any debounced save they schedule must see the files.
    addReferencedFiles(draft.segments);
    syncInput();
    // The restored draft is where undo starts.
    resetComposerHistory();

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
    // A run is going on: its message may still be answered. A remote chat's
    // is judged once its host has caught up (K23).
    if (!stored || store.isRunning(dialogId)) return;
    if (draftWaits(store.summaryOf(dialogId)?.place, cloud.caughtUp)) return;

    const draft = takePendingDraft(dialogId);
    if (draft) queueMicrotask(() => restoreDraftToComposer(draft));
  }

  /*
   * -----------------------------------------------------------------------
   * Composer drafts (unsent input, `composer_drafts` table)
   * -----------------------------------------------------------------------
   *
   * The panel unmounts whenever the sidebar tab switches away from chat,
   * so the contenteditable's DOM text would be lost. Typing schedules a
   * debounced `chat_save_composer_draft` invoke per chat; the active chat's
   * draft is loaded on mount / dialog switch / restart.
   */

  /** Loads the dialog's saved unsent input into the (empty) composer. */
  async function restoreComposerDraft(dialogId: string) {
    const segments = await loadComposerDraft(dialogId);
    if (!segments || segments.length === 0 || !isMounted || activeDialogId() !== dialogId) return;
    queueMicrotask(() => {
      // Chips come back as chips: file references and pasted text are
      // rebuilt by the same path as sent-but-unanswered drafts. Legacy
      // plain-text rows arrive as a single text segment and take the same
      // path (restoreDraftToComposer handles text-only drafts).
      if (!inputTextareaRef || (inputTextareaRef.textContent ?? '').trim().length > 0) return;
      restoreDraftToComposer({
        dialogId,
        messageId: '',
        segments,
        createdAt: new Date().toISOString(),
      });
    });
  }

  /** Schedules a debounced save of what's currently in the composer. */
  function scheduleComposerDraftSave() {
    const dialogId = activeDialogId();
    if (!dialogId || !inputTextareaRef) return;
    scheduleComposerDraft(dialogId, serializeComposer());
  }

  /** Captures the live DOM into the pending draft, then writes it now. */
  async function flushComposerDom(dialogId: string) {
    if (inputTextareaRef) scheduleComposerDraft(dialogId, serializeComposer());
    await flushComposerDraft(dialogId).catch(() => undefined);
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
  // The drafts still to judge: all of them at first, then those left for
  // later (a running chat's, a remote chat's until its host caught up), so
  // a host catching up doesn't judge the others again.
  let draftsToJudge: PendingDraft[] | null = null;
  async function recoverDraftsAfterRestart() {
    const drafts = draftsToJudge ?? listPendingDrafts();
    if (drafts.length === 0) return;
    try {
      // A running chat's and a remote chat's wait (lib/remoteGuards.ts).
      draftsToJudge = await recoverDrafts(drafts, {
        isRunning: (id) => store.isRunning(id),
        placeOf: async (id) => {
          const known = store.summaryOf(id);
          return known ? known.place : (await chatApi.getDialog(id).catch(() => null))?.place;
        },
        caughtUp: cloud.caughtUp,
        answered: (ids) => chatApi.messagesAnswered(ids),
        clear: clearPendingDraft,
        markFailed: markMessageFailed,
      });
    } catch (err) {
      console.error('Draft recovery check failed:', err);
      return;
    }

    const active = activeDialogId();
    if (active) restoreDraftForDialog(active);
  }

  // Drafts are first judged once runs going on are known (onMount); a
  // remote chat's again when its host has caught up (K23).
  const [draftsJudged, setDraftsJudged] = createSignal(false);
  createEffect(
    on(
      [draftsJudged, () => Object.keys(cloud.caughtUp).join(' ')],
      ([judged, hosts]) => {
        if (judged && hosts) void recoverDraftsAfterRestart();
      },
      { defer: true }
    )
  );

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
    // P5.7: after a failed reload, the subagent settings here are
    // defaults: what's saved is read again, never written over.
    void aiSettings
      .save({
        providers: nextProviders,
        active: nextSelection,
        permissions: nextPermissions,
        subagents: subagentSettings(),
      })
      .catch((err) => console.error('Failed to save AI settings:', err));
  }

  function handleModelChange(next: ModelSelection) {
    setSelection(next);
    persistAiSettings(providers(), next);
    saveChatModel(activeDialogId());
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

    const title = await generateChatTitle(profile, chatMessages, dialogId);
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
    subagents: resolveSubagentConfig(subagentSettings(), providers(), resolveProfile),
  });
  createEffect(() => configureAgent(agentConfig()));

  // What replies show about the model's reasoning is kept with it.
  onCleanup(onReasoningLearned((learned) => learnReasoning(learned.profileId, learned)));
  onCleanup(onApiRouteLearned(learnApiRoute));
  // "Show" on a notification of what a chat waits for: that chat, opened
  // if it isn't on screen, or the chat list when several wait.
  onCleanup(onShowChat((target) => void showFromNotice(target)));
  async function showFromNotice(target?: ShowTarget) {
    if (target && 'list' in target) {
      setHistoryOpen(true);
      return;
    }
    const id = target?.dialogId;
    if (id && id !== activeDialogId()) {
      const dialog = store.summaryOf(id) ?? (await chatApi.getDialog(id).catch(() => null));
      if (dialog) await selectRow({ dialog, title: { text: dialog.title, ranges: [] } });
    }
    listController?.followLatest();
  }

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
    const readOnly = readOnlyReason(store.summaryOf(dialogId)?.place, cloud.hosts);
    if (readOnly) {
      notify({ title: 'This chat can’t change now', message: readOnly, timeout: 6000 });
      return false;
    }
    listController?.followLatest();
    try {
      await store.flush();
      saveChatModel(dialogId);
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

  /** The chats an approval or a question waits in, for the list's badges (U03). */
  const waitingIds = createMemo(waitingChats);

  /** Why the open chat can't change now: one that's moving is read-only (U02). */
  const readOnly = () => readOnlyReason(activeDialog()?.place, cloud.hosts);

  /**
   * How a Continue would start `dialogId`, once what's queued for it is
   * stored: a move's run picks up with it, on its host or back here.
   */
  async function moveStart(dialogId: string): Promise<StartRequest | null> {
    resumed.add(dialogId);
    await store.flush();
    const profile = activeProfile();
    if (!profile) return null;
    return {
      dialogId,
      start: { kind: 'continueRun' },
      profile,
      options: reasoningOptions(profile),
      mode: chatMode(),
      contextMode: ensureContextMode(dialogId),
      root: props.rootPath(),
      currentUserContent: lastUserWireContent(dialogId),
      localDate: localDate(),
      config: agentConfig(),
    };
  }

  /** Sends a user message (shown at once; the run stores it first thing). */
  async function sendMessage(
    dialogId: string,
    message: { id: string; content: string; meta: ChatMessage['meta']; attachments?: string[] },
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
    // From its host, for a remote chat (K09).
    const text = file ? await readPlan(dialogId, file).catch(() => null) : null;
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
        meta: { mode: 'agent', sentContent: wire, implementsPlan: true },
      },
      wire
    );
  }

  /**
   * An isolated chat's conflicts with the project, handed to it
   * (IsolationBar): markers are in its files; this asks it to settle them.
   */
  async function askToResolve(
    dialogId: string,
    request: string,
    shown = 'Resolve the conflicts with the project’s changes.'
  ): Promise<boolean> {
    if (!activeProfile() || isRunning(dialogId)) return false;
    setChatMode('agent');
    return sendMessage(
      dialogId,
      {
        id: crypto.randomUUID(),
        content: shown,
        meta: { mode: 'agent', sentContent: request },
      },
      request
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

  /** The running request's checklist, for the panel.
   *
   * Single source of truth (Phase C step 9): while a run goes, the backend's
   * `checklist` event (`liveChecklist`) — not the messages loaded here
   * (a window around a search hit, or a checklist older than the newest
   * page). `latestChecklist(messages())` stays only as the fallback for a
   * `todo` call still streaming. When idle, `chat_checklist`
   * (`settledChecklist`, refreshed on open / run end).
   */
  const runChecklist = createMemo(() => {
    const list = liveChecklist(activeDialogId()) ?? latestChecklist(messages());
    return list && list.length ? list : null;
  });
  /** Open checklist while nothing runs: "Interrupted · N of M done". */
  const idleChecklist = createMemo(() => {
    if (sending()) return null;
    const settled = settledChecklist(activeDialogId());
    if (!settled || settled.items.length === 0) return null;
    if (settled.items.every(isSettled)) return null;
    return settled.items;
  });
  // When no run goes, ask the backend (whole database, carried checklists
  // included): chat opened, and after a run ends (agentRuns also refreshes
  // on runEnded; this covers remounts and chat switches).
  createEffect(
    on(
      () => [activeDialogId(), sending()] as const,
      ([dialogId, running]) => {
        if (!dialogId || running) return;
        if (settledChecklist(dialogId) === undefined) void refreshChecklist(dialogId);
      }
    )
  );
  /** Dismiss the idle panel's ✕: closes the checklist on the message holding it. */
  function dismissIdleChecklist() {
    const dialogId = activeDialogId();
    if (dialogId) void dismissChecklist(dialogId);
  }
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

  createEffect(() => {
    const dialogId = activeDialogId();
    const chat = activeChat();
    // Wait until the newest messages are loaded and nothing is running.
    if (!dialogId || !chat || chat.hasNewer || sending() || !activeProfile()) return;
    if (resumed.has(dialogId)) return;
    // A remote chat's daemon picks its run up itself (K07).
    if (!resumesHere(chat.messages, store.summaryOf(dialogId)?.place)) return;
    resumed.add(dialogId);
    untrack(() => void beginRun(dialogId, { kind: 'resume' }, lastUserWireContent(dialogId)));
  });

  /** Stop: the run ends as soon as it can; what it waits on is answered "no". */
  function stopRequest() {
    const dialogId = activeDialogId();
    if (!dialogId) return;
    void stopRun(dialogId).catch((err) => {
      console.error('Stop failed:', err);
      // A remote chat's host out of reach (6.10): the run goes on there.
      notify({ kind: 'error', title: 'Stop didn’t reach the agent', message: String(err) });
    });
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
                'A reference with lines means the user is talking about exactly those lines. Read them with read using that start_line and end_line.',
                'To change them, use write with that same start_line and end_line, quoting the current text as old_text and its replacement as content: the edit then cannot change anything outside those lines. Do not rewrite the whole file.',
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
    if (readOnly()) return;
    // A question is waiting: what's typed is the answer, not a new request.
    const waiting = activeDialogId() ? pendingQuestionsIn(activeDialogId()!) : undefined;
    if (waiting) {
      const segments = serializeComposer();
      const reply = messageText(segments).trim();
      if (!reply) return;
      const dialogId = activeDialogId();
      clearChatInput();
      if (dialogId) clearComposerDraft(dialogId);
      answerQuestions(waiting.dialogId, waiting.toolCallId, { reply });
      // An answer is text only: images and PDFs stay for the next message.
      const files = segments.filter((s) => s.type === 'image' || s.type === 'pdf');
      if (dialogId && files.length > 0) {
        restoreDraftToComposer({
          dialogId,
          messageId: '',
          segments: files,
          createdAt: new Date().toISOString(),
        });
        scheduleComposerDraftSave();
        notify({
          title: 'The attachments stay in the input',
          message: "An answer to the agent's question is text only. Send them with a message.",
          timeout: 6000,
        });
      }
      return;
    }
    const text = input().trim();
    const profile = activeProfile();
    const dialog = activeDialog();
    // From the chips actually in the composer: one deleted with Backspace
    // stays in referencedFiles() and would still be sent.
    const selectedFiles = composerReferences();

    const segments = serializeComposer();
    const attachments = attachmentIds(segments);

    /*
     * A message containing only referenced files (or attachments) is valid.
     *
     * For example:
     *
     *   [ChatPanel.tsx]
     *
     * can be sent without requiring additional text.
     */
    if (
      !profile ||
      !dialog ||
      sending() ||
      (!text && selectedFiles.length === 0 && attachments.length === 0)
    ) {
      return;
    }
    // Sent without a file the user sees in the input: wait for it.
    if (inputTextareaRef?.querySelector('.chat-loading-chip')) {
      notify({
        title: 'A file is still being added',
        message: 'Send again in a moment.',
        timeout: 4000,
      });
      return;
    }
    // Attached for another model, or before the model's settings changed.
    const notTaken = segments
      .map(segmentKind)
      .filter((kind): kind is AttachmentKind => !!kind && !modelTakes(profile, kind));
    if (notTaken.length > 0) {
      notifyNotTaken(notTaken);
      return;
    }

    /*
     * The displayed/persisted text stays clean. The wire copy, with the
     * active editor context and file references, is what the model gets; it
     * is stored in the message's meta so a retry or "continue" later sends
     * the same thing instead of the bare text. Images and PDFs go as
     * attachments, the PDFs named in the meta.
     */
    const wireContent = buildWireContent(text, selectedFiles);
    const pdfs = pdfsOf(segments);
    const message = {
      id: crypto.randomUUID(),
      content: text,
      meta: {
        mode: chatMode(),
        ...(wireContent && wireContent !== text ? { sentContent: wireContent } : {}),
        ...(pdfs.length > 0 ? { pdfs } : {}),
      },
      ...(attachments.length > 0 ? { attachments } : {}),
    };

    // Sending always brings the conversation back into view.
    setFocusMessageId(null);

    // Clear right away; the input comes back if the request fails.
    clearChatInput();
    setFileSuggestionsOpen(false);
    setFileTriggerStart(null);
    setFileQuery('');
    setSelectedSuggestionIndex(0);

    /*
     * From here until the model answers, the input is kept: in localStorage,
     * which the run clears once the message is answered (lib/agentRuns.ts),
     * and for a remote chat in `composer_drafts` too, until its start
     * succeeded, as its text files can be too big for localStorage (K49).
     */
    const back = await sendKeepingInput(
      { dialogId: dialog.id, messageId: message.id, segments, remote: !!dialog.place },
      () => sendMessage(dialog.id, message, wireContent),
      {
        savePending: savePendingDraft,
        clearPending: clearPendingDraft,
        saveDraft: async (id, saved) => {
          scheduleComposerDraft(id, saved);
          await flushComposerDraft(id);
        },
        clearDraft: clearComposerDraft,
        started: (id) => {
          // What was typed since stays; the sent input goes now.
          if (activeDialogId() !== id) return clearComposerDraft(id);
          scheduleComposerDraftSave();
          void flushComposerDraft(id).catch(() => undefined);
        },
      }
    );
    clearUpload(dialog.id);
    if (back && activeDialogId() === dialog.id) {
      restoreDraftToComposer({
        dialogId: dialog.id,
        messageId: message.id,
        segments: back,
        createdAt: new Date().toISOString(),
      });
    }
  }

  /** Line wrappers the webview puts composer lines in (Enter splits them). */
  function isComposerBlock(n: Node | null): boolean {
    return n instanceof HTMLElement && /^(DIV|P|LI|BLOCKQUOTE|H[1-6]|SECTION)$/.test(n.tagName);
  }

  /** Visible composer content: a chip, non-blank text, or an element holding either. */
  function isMeaningful(n: Node | null): boolean {
    if (!n) return false;
    if (n.nodeType === Node.TEXT_NODE) return /\S/.test(n.textContent ?? '');
    if (n instanceof HTMLElement) {
      if (n.classList.contains('chat-file-reference')) return true;
      if (n.tagName === 'BR') return false;
      return n.querySelector('.chat-file-reference') !== null || /\S/.test(n.textContent ?? '');
    }
    return false;
  }

  /** Only blank text from index to the end (no elements at all). */
  function onlyBlankAfter(el: HTMLElement, from: number): boolean {
    for (let i = from; i < el.childNodes.length; i++) {
      const c = el.childNodes[i];
      if (!(c.nodeType === Node.TEXT_NODE && /^\s*$/.test(c.textContent ?? ''))) return false;
    }
    return true;
  }

  /** Only blank text from the start to index (no elements at all). */
  function onlyBlankBefore(el: HTMLElement, to: number): boolean {
    for (let i = 0; i < Math.min(to, el.childNodes.length); i++) {
      const c = el.childNodes[i];
      if (!(c.nodeType === Node.TEXT_NODE && /^\s*$/.test(c.textContent ?? ''))) return false;
    }
    return true;
  }

  /** Meaningful content after (node, offset) up to the end of its line. */
  function lineHasMeaningfulAfter(node: Node, offset: number, container: HTMLElement): boolean {
    let n: Node | null = node;
    let o = offset;
    for (;;) {
      if (!n || !container.contains(n)) return false;
      if (n.nodeType === Node.TEXT_NODE) {
        if (/\S/.test((n.textContent ?? '').slice(o))) return true;
        const up: Node | null = n.parentNode;
        if (!up || !container.contains(up)) return false;
        o = Array.prototype.indexOf.call(up.childNodes, n) + 1;
        n = up;
        continue;
      }
      const el = n as HTMLElement;
      const len = el.childNodes.length;
      for (let i = Math.min(o, len); i < len; i++) {
        if (isMeaningful(el.childNodes[i])) return true;
      }
      if (el === container || isComposerBlock(el)) return false;
      const up2: Node | null = el.parentNode;
      if (!up2 || !container.contains(up2)) return false;
      o = Array.prototype.indexOf.call(up2.childNodes, el) + 1;
      n = up2;
    }
  }

  /** Meaningful content before (node, offset) back to the start of its line. */
  function lineHasMeaningfulBefore(node: Node, offset: number, container: HTMLElement): boolean {
    let n: Node | null = node;
    let o = offset;
    for (;;) {
      if (!n || !container.contains(n)) return false;
      if (n.nodeType === Node.TEXT_NODE) {
        if (/\S/.test((n.textContent ?? '').slice(0, o))) return true;
        const up: Node | null = n.parentNode;
        if (!up || !container.contains(up)) return false;
        o = Array.prototype.indexOf.call(up.childNodes, n);
        n = up;
        continue;
      }
      const el = n as HTMLElement;
      for (let i = Math.min(o, el.childNodes.length) - 1; i >= 0; i--) {
        if (isMeaningful(el.childNodes[i])) return true;
      }
      if (el === container || isComposerBlock(el)) return false;
      const up: Node | null = el.parentNode;
      if (!up || !container.contains(up)) return false;
      o = Array.prototype.indexOf.call(up.childNodes, el);
      n = up;
    }
  }

  /** What Backspace/Delete next to a chip should remove by script. */
  interface ComposerDeletePlan {
    /** The chip to remove (null: whitespace only). */
    chip: HTMLElement | null;
    /** Blank text nodes between caret and chip, removed with it. */
    spaces: Text[];
    /** No chip: remove exactly one char here instead. */
    trim?: { node: Text; index: number };
  }

  /** The chip containing this node, if any (caret inside chip markup). */
  function enclosingChip(n: Node | null, container: HTMLElement): HTMLElement | null {
    const el = n instanceof HTMLElement ? n : (n?.parentElement ?? null);
    const chip = el?.closest?.('.chat-file-reference') ?? null;
    return chip && container.contains(chip) ? (chip as HTMLElement) : null;
  }

  /**
   * Backspace with a collapsed caret: what to remove by script instead of
   * letting the webview do it.
   *
   * Measured in headless Chromium (the app runs WebKitGTK, same family):
   * with the caret after a chip plus only its trailing space at the end of
   * a block, native Backspace implodes the whole block — content gone,
   * caret to the start. Plain text and mid-line cases delete sanely, so
   * those stay native.
   */
  function planBackspaceDelete(): ComposerDeletePlan | null {
    const sel = window.getSelection();
    const container = inputTextareaRef;
    if (!sel?.isCollapsed || !container) return null;
    const anchor = sel.anchorNode;
    if (!anchor || !container.contains(anchor)) return null;

    // Caret inside a chip's own markup: the whole chip goes.
    const inner = enclosingChip(anchor, container);
    if (inner) return { chip: inner, spaces: [] };

    const startNode = anchor;
    const startOffset = sel.anchorOffset;
    let node: Node = anchor;
    let offset = sel.anchorOffset;
    const spaces: Text[] = [];
    let runLength = 0;

    for (;;) {
      if (node.nodeType === Node.TEXT_NODE) {
        const text = node.textContent ?? '';
        if (/^\s*$/.test(text.slice(0, offset))) {
          if (offset > 0) {
            spaces.push(node as Text);
            runLength += offset;
          }
          const parent = node.parentNode;
          if (!parent || !container.contains(parent)) return null;
          offset = Array.prototype.indexOf.call(parent.childNodes, node);
          node = parent;
          continue;
        }
        return null; // A text char: native deletes it.
      }

      const el = node as HTMLElement;
      const len = el.childNodes.length;
      let i = Math.min(offset, len) - 1;
      while (
        i >= 0 &&
        el.childNodes[i].nodeType === Node.TEXT_NODE &&
        !el.childNodes[i].textContent
      )
        i--;
      if (i < 0) {
        // Start of this element. Leaving a block crosses a line break, so
        // native merges lines instead of touching the chip.
        if (el === container || isComposerBlock(el)) return null;
        const parent = el.parentNode;
        if (!parent || !container.contains(parent)) return null;
        offset = Array.prototype.indexOf.call(parent.childNodes, el);
        node = parent;
        continue;
      }

      const child = el.childNodes[i];
      if (child.nodeType === Node.TEXT_NODE) {
        // Descend; the text branch above collects the whitespace.
        node = child;
        offset = (child.textContent ?? '').length;
        continue;
      }
      if (child instanceof HTMLElement) {
        if (child.classList.contains('chat-file-reference')) {
          // Text after the caret on this line stays: native deletes just
          // the space (measured sane); only at end of line remove by script.
          if (lineHasMeaningfulAfter(startNode, startOffset, container)) return null;
          if (runLength > 1) {
            // More than the auto-inserted space: one press removes one space.
            if (startNode.nodeType === Node.TEXT_NODE && startOffset > 0) {
              return {
                chip: null,
                spaces: [],
                trim: { node: startNode as Text, index: startOffset - 1 },
              };
            }
            const first = spaces[0];
            return {
              chip: null,
              spaces: [],
              trim: { node: first, index: (first.textContent ?? '').length - 1 },
            };
          }
          return { chip: child, spaces };
        }
        if (child.tagName === 'BR') return null;
        if (isComposerBlock(child)) {
          // A whole line before the caret: only dive into its end when
          // nothing at all follows the caret (else native merges lines).
          if (!onlyBlankAfter(el, i + 1)) return null;
          node = child;
          offset = child.childNodes.length;
          continue;
        }
        // Inline wrapper: look inside its end.
        node = child;
        offset = child.childNodes.length;
        continue;
      }
      return null;
    }
  }

  /**
   * Delete with a collapsed caret: the mirror of planBackspaceDelete.
   * Native forward deletion was measured sane wherever text is adjacent;
   * by script only at the start of a line and into line wrappers.
   */
  function planForwardDelete(): ComposerDeletePlan | null {
    const sel = window.getSelection();
    const container = inputTextareaRef;
    if (!sel?.isCollapsed || !container) return null;
    const anchor = sel.anchorNode;
    if (!anchor || !container.contains(anchor)) return null;

    const inner = enclosingChip(anchor, container);
    if (inner) return { chip: inner, spaces: [] };

    const startNode = anchor;
    const startOffset = sel.anchorOffset;
    let node: Node = anchor;
    let offset = sel.anchorOffset;
    const spaces: Text[] = [];
    let runLength = 0;

    for (;;) {
      if (node.nodeType === Node.TEXT_NODE) {
        const text = node.textContent ?? '';
        if (/^\s*$/.test(text.slice(offset))) {
          if (offset < text.length) {
            spaces.push(node as Text);
            runLength += text.length - offset;
          }
          const parent = node.parentNode;
          if (!parent || !container.contains(parent)) return null;
          offset = Array.prototype.indexOf.call(parent.childNodes, node) + 1;
          node = parent;
          continue;
        }
        return null; // A text char: native deletes it.
      }

      const el = node as HTMLElement;
      const len = el.childNodes.length;
      let i = Math.min(offset, len);
      while (
        i < len &&
        el.childNodes[i].nodeType === Node.TEXT_NODE &&
        !el.childNodes[i].textContent
      )
        i++;
      if (i >= len) {
        // End of this element. Leaving a block crosses a line break, so
        // native joins lines instead of touching the chip.
        if (el === container || isComposerBlock(el)) return null;
        const parent = el.parentNode;
        if (!parent || !container.contains(parent)) return null;
        offset = Array.prototype.indexOf.call(parent.childNodes, el) + 1;
        node = parent;
        continue;
      }

      const child = el.childNodes[i];
      if (child.nodeType === Node.TEXT_NODE) {
        // Descend; the text branch above collects the whitespace.
        node = child;
        offset = 0;
        continue;
      }
      if (child instanceof HTMLElement) {
        if (child.classList.contains('chat-file-reference')) {
          // Text before the caret on this line stays: native deletes just
          // the space; only at start of line remove by script.
          if (lineHasMeaningfulBefore(startNode, startOffset, container)) return null;
          if (runLength > 1) {
            if (
              startNode.nodeType === Node.TEXT_NODE &&
              startOffset < (startNode.textContent ?? '').length
            ) {
              return {
                chip: null,
                spaces: [],
                trim: { node: startNode as Text, index: startOffset },
              };
            }
            return { chip: null, spaces: [], trim: { node: spaces[0], index: 0 } };
          }
          return { chip: child, spaces };
        }
        if (child.tagName === 'BR') return null;
        if (isComposerBlock(child)) {
          // A whole line after the caret: only dive into its start when
          // nothing at all precedes the caret (else native merges lines).
          if (!onlyBlankBefore(el, i)) return null;
          node = child;
          offset = 0;
          continue;
        }
        // Inline wrapper: look inside its start.
        node = child;
        offset = 0;
        continue;
      }
      return null;
    }
  }

  /** Removes what planBackspaceDelete/planForwardDelete found, by script. */
  function applyComposerDelete(plan: ComposerDeletePlan) {
    const container = inputTextareaRef;
    if (!container) return;

    const setCaret = (n: Node, o: number) => {
      const s = window.getSelection();
      if (!s) {
        container.focus();
        return;
      }
      const range = document.createRange();
      try {
        range.setStart(n, Math.min(o, (n as Text).length ?? (n as HTMLElement).childNodes.length));
      } catch {
        range.selectNodeContents(container);
        range.collapse(false);
      }
      range.collapse(true);
      s.removeAllRanges();
      s.addRange(range);
    };

    if (plan.trim) {
      const { node, index } = plan.trim;
      if (!node.isConnected) return;
      const text = node.textContent ?? '';
      const next = text.slice(0, index) + text.slice(index + 1);
      if (next.length === 0) {
        const parent = node.parentNode ?? container;
        const at = Array.prototype.indexOf.call(parent.childNodes, node);
        node.remove();
        setCaret(parent, at);
      } else {
        node.textContent = next;
        setCaret(node, index);
      }
    } else if (plan.chip) {
      const chip = plan.chip;
      const parent = chip.parentNode ?? container;
      const at = Array.prototype.indexOf.call(parent.childNodes, chip);
      for (const s of plan.spaces) s.remove();
      const refKey = chip.dataset.ref ?? chip.dataset.path ?? '';
      const isPaste = chip.classList.contains('chat-pasted-chip');
      chip.remove();
      if (!isPaste && refKey) {
        setReferencedFiles((current) => current.filter((file) => referenceKey(file) !== refKey));
      }
      // An emptied composer shows the placeholder again.
      if (
        !container.querySelector('.chat-file-reference') &&
        !(container.textContent ?? '').trim()
      ) {
        container.innerHTML = '';
        setCaret(container, 0);
      } else if (parent.isConnected) {
        setCaret(parent, Math.max(0, Math.min(at, parent.childNodes.length)));
      } else {
        container.focus();
      }
    } else {
      return;
    }

    // No `input` event fires (the key was prevented), so sync manually.
    syncInput();
    scheduleComposerDraftSave();
    recordComposerHistory('other');
    const raw = container.textContent ?? '';
    const s = window.getSelection();
    const caret =
      s?.focusNode && container.contains(s.focusNode)
        ? getTextOffset(container, s.focusNode, s.focusOffset)
        : raw.length;
    updateFileAutocomplete(raw, caret);
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
          <Show when={hasBackgroundUnread()}>
            <span
              class="chat-history-badge"
              title="A background chat finished"
              aria-label="A background chat has new messages"
            />
          </Show>
        </button>

        <span class="chat-title" title={title()}>
          {title()}
        </span>

        <Show when={activeDialog()} keyed>
          {(dialog) => (
            <ChatLocation
              dialog={dialog}
              startFor={moveStart}
              onResynced={(id) => {
                const open = store.summaryOf(id);
                if (open && store.state.activeId === id)
                  void store.openChat(open, { reload: true });
              }}
            />
          )}
        </Show>

        <div class="chat-controls">
          <button class="icon-button" onClick={() => startNewChat()} title="New chat">
            ＋
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
            unreadIds={store.state.unread}
            runningIds={runningListIds()}
            waitingIds={waitingIds()}
            hosts={cloud.hosts}
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
        toolNotes={toolNotes(activeDialogId())}
        onRetry={retryLastTurn}
        onContinue={continueReply}
        onContinueRun={continueRun}
        onRunChecks={runApprovedChecks}
        onOpenProjectChecks={() => openAppSettings('project')}
        planMessageId={planMessageId()}
        onImplementPlan={implementPlan}
        readOnly={!!readOnly()}
      />
      {/* The agent's checklist while it works (read-only). */}
      <Show when={sending() && !waitingQuestion() && runChecklist()}>
        {(items) => <TodoPanel items={items()} plan={runPlanFile()} dialogId={activeDialogId()} />}
      </Show>
      {/* Open checklist while nothing runs: interrupted, with Continue + dismiss. */}
      <Show when={!sending() && !waitingQuestion() && idleChecklist()}>
        {(items) => (
          <TodoPanel
            items={items()}
            plan={runPlanFile()}
            dialogId={activeDialogId()}
            interrupted
            onContinue={continueRun}
            onDismiss={dismissIdleChecklist}
          />
        )}
      </Show>
      {/* An isolated chat's changes that aren't in the project yet. */}
      {/* Keyed: a new bar per chat, so nothing of one chat's acts on another. */}
      <Show when={activeDialogId()} keyed>
        {(id) => (
          <IsolationBar
            dialogId={id}
            revision={isolationRevision()}
            running={isRunning(id)}
            canAskChat={!!activeProfile() && !isRunning(id)}
            onAskChat={askToResolve}
            chatTitle={(other) => store.state.list.items.find((d) => d.id === other)?.title}
            remote={store.summaryOf(id)?.place?.kind === 'host'}
            readOnly={readOnly()}
          />
        )}
      </Show>
      {/* The agent's questions, one at a time, above the input. */}
      <Show when={waitingQuestion()} keyed>
        {(pending) => <QuestionPanel pending={pending} />}
      </Show>
      {/* What this request's subagents used, as their providers reported it. */}
      <Show when={usageLabel(runUsage(messages()))}>
        {(label) => (
          <div class="subagent-run-usage" title="Tokens used by subagents for the latest request">
            Subagents: {label()}
          </div>
        )}
      </Show>
      {/* In CLM mode the context row shows the size of the agent's own context. */}
      <Show when={contextModeFor(activeDialogId()) !== 'clm' && contextUsage()}>
        {(usage) => <ContextMeter usage={usage()} />}
      </Show>
      {/* A remote send's files going up before its message (6.11). */}
      <UploadProgress dialogId={activeDialogId()} />
      <div class="chat-input-row">
        <div class="chat-input-container">
          <div
            ref={inputTextareaRef as unknown as HTMLDivElement}
            class="chat-input-contenteditable"
            contenteditable={readOnly() ? 'false' : 'true'}
            aria-readonly={!!readOnly()}
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
            onCopy={handleComposerCopy}
            onCut={handleComposerCopy}
            onDrop={handleComposerDrop}
            onKeyDown={handleKeyDown}
            onContextMenu={handleComposerContextMenu}
            data-placeholder={
              readOnly()
                ? readOnly()!
                : !activeProfile()
                  ? 'Add a model in settings first'
                  : waitingQuestion()
                    ? 'Answer the question above, or type your own answer…'
                    : chatMode() === 'plan'
                      ? 'Describe what to plan…'
                      : 'Message the model...'
            }
          />

          <Show when={inputMenu()}>
            {(menu) => (
              <ChatInputContextMenu
                ref={setInputMenuEl}
                x={menu().x}
                y={menu().y}
                hasSelection={menu().hasSelection}
                hasContent={menu().hasContent}
                onCut={() => copyFromComposer(true)}
                onCopy={() => copyFromComposer(false)}
                onPaste={() => void pasteFromMenu()}
                onSelectAll={selectAllInComposer}
              />
            )}
          </Show>

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

          {/* Send/stop lives in normal flow in its own row below the text —
              never overlaid — so typed text uses the full width and can
              never slide underneath the button. Attach is at its other end. */}
          <div class="chat-input-actions">
            <button
              type="button"
              class="chat-attach-button"
              disabled={!activeProfile() || !activeDialog() || !!readOnly()}
              title="Attach images, PDFs or text files"
              aria-label="Attach files"
              // The input keeps its focus and caret: the files go there.
              onMouseDown={(e) => e.preventDefault()}
              onClick={pickFiles}
            >
              <svg
                viewBox="0 0 16 16"
                width="14"
                height="14"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M13.5 7.5 8 13a3.5 3.5 0 0 1-5-5l6-6a2.3 2.3 0 0 1 3.3 3.3L6.4 11.2a1.2 1.2 0 0 1-1.7-1.7l5.6-5.6" />
              </svg>
            </button>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept={ATTACH_ACCEPT}
              hidden
              onChange={(e) => filesPicked(e.currentTarget)}
            />
            <Show
              when={sending() && !waitingQuestion()}
              fallback={
                <button
                  onClick={() => void handleSend()}
                  disabled={
                    !activeProfile() ||
                    !activeDialog() ||
                    !!readOnly() ||
                    (!input().trim() && referencedFiles().length === 0 && attachmentCount() === 0)
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
                <svg
                  viewBox="0 0 24 24"
                  width="12"
                  height="12"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <rect x="5" y="5" width="14" height="14" rx="2" />
                </svg>
              </button>
            </Show>
          </div>
        </div>
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
            onOpenSettings={() => openAppSettings('providers')}
          />
          <Show when={showEffort()}>
            <EffortSelector value={effortFor(activeProfile()?.id)} onChange={setEffort} />
          </Show>
        </div>
        <ContextControls
          dialogId={activeDialogId()}
          budget={clmBudget()?.dialogId === activeDialogId() ? clmBudget()!.budget : null}
          sending={sending()}
          onOpenVirtualFiles={() => openAppSettings('vfs')}
        />
      </div>
    </div>
  );
}
