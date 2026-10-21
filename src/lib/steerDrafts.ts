// steerDrafts.ts
//
// A message sent to a run while it works (steering, docs/design/
// steering-plan.md 3.7) is kept here from the moment it's sent, as the
// pending drafts are (lib/pendingDraft.ts), so a crash puts it back into the
// input: chips, images and PDFs included. Unlike those, a chat can have
// several at once, so they're kept by message id.
//
// A draft goes when the run has read its message and gone on (the run's
// end), when it's taken back with ✕, or when its text is back in the input.
// One whose message the run ended without reading is marked `returned`:
// its chat's input takes it back when the chat is open.

import type { PendingDraft } from './pendingDraft';

export interface SteerDraft extends PendingDraft {
  /** The run read it: its row is stored (the run's end lets it go). */
  stored?: boolean;
  /** Its text goes back into the chat's input (the run ended without reading it). */
  returned?: boolean;
}

const STORAGE_KEY = 'rovik.steerDrafts';

function readAll(): Record<string, SteerDraft> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

function writeAll(drafts: Record<string, SteerDraft>) {
  try {
    if (Object.keys(drafts).length === 0) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, JSON.stringify(drafts));
  } catch (err) {
    console.error('Failed to persist a steering draft:', err);
  }
}

/** Oldest first. */
const byAge = (a: SteerDraft, b: SteerDraft) => a.createdAt.localeCompare(b.createdAt);

export function saveSteerDraft(draft: SteerDraft) {
  writeAll({ ...readAll(), [draft.messageId]: draft });
}

export function steerDraft(messageId: string): SteerDraft | undefined {
  return readAll()[messageId];
}

export function clearSteerDraft(messageId: string) {
  const all = readAll();
  if (!all[messageId]) return;
  delete all[messageId];
  writeAll(all);
}

/** Every draft, oldest first. */
export function listSteerDrafts(): SteerDraft[] {
  return Object.values(readAll()).sort(byAge);
}

/** The chat's drafts, oldest first. */
export function steerDraftsOf(dialogId: string): SteerDraft[] {
  return listSteerDrafts().filter((d) => d.dialogId === dialogId);
}

/** These messages were read: their rows are stored. */
export function markSteerDraftsStored(messageIds: string[]) {
  changeDrafts(messageIds, (d) => ({ ...d, stored: true }));
}

/** These messages go back into their chat's input (`takeReturnedSteerDrafts`). */
export function returnSteerDrafts(messageIds: string[]) {
  changeDrafts(messageIds, (d) => ({ ...d, returned: true }));
}

function changeDrafts(messageIds: string[], change: (d: SteerDraft) => SteerDraft) {
  const all = readAll();
  let changed = false;
  for (const id of messageIds) {
    if (!all[id]) continue;
    all[id] = change(all[id]);
    changed = true;
  }
  if (changed) writeAll(all);
}

/** The run ended: what it read went on with it, so their drafts go. */
export function clearStoredSteerDrafts(dialogId: string) {
  const all = readAll();
  let changed = false;
  for (const [id, d] of Object.entries(all)) {
    if (d.dialogId !== dialogId || !d.stored || d.returned) continue;
    delete all[id];
    changed = true;
  }
  if (changed) writeAll(all);
}

/** Returns and removes the chat's returned drafts, oldest first, for its input. */
export function takeReturnedSteerDrafts(dialogId: string): SteerDraft[] {
  const all = readAll();
  const taken = Object.values(all)
    .filter((d) => d.dialogId === dialogId && d.returned)
    .sort(byAge);
  if (taken.length === 0) return [];
  for (const d of taken) delete all[d.messageId];
  writeAll(all);
  return taken;
}

/** The chat is deleted: its drafts go with it. */
export function clearSteerDraftsOf(dialogId: string) {
  const all = readAll();
  let changed = false;
  for (const [id, d] of Object.entries(all)) {
    if (d.dialogId !== dialogId) continue;
    delete all[id];
    changed = true;
  }
  if (changed) writeAll(all);
}
