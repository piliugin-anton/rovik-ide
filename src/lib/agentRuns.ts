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
//     lib/questions.ts, where the chat and the editor answer them;
//   - files it writes refresh open tabs, and unreviewed edits are marked;
//   - what a reply showed about the model's reasoning goes to the panel,
//     which keeps it in the settings (queued until the panel is there);
//   - the CLM state each request went out with goes to lib/contextMode.ts.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createStore, produce } from 'solid-js/store';
import type { ChatMessage, ModelProfile, OpenAIChatOptions } from '../types';
import type { AgentPermissions } from './permissions';
import { chatStore, type MessageRow } from './chatStore';
import {
  clearApproval,
  notifyFileWritten,
  registerApproval,
  type ApprovalRequest,
} from './approvals';
import { clearQuestions, registerQuestions, type PendingQuestions } from './questions';
import { recordAgentEdit } from './agentChanges';
import { clearPendingDraft } from './pendingDraft';
import { dismiss, notify } from './notifications';
import { noteClmStatus, type ClmStatus } from './contextMode';

/** Settings runs follow (agent_run::AgentConfig). */
export interface AgentConfig {
  permissions: AgentPermissions;
  /** The AI reviewer, then its fallbacks: enabled models only. */
  approvalProfiles: ModelProfile[];
}

export type StartKind =
  | { kind: 'send'; message: { id: string; content: string; meta?: ChatMessage['meta'] } }
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
  mode: 'agent' | 'plan';
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

type MessagePatch = Partial<
  Pick<ChatMessage, 'content' | 'toolCalls' | 'meta' | 'error' | 'failed' | 'pending' | 'finalized'>
> & { status?: ChatMessage['status'] | null };

type AgentEvent =
  | { type: 'runStarted'; dialogId: string; runId: string }
  | { type: 'runEnded'; dialogId: string; runId: string; outcome: string }
  | { type: 'messagesAdded'; dialogId: string; messages: (MessageRow & { pending?: boolean })[] }
  | { type: 'messagePatched'; dialogId: string; messageId: string; patch: MessagePatch }
  | { type: 'approvalRequested'; request: ApprovalRequest }
  | { type: 'approvalSettled'; toolCallId: string }
  | { type: 'questionsAsked'; request: PendingQuestions }
  | { type: 'questionsSettled'; toolCallId: string }
  | { type: 'toolNote'; dialogId: string; toolCallId: string; note: string | null }
  | {
      type: 'fileWritten';
      absPath: string;
      before: string | null;
      content: string;
      reviewed: boolean;
    }
  | ({ type: 'reasoningLearned' } & ReasoningLearned)
  | { type: 'userMessageSettled'; dialogId: string; messageId: string; ok: boolean }
  | { type: 'contextUpdated'; dialogId: string; status: ClmStatus }
  | {
      type: 'notice';
      kind: 'info' | 'success' | 'warning' | 'error';
      title: string;
      message: string;
    };

const [state, setState] = createStore<{
  /** Live notes on tool calls, e.g. "Reviewing with …", by tool call id. */
  toolNotes: Record<string, string>;
}>({ toolNotes: {} });

export const toolNotes = () => state.toolNotes;

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

let unansweredHandler: ((dialogId: string, messageId: string) => void) | null = null;

/** A sent message got no answer: the panel brings its draft back, if it's open. */
export function onMessageUnanswered(handler: (dialogId: string, messageId: string) => void) {
  unansweredHandler = handler;
  return () => {
    if (unansweredHandler === handler) unansweredHandler = null;
  };
}

const showChatListeners = new Set<() => void>();

/** "Show" on a question's notification: App opens the chat, the panel scrolls to it. */
export function onShowChat(listener: () => void): () => void {
  showChatListeners.add(listener);
  return () => showChatListeners.delete(listener);
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
      break;
    case 'messagesAdded':
      chatStore.receiveMessages(event.dialogId, event.messages);
      break;
    case 'messagePatched':
      chatStore.applyPatch(event.dialogId, event.messageId, event.patch);
      break;
    case 'approvalRequested':
      registerApproval(event.request);
      break;
    case 'approvalSettled':
      clearApproval(event.toolCallId);
      break;
    case 'questionsAsked': {
      registerQuestions(event.request);
      // In the chat; and in the corner, in case the user is working elsewhere.
      const { questions, toolCallId } = event.request;
      notify({
        id: `question-${toolCallId}`,
        title:
          questions.length === 1
            ? 'The agent has a question'
            : `The agent has ${questions.length} questions`,
        message: questions[0]?.question,
        actions: [
          {
            label: 'Show',
            primary: true,
            run: () => {
              for (const l of [...showChatListeners]) l();
            },
          },
        ],
      });
      break;
    }
    case 'questionsSettled':
      clearQuestions(event.toolCallId);
      dismiss(`question-${event.toolCallId}`);
      break;
    case 'toolNote':
      setState(
        'toolNotes',
        produce((notes) => {
          if (event.note) notes[event.toolCallId] = event.note;
          else delete notes[event.toolCallId];
        })
      );
      break;
    case 'fileWritten':
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
  ready ??= (async () => {
    await listen<AgentEvent>('agent-event', (e) => handle(e.payload));
    try {
      const status = await invoke<AgentStatus>('agent_status');
      for (const run of status.runs) chatStore.setRunning(run.dialogId, true);
      for (const request of status.approvals) registerApproval(request);
      for (const request of status.questions) registerQuestions(request);
    } catch (err) {
      console.error('Agent status unavailable:', err);
    }
  })();
  return ready;
}
