// pendingDraft.ts
//
// A message is saved here the moment it's sent and removed once the model
// has answered it. If the request fails, or the app closes before an answer
// arrives, the draft is still here and gets put back into the composer.
//
// localStorage is synchronous, so the draft is on disk before the request
// starts. Tauri persists it in the app's data directory.

import type { FileSuggestion } from '../types';

export type DraftSegment =
  | { type: 'text'; text: string }
  | { type: 'file'; file: FileSuggestion }
  /** A long paste, kept as a chip (ChatPanel). */
  | { type: 'pasted'; text: string };

export interface PendingDraft {
  dialogId: string;
  /** The user message this draft was sent as. */
  messageId: string;
  segments: DraftSegment[];
  createdAt: string;
}

const STORAGE_KEY = 'tauri-ide:pending-drafts';

function readAll(): Record<string, PendingDraft> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

function writeAll(drafts: Record<string, PendingDraft>) {
  try {
    if (Object.keys(drafts).length === 0) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, JSON.stringify(drafts));
  } catch (err) {
    console.error('Failed to persist pending draft:', err);
  }
}

/** Keyed by dialog: only one message per dialog can be in flight. */
export function savePendingDraft(draft: PendingDraft) {
  writeAll({ ...readAll(), [draft.dialogId]: draft });
}

/** Removes the draft, but only if it still belongs to `messageId`. */
export function clearPendingDraft(dialogId: string, messageId: string) {
  const all = readAll();
  if (all[dialogId]?.messageId !== messageId) return;
  delete all[dialogId];
  writeAll(all);
}

/** Returns and removes the draft for a dialog, if any. */
export function takePendingDraft(dialogId: string): PendingDraft | undefined {
  const all = readAll();
  const draft = all[dialogId];
  if (!draft) return undefined;
  delete all[dialogId];
  writeAll(all);
  return draft;
}

export function listPendingDrafts(): PendingDraft[] {
  return Object.values(readAll());
}

/** Drops drafts whose dialog no longer exists (e.g. deleted while offline). */
export function prunePendingDrafts(existingDialogIds: Set<string>) {
  const all = readAll();
  let changed = false;
  for (const id of Object.keys(all)) {
    if (!existingDialogIds.has(id)) {
      delete all[id];
      changed = true;
    }
  }
  if (changed) writeAll(all);
}
