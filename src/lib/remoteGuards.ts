// remoteGuards.ts
//
// What the chat panel does differently for a remote chat (U04), apart
// from the panel so it can be tested:
// - Opening a chat whose last step has tool calls without results, or
//   whose Autopilot task was left unfinished, picks it up here, but not a
//   remote chat's: its daemon resumes it (K07).
// - A draft left from before a restart is judged against the database: a
//   remote chat's only once its host has connected and its mirror caught
//   up, as only the owner knows whether its message was answered (K23).
// - A sent message's input is kept until the message is answered, in
//   localStorage. That can't hold a few megabytes of text files, and its
//   error is swallowed, so a remote chat's input also stays saved in
//   `composer_drafts` until its start succeeded (K49).
import type { ChatMessage, ChatPlace, ChatToolCall } from '../types';
import { taskOver, unfinishedTask, type StageRow } from './autopilot';
import type { DraftSegment } from './composerSegments';
import type { PendingDraft } from './pendingDraft';

/** Tool calls of the chat's last agent step that have no result yet. */
export function unansweredToolCalls(list: ChatMessage[]): ChatToolCall[] {
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

/**
 * Whether opening the chat picks its run up here: its last step has calls
 * without results, or its Autopilot task is unfinished (the backend knows
 * where to go on), and it's this computer's chat. `task`: its task as
 * stored (`taskRecords`), when the page of messages held shows none of its
 * records: a long run since its stage's started (R03).
 */
export function resumesHere(
  messages: ChatMessage[],
  place: ChatPlace | undefined,
  task?: StageRow[] | null
): boolean {
  if (place) return false;
  const unfinished = unfinishedTask(messages) || (!!task?.length && !taskOver(task));
  return unansweredToolCalls(messages).length > 0 || unfinished;
}

/** Whether a chat's draft waits to be judged: its host hasn't caught up yet. */
export function draftWaits(place: ChatPlace | undefined, caughtUp: Record<string, true>): boolean {
  if (!place) return false;
  // A moving chat can't be written to, whatever its host says.
  return place.kind !== 'host' || !caughtUp[place.host];
}

export interface DraftRecovery {
  /** A chat whose run still goes: its message may yet be answered. */
  isRunning: (dialogId: string) => boolean;
  /** Where the chat is: absent for one on this computer. */
  placeOf: (dialogId: string) => Promise<ChatPlace | undefined>;
  /** The hosts whose mirrors caught up since they connected. */
  caughtUp: Record<string, true>;
  /** The messages among these that were answered. */
  answered: (messageIds: string[]) => Promise<string[]>;
  /** Clears an answered message's draft. */
  clear: (dialogId: string, messageId: string) => void;
  /** Marks an unanswered message failed; its draft comes back into the input. */
  markFailed: (dialogId: string, messageId: string) => void;
}

/**
 * Judges the drafts left from before a restart: an answered message's goes,
 * an unanswered one is marked failed. Returns those left for later: a
 * running chat's, and a remote chat's until its host has caught up.
 */
export async function recoverDrafts(
  drafts: PendingDraft[],
  deps: DraftRecovery
): Promise<PendingDraft[]> {
  const now: PendingDraft[] = [];
  const later: PendingDraft[] = [];
  for (const draft of drafts) {
    const waits =
      deps.isRunning(draft.dialogId) ||
      draftWaits(await deps.placeOf(draft.dialogId), deps.caughtUp);
    (waits ? later : now).push(draft);
  }
  if (now.length === 0) return later;
  const answered = new Set(await deps.answered(now.map((d) => d.messageId)));
  for (const draft of now) {
    if (answered.has(draft.messageId)) deps.clear(draft.dialogId, draft.messageId);
    else deps.markFailed(draft.dialogId, draft.messageId);
  }
  return later;
}

export interface SentInput {
  dialogId: string;
  messageId: string;
  segments: DraftSegment[];
  /** A remote chat's: its files go up before its start. */
  remote: boolean;
}

export interface KeepInput {
  /** Into localStorage, until the message is answered; may fail silently. */
  savePending: (draft: PendingDraft) => void;
  clearPending: (dialogId: string, messageId: string) => void;
  /** The input's saved draft (`composer_drafts`), written now. */
  saveDraft: (dialogId: string, segments: DraftSegment[]) => Promise<void>;
  clearDraft: (dialogId: string) => void;
  /** The send started: the saved draft becomes the input as it is now. */
  started: (dialogId: string) => void;
}

/**
 * Sends a message, keeping its input until it's safe to drop: in
 * localStorage until it's answered, and for a remote chat in
 * `composer_drafts` too until its start succeeded. Resolves with the input
 * to put back when the send failed, or null when it started.
 */
export async function sendKeepingInput(
  input: SentInput,
  send: () => Promise<boolean>,
  keep: KeepInput
): Promise<DraftSegment[] | null> {
  const { dialogId, messageId, segments } = input;
  keep.savePending({ dialogId, messageId, segments, createdAt: new Date().toISOString() });
  if (input.remote) await keep.saveDraft(dialogId, segments);
  else keep.clearDraft(dialogId);
  if (await send()) {
    if (input.remote) keep.started(dialogId);
    return null;
  }
  keep.clearPending(dialogId, messageId);
  return segments;
}
