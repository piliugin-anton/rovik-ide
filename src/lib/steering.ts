// steering.ts
//
// Messages to the agent while it works (docs/design/steering-plan.md): the
// decisions the chat panel takes about them, apart from the panel so they
// can be tested. What's sent and stored is lib/agentRuns.ts's and
// lib/chatStore.ts's; the drafts that bring a message back after a crash
// are lib/steerDrafts.ts's.
import type { ChatMessage, ChatPlace } from '../types';
import { draftWaits } from './remoteGuards';
import { isRequestMessage, steersOfRequest } from './requests';
import type { SteerDraft } from './steerDrafts';

/** What Enter (or a Send button) does with what's typed. */
export type SendAction =
  /** A question waits: what's typed is its answer. */
  | 'answer'
  /** Nothing runs: a new message, the request of a new run. */
  | 'send'
  /** A run that takes messages goes: it reads this one at its next step. */
  | 'steer'
  /** A run that takes none goes (Plan mode, an Autopilot task's, one still starting): the text stays. */
  | 'hold';

export function sendAction(chat: {
  /** An `ask_user` question waits for an answer. */
  questionWaits: boolean;
  /** A run goes, is being started, or a task's driver is at work. */
  running: boolean;
  /** The run said it takes messages (`runStarted`): an Agent-mode run's. */
  steerable: boolean;
}): SendAction {
  if (chat.questionWaits) return 'answer';
  if (!chat.running) return 'send';
  return chat.steerable ? 'steer' : 'hold';
}

/** What the run is busy with, as the chat shows it, for a queued message's label. */
export interface RunWaits {
  /** An approval card waits for the user. */
  approval: boolean;
  /** Subagents it started are running. */
  subagents: boolean;
  /** Its change review or its checks are running. */
  checks: boolean;
}

/** The label of a message the run hasn't read yet. */
export function queuedLabel(waits: RunWaits): string {
  if (waits.approval) return 'Queued: the agent reads it after you answer the approval';
  if (waits.subagents) return 'Queued: the agent reads it when its subagents report back';
  if (waits.checks) return 'Queued: the agent reads it when the checks finish';
  return 'Queued: the agent reads it at its next step';
}

/** The label of one the run could no longer take (`ended`). */
export const LATE_LABEL = 'Sent as a new message once the agent has finished';

/** Whether the current request's change review or checks are running. */
export function checksRunning(messages: ChatMessage[]): boolean {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (isRequestMessage(m)) return false;
    if (m.meta?.verification?.status === 'running') return true;
    if (m.meta?.review?.kind === 'change' && m.meta.review.status === 'running') return true;
  }
  return false;
}

/** From this many steering messages in one request, the hint suggests a fresh start. */
export const HINT_FROM = 3;

export const HINT_TEXT =
  'Several corrections in one run: a fresh start with one message that says it all often works better.';

/**
 * Whether the hint shows: the current request's steering messages, those
 * stored and those still queued (not the ones to be sent as a new message),
 * are HINT_FROM or more.
 */
export function showsSteerHint(messages: ChatMessage[], queued: number): boolean {
  return steersOfRequest(messages).length + queued >= HINT_FROM;
}

/** Why the run's messages are back in the input, by how the run ended (`steerReturned`). */
export function returnedNotice(outcome: string): { title: string; message: string } {
  const title = 'Your message is back in the input';
  switch (outcome) {
    case 'stopped':
    case 'verify-stopped':
      return { title, message: 'The agent stopped before it read your message.' };
    case 'error':
      return { title, message: 'The agent stopped on an error before it read your message.' };
    case 'verify-needs-approval':
      return {
        title,
        message: 'The run ended with its checks waiting for your go, before it read your message.',
      };
    default:
      return { title, message: 'The agent ended before it read your message.' };
  }
}

export interface SteerJudging {
  /** A chat whose run still goes: it may yet read its messages. */
  isRunning: (dialogId: string) => boolean;
  /** Its bubble shows: a run still holds it (a host's run the app came back to). */
  isQueued: (dialogId: string, messageId: string) => boolean;
  /** Where the chat is: absent for one on this computer. */
  placeOf: (dialogId: string) => Promise<ChatPlace | undefined>;
  /** The hosts whose mirrors caught up since they connected. */
  caughtUp: Record<string, true>;
  /** The messages among these whose rows are stored: the run read them. */
  stored: (messageIds: string[]) => Promise<string[]>;
  /** The messages among these with an answer after them. */
  answered: (messageIds: string[]) => Promise<string[]>;
  /** Its message was read and answered: the draft goes. */
  clear: (messageId: string) => void;
  /** Read but not answered: marked failed (never sent to the model again). */
  markFailed: (dialogId: string, messageId: string) => void;
  /** Its text goes back into the chat's input. */
  giveBack: (draft: SteerDraft) => void;
}

/**
 * Judges the steering drafts no run holds any more (after a restart, or a
 * run that ended without word of them), as the pending drafts are
 * (`recoverDrafts`): one never read comes back into the input; one read
 * and answered goes; one read with no answer after it (the run ended
 * between the two) is marked failed and comes back. Returns those left for
 * later: a running chat's, a queued one's, and a host chat's until the host
 * has caught up (K23).
 */
export async function judgeSteerDrafts(
  drafts: SteerDraft[],
  deps: SteerJudging
): Promise<SteerDraft[]> {
  const now: SteerDraft[] = [];
  const later: SteerDraft[] = [];
  for (const draft of drafts) {
    if (draft.returned) continue;
    const waits =
      deps.isRunning(draft.dialogId) ||
      deps.isQueued(draft.dialogId, draft.messageId) ||
      draftWaits(await deps.placeOf(draft.dialogId), deps.caughtUp);
    (waits ? later : now).push(draft);
  }
  if (now.length === 0) return later;
  const ids = now.map((d) => d.messageId);
  const stored = new Set(await deps.stored(ids));
  const answered = new Set(await deps.answered(ids.filter((id) => stored.has(id))));
  for (const draft of now) {
    if (answered.has(draft.messageId)) {
      deps.clear(draft.messageId);
      continue;
    }
    if (stored.has(draft.messageId)) deps.markFailed(draft.dialogId, draft.messageId);
    deps.giveBack(draft);
  }
  return later;
}
