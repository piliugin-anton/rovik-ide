// checklist.ts
//
// The chat's current checklist from the backend's point of view
// (`plan::current_checklist`): the single source of truth for the panel,
// instead of the UI recomputing it from the messages it has loaded (which
// may be a window around a search hit, or miss an older page).
//
// - While a run goes, the backend sends a `checklist` event after every
//   `todo` result and at run start when a checklist is carried: that is
//   what the panel reads, so it updates on the call, not on the scroll.
// - When no run goes (chat opened, run ended), the panel asks
//   `chat_checklist`, which reads the whole database and carries open
//   checklists across short follow-ups like "continue".
import { invoke } from '@tauri-apps/api/core';
import { createStore } from 'solid-js/store';
import type { TodoItem } from './todo';
import { isSettled } from './todo';
import { chatApi } from './chatApi';

export interface BackendChecklist {
  items: TodoItem[];
  carried: boolean;
  fromMessageId: string | null;
}

const [state, setState] = createStore<{
  /** Live items from the `checklist` event, per chat, while a run goes. */
  live: Record<string, TodoItem[]>;
  /** Last `chat_checklist` answer per chat, for when no run goes. */
  settled: Record<string, BackendChecklist | null>;
  pending: Record<string, boolean>;
}>({ live: {}, settled: {}, pending: {} });

/** Live items from the backend's `checklist` event (while a run goes). */
export const liveChecklist = (dialogId: string | null | undefined): TodoItem[] | null =>
  dialogId ? (state.live[dialogId] ?? null) : null;

/** Last `chat_checklist` answer (when no run goes). */
export const settledChecklist = (
  dialogId: string | null | undefined
): BackendChecklist | null | undefined => (dialogId ? state.settled[dialogId] : undefined);

/** Whether a `chat_checklist` round-trip is in flight. */
export const checklistPending = (dialogId: string | null | undefined): boolean =>
  !!dialogId && !!state.pending[dialogId];

/** A `checklist` event arrived: the panel's live source while the run goes. */
export function noteChecklist(dialogId: string, items: TodoItem[]): void {
  setState('live', dialogId, items);
}

/** The run ended: drop the live items; the next `refreshChecklist` decides. */
export function clearLiveChecklist(dialogId: string): void {
  setState('live', (live) => {
    if (!(dialogId in live)) return live;
    const next = { ...live };
    delete next[dialogId];
    return next;
  });
}

/**
 * Ask the backend for the chat's current checklist (whole database, carried
 * checklists included). Used when a chat is opened and when a run ends.
 */
export async function refreshChecklist(dialogId: string): Promise<BackendChecklist | null> {
  setState('pending', dialogId, true);
  try {
    const answer = await invoke<BackendChecklist | null>('chat_checklist', { dialogId });
    const items = (answer?.items ?? []) as TodoItem[];
    const checklist = answer ? { ...answer, items } : null;
    setState('settled', dialogId, checklist);
    // A settled answer supersedes the live one (e.g. the run ended and the
    // checklist is done or dismissed): no stale panel.
    if (!checklist || checklist.items.every(isSettled)) clearLiveChecklist(dialogId);
    return checklist;
  } catch (err) {
    console.error('Checklist unavailable:', err);
    return dialogId ? (state.settled[dialogId] ?? null) : null;
  } finally {
    setState('pending', (pending) => {
      if (!(dialogId in pending)) return pending;
      const next = { ...pending };
      delete next[dialogId];
      return next;
    });
  }
}

/**
 * Dismiss the open checklist (the panel's ✕): marks the latest real user
 * message `checklistClosed` so the backend no longer carries it, then
 * refreshes. Returns false when there was nothing to dismiss.
 */
export async function dismissChecklist(
  dialogId: string,
  messageId: string | null,
  meta: Record<string, unknown> | undefined
): Promise<boolean> {
  if (!messageId) return false;
  try {
    await chatApi.updateMessage(messageId, {
      meta: { ...(meta ?? {}), checklistClosed: true } as never,
    });
  } catch (err) {
    console.error('Checklist not dismissed:', err);
    return false;
  }
  clearLiveChecklist(dialogId);
  setState('settled', dialogId, null);
  void refreshChecklist(dialogId);
  return true;
}
