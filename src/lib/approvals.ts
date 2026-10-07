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
import { invoke } from '@tauri-apps/api/core';
import { createStore, produce } from 'solid-js/store';
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
}

/** As a run asks: an edit comes with the file's text before and after it. */
export interface ApprovalRequest extends Omit<PendingApproval, 'edit'> {
  edit?: { absPath: string; relPath: string; current: string; proposed: string };
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

const [state, setState] = createStore<{ pending: Record<string, PendingApproval> }>({
  pending: {},
});

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
  if (state.pending[request.toolCallId]) return;
  const { edit, ...rest } = request;
  const review = edit
    ? buildEditReview(edit.absPath, edit.relPath, edit.current, edit.proposed)
    : null;
  setState('pending', request.toolCallId, { ...rest, ...(review ? { edit: review } : {}) });
  if (review) revealEdit(request.toolCallId);
}

/** Answered, or no longer waited for (the run stopped). */
export function clearApproval(toolCallId: string) {
  if (!state.pending[toolCallId]) return;
  setState(
    'pending',
    produce((pending) => {
      delete pending[toolCallId];
    })
  );
}

/*
 * ---------------------------------------------------------------------------
 * Reading (reactive)
 * ---------------------------------------------------------------------------
 */

export function pendingApproval(toolCallId: string): PendingApproval | undefined {
  return state.pending[toolCallId];
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

function settle(toolCallId: string, decision: ManualDecision) {
  clearApproval(toolCallId);
  void invoke('agent_resolve_approval', { toolCallId, decision }).catch((err) =>
    console.error('Approval not delivered:', err)
  );
}

export const allow = (toolCallId: string) => settle(toolCallId, { kind: 'allow' });
export const deny = (toolCallId: string) => settle(toolCallId, { kind: 'deny' });

/** Resolves an edit review once no change is left pending. */
function finishIfDecided(toolCallId: string) {
  const edit = state.pending[toolCallId]?.edit;
  if (!edit) return;
  const { pending, accepted, total } = hunkCounts(edit);
  if (pending > 0) return;
  if (accepted === total) return allow(toolCallId);
  if (accepted === 0) return deny(toolCallId);

  const hunks = edit.hunks.map((h) => ({ ...h, added: [...h.added], deleted: [...h.deleted] }));
  const merged = applyHunks(
    [...edit.baseLines],
    hunks,
    (h) => (h as ReviewHunk).state === 'accepted'
  );
  settle(toolCallId, {
    kind: 'partial',
    content: merged.join(edit.eol),
    total: hunks.length,
    rejected: hunks
      .filter((h) => h.state === 'rejected')
      .map(({ origStart, deleted, added }) => ({ origStart, deleted, added })),
  });
}

export function setHunkState(toolCallId: string, index: number, next: HunkState) {
  if (!state.pending[toolCallId]?.edit) return;
  setState('pending', toolCallId, 'edit', 'hunks', index, 'state', next);
  finishIfDecided(toolCallId);
}

/** Accepts (or rejects) every change still pending; decided ones stay. */
export function decideRemaining(toolCallId: string, next: 'accepted' | 'rejected') {
  const edit = state.pending[toolCallId]?.edit;
  if (!edit) return;
  setState('pending', toolCallId, 'edit', 'hunks', (h) => h.state === 'pending', 'state', next);
  finishIfDecided(toolCallId);
}

/*
 * ---------------------------------------------------------------------------
 * Editor bridge
 * ---------------------------------------------------------------------------
 */

/** Opens the file of an edit review and scrolls to its next pending change. */
export function revealEdit(toolCallId: string) {
  const edit = state.pending[toolCallId]?.edit;
  if (!edit) return;
  setState('pending', toolCallId, 'edit', 'revealNonce', (n) => n + 1);
  for (const listener of revealListeners) listener(edit.absPath);
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
