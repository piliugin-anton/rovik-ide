// approvals.ts
//
// Manual approvals waiting for the user. A run in the backend asks
// (src-tauri/src/agent_run/execute.rs) and waits; the request arrives here
// (lib/agentRuns.ts), the chat (ToolCallBlock) and, for file edits, the
// editor (Editor + editorReview) show it, and the answer goes back to the
// run. Module-level, like ConfirmDialog, so no props need threading through.
//
// A file edit (write) is reviewed as hunks: each can be accepted or
// rejected on its own. Once none is left pending, the approval resolves:
// everything accepted → allow, everything rejected → deny, otherwise
// a partial result with the merged content.
//
// A remote chat's edit is to the server's file, not this computer's (K09):
// it shows in a read-only compare tab built from the file before and after,
// never on the local file's tab, and is allowed or denied whole.
import { invoke } from '@tauri-apps/api/core';
import { createStore, produce } from 'solid-js/store';
import { requestCompare } from './editorBridge';
import { applyHunks, diffLines, splitLines, type DiffHunk } from './lineDiff';

export type HunkState = 'pending' | 'accepted' | 'rejected';

export interface ReviewHunk extends DiffHunk {
  state: HunkState;
}

export interface EditReview {
  /** Absolute path, as the editor tabs know it. */
  absPath: string;
  /** As the model wrote it, for display. */
  relPath: string;
  /** File content the hunks were computed against (\r removed). */
  baseLines: string[];
  eol: '\n' | '\r\n';
  proposed: string;
  hunks: ReviewHunk[];
  /** Bumped to ask the editor to open the file and scroll to the next change. */
  revealNonce: number;
}

export interface PendingApproval {
  toolCallId: string;
  dialogId: string;
  toolName: string;
  /** One line, e.g. "Run `npm test`". */
  summary: string;
  /** bash: the full command line. */
  command?: string;
  /** Why this is asked manually, e.g. the AI reviewer couldn't decide. */
  note?: string;
  /** write on an existing file: the reviewable diff. */
  edit?: EditReview;
  /** A remote chat's write: the server's file before and after it. */
  remoteEdit?: RemoteEdit;
  /** A subagent's request: its lead's chat and the `task` call there. */
  parentDialogId?: string;
  parentCallId?: string;
}

/** A remote chat's edit: its file is on the server. */
export interface RemoteEdit {
  relPath: string;
  current: string;
  proposed: string;
}

/** As a run asks: an edit comes with the file's text before and after it. */
export interface ApprovalRequest extends Omit<PendingApproval, 'edit' | 'remoteEdit'> {
  edit?: { absPath: string; relPath: string; current: string; proposed: string };
  /** A remote chat's edit (K09): `edit` is the server's file. */
  remote?: boolean;
}

/** The answer the run gets (agent_run::ApprovalDecision). */
export type ManualDecision =
  | { kind: 'allow' }
  | { kind: 'deny' }
  | {
      kind: 'partial';
      /** The original with only the accepted changes applied. */
      content: string;
      total: number;
      rejected: DiffHunk[];
    };

// Keyed by chat and call: providers number calls per reply (Gemini:
// "function-call-1"), so two chats can wait on the same call id.
const [state, setState] = createStore<{ pending: Record<string, PendingApproval> }>({
  pending: {},
});

const keyOf = (dialogId: string, toolCallId: string) => `${dialogId}\n${toolCallId}`;

const revealListeners = new Set<(absPath: string) => void>();
const writtenListeners = new Set<(absPath: string, content: string) => void>();

/*
 * ---------------------------------------------------------------------------
 * Asking
 * ---------------------------------------------------------------------------
 */

/** Builds the review of a write call; null when there's nothing to diff. */
export function buildEditReview(
  absPath: string,
  relPath: string,
  current: string,
  proposed: string
): EditReview | null {
  const base = splitLines(current);
  const next = splitLines(proposed);
  const hunks = diffLines(base.lines, next.lines);
  if (hunks.length === 0) return null;
  return {
    absPath,
    relPath,
    baseLines: base.lines,
    eol: base.eol,
    proposed,
    hunks: hunks.map((h) => ({ ...h, state: 'pending' })),
    revealNonce: 0,
  };
}

/** A run waits for the user's decision on a tool call. File edits open in the editor. */
export function registerApproval(request: ApprovalRequest) {
  const key = keyOf(request.dialogId, request.toolCallId);
  if (state.pending[key]) return;
  const { edit, remote, ...rest } = request;
  if (edit && remote) {
    const { relPath, current, proposed } = edit;
    setState('pending', key, { ...rest, remoteEdit: { relPath, current, proposed } });
    showRemoteEdit(request.dialogId, request.toolCallId);
    return;
  }
  const review = edit
    ? buildEditReview(edit.absPath, edit.relPath, edit.current, edit.proposed)
    : null;
  setState('pending', key, { ...rest, ...(review ? { edit: review } : {}) });
  if (review) revealEdit(request.dialogId, request.toolCallId);
}

/** Answered, or no longer waited for (the run stopped). */
export function clearApproval(dialogId: string, toolCallId: string) {
  const key = keyOf(dialogId, toolCallId);
  if (!state.pending[key]) return;
  setState(
    'pending',
    produce((pending) => {
      delete pending[key];
    })
  );
}

/*
 * ---------------------------------------------------------------------------
 * Reading (reactive)
 * ---------------------------------------------------------------------------
 */

export function pendingApproval(dialogId: string, toolCallId: string): PendingApproval | undefined {
  return state.pending[keyOf(dialogId, toolCallId)];
}

/** The chats an approval waits in: a subagent's counts for its lead's chat. */
export function chatsWaitingOnApprovals(): Record<string, true> {
  const chats: Record<string, true> = {};
  for (const p of Object.values(state.pending)) chats[p.parentDialogId ?? p.dialogId] = true;
  return chats;
}

/** Every approval waiting now, in any chat. */
export function approvalsWaiting(): PendingApproval[] {
  return Object.values(state.pending);
}

/** Every approval a chat waits on (a subagent's, shown on its block). */
export function pendingApprovalsOf(dialogId: string): PendingApproval[] {
  return Object.values(state.pending).filter((p) => p.dialogId === dialogId);
}

/**
 * The approvals a lead's `task` call waits on: its subagent's, found by the
 * subagent's chat when it's known and by the call the requests name
 * (P1.5: after a reload, before anything else about the subagent is).
 */
export function pendingApprovalsOfTask(
  dialogId: string,
  toolCallId: string,
  childId: string | null
): PendingApproval[] {
  // A parallel group's workers run under "<call>#<label>" (group.rs), so
  // their requests are the group's call's too (round 3).
  const workers = `${toolCallId}#`;
  return Object.values(state.pending).filter(
    (p) =>
      (childId !== null && p.dialogId === childId) ||
      (p.parentDialogId === dialogId &&
        (p.parentCallId === toolCallId || (p.parentCallId?.startsWith(workers) ?? false)))
  );
}

/** The pending edit review of a file, if any. */
export function pendingEditFor(absPath: string): PendingApproval | undefined {
  const key = normalizePath(absPath);
  return Object.values(state.pending).find((p) => p.edit && normalizePath(p.edit.absPath) === key);
}

export function hunkCounts(edit: EditReview) {
  let pending = 0;
  let accepted = 0;
  let rejected = 0;
  for (const h of edit.hunks) {
    if (h.state === 'pending') pending++;
    else if (h.state === 'accepted') accepted++;
    else rejected++;
  }
  return { pending, accepted, rejected, total: edit.hunks.length };
}

const normalizePath = (p: string) => p.replace(/\\/g, '/').replace(/\/+/g, '/');

/*
 * ---------------------------------------------------------------------------
 * Answering
 * ---------------------------------------------------------------------------
 */

function settle(dialogId: string, toolCallId: string, decision: ManualDecision) {
  clearApproval(dialogId, toolCallId);
  void invoke('agent_resolve_approval', { dialogId, toolCallId, decision }).catch((err) =>
    console.error('Approval not delivered:', err)
  );
}

export const allow = (dialogId: string, toolCallId: string) =>
  settle(dialogId, toolCallId, { kind: 'allow' });
export const deny = (dialogId: string, toolCallId: string) =>
  settle(dialogId, toolCallId, { kind: 'deny' });

/** Resolves an edit review once no change is left pending. */
function finishIfDecided(dialogId: string, toolCallId: string) {
  const edit = state.pending[keyOf(dialogId, toolCallId)]?.edit;
  if (!edit) return;
  const { pending, accepted, total } = hunkCounts(edit);
  if (pending > 0) return;
  if (accepted === total) return allow(dialogId, toolCallId);
  if (accepted === 0) return deny(dialogId, toolCallId);

  const hunks = edit.hunks.map((h) => ({ ...h, added: [...h.added], deleted: [...h.deleted] }));
  const merged = applyHunks(
    [...edit.baseLines],
    hunks,
    (h) => (h as ReviewHunk).state === 'accepted'
  );
  settle(dialogId, toolCallId, {
    kind: 'partial',
    content: merged.join(edit.eol),
    total: hunks.length,
    rejected: hunks
      .filter((h) => h.state === 'rejected')
      .map(({ origStart, deleted, added }) => ({ origStart, deleted, added })),
  });
}

export function setHunkState(dialogId: string, toolCallId: string, index: number, next: HunkState) {
  const key = keyOf(dialogId, toolCallId);
  if (!state.pending[key]?.edit) return;
  setState('pending', key, 'edit', 'hunks', index, 'state', next);
  finishIfDecided(dialogId, toolCallId);
}

/** Accepts (or rejects) every change still pending; decided ones stay. */
export function decideRemaining(
  dialogId: string,
  toolCallId: string,
  next: 'accepted' | 'rejected'
) {
  const key = keyOf(dialogId, toolCallId);
  if (!state.pending[key]?.edit) return;
  setState('pending', key, 'edit', 'hunks', (h) => h.state === 'pending', 'state', next);
  finishIfDecided(dialogId, toolCallId);
}

/*
 * ---------------------------------------------------------------------------
 * Editor bridge
 * ---------------------------------------------------------------------------
 */

/** Opens the file of an edit review and scrolls to its next pending change. */
export function revealEdit(dialogId: string, toolCallId: string) {
  const key = keyOf(dialogId, toolCallId);
  const edit = state.pending[key]?.edit;
  if (!edit) return;
  setState('pending', key, 'edit', 'revealNonce', (n) => n + 1);
  for (const listener of revealListeners) listener(edit.absPath);
}

/** A remote chat's edit, in a read-only tab comparing the file before and after it. */
export function showRemoteEdit(dialogId: string, toolCallId: string) {
  const edit = state.pending[keyOf(dialogId, toolCallId)]?.remoteEdit;
  if (!edit) return;
  const name = edit.relPath.split(/[\\/]/).pop() || edit.relPath;
  requestCompare({
    id: `remote-edit:${dialogId}:${toolCallId}`,
    title: `${name} (proposed on the server)`,
    fileName: edit.relPath,
    before: edit.current,
    after: edit.proposed,
  });
}

/** App opens the file in a tab. */
export function onRevealEdit(listener: (absPath: string) => void): () => void {
  revealListeners.add(listener);
  return () => revealListeners.delete(listener);
}

/** The agent wrote a file: open tabs refresh their content. */
export function notifyFileWritten(absPath: string, content: string) {
  for (const listener of writtenListeners) listener(absPath, content);
}

export function onFileWritten(listener: (absPath: string, content: string) => void): () => void {
  writtenListeners.add(listener);
  return () => writtenListeners.delete(listener);
}

const deletedListeners = new Set<(absPath: string) => void>();

/** The agent removed a file (a rollback of one it created, P3.4). */
export function notifyFileDeleted(absPath: string) {
  for (const listener of deletedListeners) listener(absPath);
}

export function onFileDeleted(listener: (absPath: string) => void): () => void {
  deletedListeners.add(listener);
  return () => deletedListeners.delete(listener);
}
