// agentRuns.ts
//
// The agent runs in the backend (src-tauri/src/agent_run): a run goes on
// whatever the UI does, switching the sidebar away from the chat included.
// This module starts and stops runs and follows them. It listens to the
// backend's `agent-event`s once, at the module level, so nothing is missed
// while the chat panel isn't mounted:
//
//   - messages the run stores, and changes to them, go into the chat store;
//   - approvals and questions it waits on go to lib/approvals.ts and
//     lib/questions.ts, where the chat and the editor answer them, and
//     lib/remoteNotices.ts tells of them in the corner;
//   - files it writes refresh open tabs, and unreviewed edits are marked;
//   - what a reply showed about the model's reasoning goes to the panel,
//     which keeps it in the settings (queued until the panel is there);
//   - the CLM state each request went out with goes to lib/contextMode.ts.
//
// The events' shapes are rovik-protocol's (src-tauri/crates/rovik-protocol):
// test-fixtures/agent-events.json holds one of each for both sides' tests.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createStore, produce } from 'solid-js/store';
import type { ApiRoute, ChatMessage, ModelProfile, OpenAIChatOptions } from '../types';
import type { AgentPermissions } from './permissions';
import { chatStore, type MessagePatch, type MessageRow } from './chatStore';
import { noteChecklist, clearLiveChecklist, refreshChecklist } from './checklist';
import {
  clearApproval,
  notifyFileDeleted,
  notifyFileWritten,
  pendingApproval,
  registerApproval,
  type ApprovalRequest,
} from './approvals';
import {
  clearQuestions,
  pendingQuestions,
  registerQuestions,
  type PendingQuestions,
} from './questions';
import {
  approvalNoticeId,
  initRemoteNotices,
  questionNoticeId,
  tellWaiting,
  waitingSettled,
  type ShowTarget,
  type Waiting,
} from './remoteNotices';
import { clearAgentChanges, recordAgentEdit } from './agentChanges';
import { clearPendingDraft } from './pendingDraft';
import { dismiss, notify } from './notifications';
import { noteClmStatus, type ClmStatus } from './contextMode';
import {
  isSubagentChat,
  seedRunningSubagent,
  type RunningSubagent,
  type SubagentEnded,
  type SubagentProgress,
  type SubagentStarted,
  noteSubagentEnded,
  noteSubagentProgress,
  noteSubagentStarted,
  patchSubagentMessage,
  receiveSubagentMessages,
} from './subagents';
import type { SubagentConfig } from './subagentSettings';
import type { AutopilotConfig } from './autopilotSettings';

/** Settings runs follow (agent_run::AgentConfig). */
export interface AgentConfig {
  permissions: AgentPermissions;
  /** The AI reviewer, then its fallbacks: enabled models only. */
  approvalProfiles: ModelProfile[];
  /** Subagents (lib/subagentSettings.ts), resolved. */
  subagents: SubagentConfig;
  /** How chats in Autopilot run their tasks (lib/autopilotSettings.ts). */
  autopilot: AutopilotConfig;
}

export type StartKind =
  | {
      kind: 'send';
      message: {
        id: string;
        content: string;
        meta?: ChatMessage['meta'];
        /** Attached images' and PDFs' ids (lib/composerSegments.ts); `meta.pdfs` names the PDFs. */
        attachments?: string[];
      };
    }
  | { kind: 'retry' }
  | { kind: 'continueReply' }
  | { kind: 'continueRun' }
  | { kind: 'runChecks'; recordId: string }
  | { kind: 'resume' };

export interface StartRequest {
  dialogId: string;
  start: StartKind;
  profile: ModelProfile;
  options?: OpenAIChatOptions;
  /** The mode selected in the chat, for a run whose message didn't record one. */
  mode: import('./chatMode').ChatMode;
  contextMode: 'trimming' | 'clm';
  root: string | null;
  /** The request's user message as sent (with editor context). */
  currentUserContent?: string;
  /** Today on the user's clock, "2026-10-04" (plan file names). */
  localDate: string;
  config: AgentConfig;
}

export interface ReasoningLearned {
  profileId: string;
  accepted: string[];
  rejected: string[];
  unresolved: string[];
  reasons: boolean;
}

type AgentEvent =
  | { type: 'runStarted'; dialogId: string; runId: string }
  | { type: 'runEnded'; dialogId: string; runId: string; outcome: string }
  | { type: 'messagesAdded'; dialogId: string; messages: (MessageRow & { pending?: boolean })[] }
  | { type: 'messagePatched'; dialogId: string; messageId: string; patch: MessagePatch }
  | { type: 'approvalRequested'; request: ApprovalRequest }
  | { type: 'approvalSettled'; dialogId: string; toolCallId: string }
  | { type: 'questionsAsked'; request: PendingQuestions }
  | { type: 'questionsSettled'; dialogId: string; toolCallId: string }
  | { type: 'toolNote'; dialogId: string; toolCallId: string; note: string | null }
  | { type: 'checklist'; dialogId: string; items: import('./todo').TodoItem[] }
  | {
      type: 'fileWritten';
      /** The chat whose run wrote it. */
      dialogId?: string;
      absPath: string;
      before: string | null;
      content: string;
      reviewed: boolean;
      /** Removed: a rollback of a file the run created. */
      deleted?: boolean;
    }
  | ({ type: 'reasoningLearned' } & ReasoningLearned)
  | { type: 'apiRouteLearned'; profileId: string; route: ApiRoute }
  | { type: 'userMessageSettled'; dialogId: string; messageId: string; ok: boolean }
  | { type: 'contextUpdated'; dialogId: string; status: ClmStatus }
  | {
      type: 'notice';
      /** The chat it's about, if it's about one. */
      dialogId?: string;
      kind: 'info' | 'success' | 'warning' | 'error';
      title: string;
      message: string;
    }
  | ({ type: 'subagentStarted' } & SubagentStarted)
  | ({ type: 'subagentProgress' } & SubagentProgress)
  | ({ type: 'subagentEnded' } & SubagentEnded);

const [state, setState] = createStore<{
  /**
   * Live notes on tool calls, e.g. "Reviewing with …", by chat, then tool
   * call id (two chats can have calls with the same id).
   */
  toolNotes: Record<string, Record<string, string>>;
}>({ toolNotes: {} });

/** The chat's live tool call notes. */
export const toolNotes = (dialogId: string | null): Record<string, string> | undefined =>
  dialogId ? state.toolNotes[dialogId] : undefined;

/*
 * ---------------------------------------------------------------------------
 * Commands
 * ---------------------------------------------------------------------------
 */

/**
 * Starts a run; resolves with its id, or null when there was nothing to do.
 * The chat counts as running from now on, so it can't be started twice.
 */
export async function startRun(request: StartRequest): Promise<string | null> {
  // A new run shows only its own checklist: whatever an earlier run left
  // live goes now, before the backend can send this run's first one (a
  // carried checklist arrives while the run is prepared, before
  // runStarted, so clearing there would drop it).
  clearLiveChecklist(request.dialogId);
  chatStore.setStarting(request.dialogId, true);
  try {
    return await invoke<string | null>('agent_start', { request });
  } finally {
    chatStore.setStarting(request.dialogId, false);
  }
}

/** Stop: the run ends as soon as it can; what it waits on is answered "no". */
export function stopRun(dialogId: string): Promise<boolean> {
  return invoke<boolean>('agent_stop', { dialogId });
}

/**
 * A reasoning effort picked for a model in a chat: runs using the model take
 * it for their next request. The run in that chat also asks again with it at
 * once if its request stopped answering before any of the reply arrived
 * (waiting long for it, stalled, between retries).
 */
export function setRunEffort(
  profileId: string,
  options: OpenAIChatOptions | undefined,
  dialogId: string | null
): void {
  void invoke('agent_set_effort', { profileId, options: options ?? null, dialogId }).catch((err) =>
    console.error('Effort not sent to the agent:', err)
  );
}

export function configureAgent(config: AgentConfig): void {
  void invoke('agent_configure', { config }).catch((err) =>
    console.error('Agent settings not sent:', err)
  );
}

export function isRunning(dialogId: string | null | undefined): boolean {
  return !!dialogId && chatStore.isRunning(dialogId);
}

/** Today on the user's clock, "2026-10-04". */
export function localDate(d = new Date()): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/*
 * ---------------------------------------------------------------------------
 * The panel's hooks (it isn't always mounted)
 * ---------------------------------------------------------------------------
 */

let reasoningHandler: ((learned: ReasoningLearned) => void) | null = null;
const reasoningQueue: ReasoningLearned[] = [];

/** The panel keeps reasoning support with the model; queued until it's there. */
export function onReasoningLearned(handler: (learned: ReasoningLearned) => void): () => void {
  reasoningHandler = handler;
  for (const learned of reasoningQueue.splice(0)) handler(learned);
  return () => {
    if (reasoningHandler === handler) reasoningHandler = null;
  };
}

let routeHandler: ((profileId: string, route: ApiRoute) => void) | null = null;
const routeQueue: { profileId: string; route: ApiRoute }[] = [];

/** The panel saves the API route found for a model; queued until it's there. */
export function onApiRouteLearned(
  handler: (profileId: string, route: ApiRoute) => void
): () => void {
  routeHandler = handler;
  for (const { profileId, route } of routeQueue.splice(0)) handler(profileId, route);
  return () => {
    if (routeHandler === handler) routeHandler = null;
  };
}

/** A request found the API route a model works on (agent runs and one-off requests). */
export function noteApiRoute(profileId: string, route: ApiRoute) {
  if (routeHandler) routeHandler(profileId, route);
  else routeQueue.push({ profileId, route });
}

let unansweredHandler: ((dialogId: string, messageId: string) => void) | null = null;

/** A sent message got no answer: the panel brings its draft back, if it's open. */
export function onMessageUnanswered(handler: (dialogId: string, messageId: string) => void) {
  unansweredHandler = handler;
  return () => {
    if (unansweredHandler === handler) unansweredHandler = null;
  };
}

const showChatListeners = new Set<(target?: ShowTarget) => void>();

/**
 * "Show" on a notification of what a chat waits for: App brings up the
 * chats, and the panel opens the chat, or the chat list, and scrolls to it.
 */
export function onShowChat(listener: (target?: ShowTarget) => void): () => void {
  showChatListeners.add(listener);
  return () => showChatListeners.delete(listener);
}

function showChat(target?: ShowTarget) {
  for (const l of [...showChatListeners]) l(target);
}

/** An approval, as lib/remoteNotices.ts tells of it: a remote chat's only. */
function waitingApproval(request: ApprovalRequest): Waiting {
  const { dialogId, toolCallId, summary } = request;
  const chat = request.parentDialogId ?? dialogId;
  return {
    kind: 'approval',
    dialogId,
    toolCallId,
    chat,
    pending: () => !!pendingApproval(dialogId, toolCallId),
    notice: (host) => ({
      id: approvalNoticeId(dialogId, toolCallId),
      title: 'The agent needs your approval',
      message: host ? `${summary} (on ${host})` : summary,
      actions: [{ label: 'Show', primary: true, run: () => showChat({ dialogId: chat }) }],
    }),
  };
}

/** A question call, as lib/remoteNotices.ts tells of it, a local chat's too. */
function waitingQuestions(request: PendingQuestions): Waiting {
  const { questions, toolCallId, dialogId } = request;
  return {
    kind: 'question',
    dialogId,
    toolCallId,
    chat: dialogId,
    pending: () => !!pendingQuestions(dialogId, toolCallId),
    notice: () => ({
      id: questionNoticeId(dialogId, toolCallId),
      title:
        questions.length === 1
          ? 'The agent has a question'
          : `The agent has ${questions.length} questions`,
      message: questions[0]?.question,
      actions: [{ label: 'Show', primary: true, run: () => showChat({ dialogId }) }],
    }),
  };
}

/*
 * ---------------------------------------------------------------------------
 * Events
 * ---------------------------------------------------------------------------
 */

function handle(event: AgentEvent) {
  switch (event.type) {
    case 'runStarted':
      chatStore.setRunning(event.dialogId, true);
      break;
    case 'runEnded':
      chatStore.setRunning(event.dialogId, false);
      // The run is over: the live items are stale. Re-ask the backend
      // (whole database, carried checklists included) for the idle panel.
      // A finished checklist resolves to null and hides the panel.
      clearLiveChecklist(event.dialogId);
      void refreshChecklist(event.dialogId);
      break;
    case 'messagesAdded':
      // A subagent's chat isn't a chat in the list: only its open transcript follows it.
      if (isSubagentChat(event.dialogId)) receiveSubagentMessages(event.dialogId, event.messages);
      else chatStore.receiveMessages(event.dialogId, event.messages);
      break;
    case 'messagePatched':
      if (isSubagentChat(event.dialogId))
        patchSubagentMessage(event.dialogId, event.messageId, event.patch);
      else chatStore.applyPatch(event.dialogId, event.messageId, event.patch);
      break;
    case 'subagentStarted':
      noteSubagentStarted(event);
      break;
    case 'subagentProgress':
      noteSubagentProgress(event);
      break;
    case 'subagentEnded':
      noteSubagentEnded(event);
      break;
    case 'approvalRequested': {
      const { dialogId, toolCallId } = event.request;
      // Told again (a host that came back): it's been told of.
      const known = !!pendingApproval(dialogId, toolCallId);
      registerApproval(event.request);
      if (!known) tellWaiting(waitingApproval(event.request));
      break;
    }
    case 'approvalSettled':
      clearApproval(event.dialogId, event.toolCallId);
      dismiss(approvalNoticeId(event.dialogId, event.toolCallId));
      waitingSettled();
      break;
    case 'questionsAsked': {
      const { dialogId, toolCallId } = event.request;
      const known = !!pendingQuestions(dialogId, toolCallId);
      registerQuestions(event.request);
      // In the chat; and in the corner, in case the user is working elsewhere.
      if (!known) tellWaiting(waitingQuestions(event.request));
      break;
    }
    case 'questionsSettled':
      clearQuestions(event.dialogId, event.toolCallId);
      dismiss(questionNoticeId(event.dialogId, event.toolCallId));
      waitingSettled();
      break;
    case 'toolNote':
      setState(
        'toolNotes',
        produce((notes) => {
          const chat = (notes[event.dialogId] ??= {});
          if (event.note) chat[event.toolCallId] = event.note;
          else delete chat[event.toolCallId];
          if (Object.keys(chat).length === 0) delete notes[event.dialogId];
        })
      );
      break;
    case 'checklist':
      // Live source while the run goes: the panel reads this, not the
      // messages loaded here (which may be a window around a search hit).
      noteChecklist(event.dialogId, event.items);
      break;
    case 'fileWritten':
      if (event.deleted) {
        clearAgentChanges(event.absPath);
        notifyFileDeleted(event.absPath);
        break;
      }
      // An edit the user didn't review shows in the editor until they save.
      if (!event.reviewed) recordAgentEdit(event.absPath, event.before, event.content);
      notifyFileWritten(event.absPath, event.content);
      break;
    case 'reasoningLearned': {
      const { type: _type, ...learned } = event;
      if (reasoningHandler) reasoningHandler(learned);
      else reasoningQueue.push(learned);
      break;
    }
    case 'apiRouteLearned':
      noteApiRoute(event.profileId, event.route);
      break;
    case 'userMessageSettled':
      if (event.ok) clearPendingDraft(event.dialogId, event.messageId);
      // Unanswered: the draft stays stored until it's put back in the composer.
      else unansweredHandler?.(event.dialogId, event.messageId);
      break;
    case 'contextUpdated':
      noteClmStatus(event.dialogId, event.status);
      break;
    case 'notice':
      notify({ kind: event.kind, title: event.title, message: event.message, timeout: 12_000 });
      break;
  }
}

interface AgentStatus {
  runs: { dialogId: string; runId: string }[];
  /** Subagents' runs: their chats and their leads'. */
  subagents?: RunningSubagent[];
  approvals: ApprovalRequest[];
  questions: PendingQuestions[];
}

let ready: Promise<void> | null = null;

/**
 * Starts following the backend's runs (once). Resolves when what's already
 * going on is known: a reloaded page picks up running runs and what they
 * wait on.
 */
export function initAgentRuns(): Promise<void> {
  if (ready) return ready;
  ready = (async () => {
    await listen<AgentEvent>('agent-event', (e) => handle(e.payload));
    try {
      const status = await invoke<AgentStatus>('agent_status');
      for (const run of status.runs) chatStore.setRunning(run.dialogId, true);
      for (const sub of status.subagents ?? []) seedRunningSubagent(sub);
      // What waited already: in the chats, without a compare tab for each.
      for (const request of status.approvals) registerApproval(request, false);
      for (const request of status.questions) registerQuestions(request);
    } catch (err) {
      console.error('Agent status unavailable:', err);
    }
  })();
  // What already waited is told of with what its host's notice sums up.
  initRemoteNotices({ seeded: ready, show: showChat });
  return ready;
}
