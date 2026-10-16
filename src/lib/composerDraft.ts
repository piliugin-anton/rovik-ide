// composerDraft.ts
//
// Unsent chat input, one row per chat (`composer_drafts` table).
//
// The composer is a contenteditable that unmounts whenever the sidebar tab
// switches away from chat, so its DOM would be lost. Every change schedules
// a debounced `chat_save_composer_draft` invoke; the active chat's draft is
// loaded on mount / dialog switch / restart.
//
// What is stored is the JSON-encoded `DraftSegment[]` (composerSegments.ts):
// text plus its chips (file references, pasted text, attachments). Saving
// plain text would flatten chips into their labels and they could never come
// back as chips.
// Rows written by the previous plain-text version are still readable: they
// come back as a single text segment.

import { chatApi } from './chatApi';
import type { DraftSegment } from './composerSegments';

/** Long enough to stay out of the way while typing, short enough to beat a tab switch. */
export const COMPOSER_DRAFT_DEBOUNCE_MS = 600;

const timers = new Map<string, number>();
const pending = new Map<string, DraftSegment[]>();

function clearTimer(dialogId: string) {
  const t = timers.get(dialogId);
  if (t !== undefined) {
    clearTimeout(t);
    timers.delete(dialogId);
  }
}

/** True when there is nothing worth storing (no chips, text blank). */
export function isEmptySegments(segments: DraftSegment[]): boolean {
  return !segments.some((s) => (s.type === 'text' ? s.text.trim().length > 0 : true));
}

function encode(segments: DraftSegment[]): string {
  return isEmptySegments(segments) ? '' : JSON.stringify(segments);
}

function send(dialogId: string, segments: DraftSegment[]): void {
  chatApi.saveComposerDraft(dialogId, encode(segments)).catch((err: unknown) => {
    console.error('Failed to save chat input draft:', err);
    // Keep it for the next flush attempt.
    pending.set(dialogId, segments);
  });
}

/**
 * Schedules a debounced write of the dialog's unsent input. Successive
 * changes in the same dialog collapse into one invoke.
 */
export function scheduleComposerDraft(dialogId: string, segments: DraftSegment[]): void {
  pending.set(dialogId, segments);
  clearTimer(dialogId);
  timers.set(
    dialogId,
    window.setTimeout(() => {
      timers.delete(dialogId);
      const latest = pending.get(dialogId);
      pending.delete(dialogId);
      if (latest === undefined) return;
      send(dialogId, latest);
    }, COMPOSER_DRAFT_DEBOUNCE_MS)
  );
}

/** Drops a scheduled write without sending it (e.g. the chat was deleted). */
export function cancelComposerDraft(dialogId: string): void {
  clearTimer(dialogId);
  pending.delete(dialogId);
}

/**
 * Writes the pending draft now, if any. Called when the panel unmounts
 * (tab switch), the dialog changes, or a message is sent. Awaits the write
 * so unmount can flush before the DOM is gone.
 */
export async function flushComposerDraft(dialogId: string): Promise<void> {
  clearTimer(dialogId);
  const latest = pending.get(dialogId);
  if (latest === undefined) return;
  pending.delete(dialogId);
  await chatApi.saveComposerDraft(dialogId, encode(latest));
}

/** Clears the saved draft, debounced write included (message sent). */
export function clearComposerDraft(dialogId: string): void {
  cancelComposerDraft(dialogId);
  chatApi.saveComposerDraft(dialogId, '').catch((err: unknown) => {
    console.error('Failed to clear chat input draft:', err);
  });
}

/**
 * The chat's saved unsent input, if any (null when none). Plain-text rows
 * from before chips were persisted come back as a single text segment.
 */
export function loadComposerDraft(dialogId: string): Promise<DraftSegment[] | null> {
  return chatApi
    .getComposerDraft(dialogId)
    .then((raw) => {
      if (!raw) return null;
      try {
        const parsed: unknown = JSON.parse(raw);
        if (Array.isArray(parsed)) return parsed as DraftSegment[];
      } catch {
        // Not JSON: a legacy plain-text row.
      }
      return raw.trim().length > 0 ? [{ type: 'text' as const, text: raw }] : null;
    })
    .catch((err: unknown) => {
      console.error('Failed to load chat input draft:', err);
      return null;
    });
}
