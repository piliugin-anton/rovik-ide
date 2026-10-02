// approvals.ts
//
// Manual approvals waiting for the user. The agent loop (ChatPanel) asks
// with `requestApproval` and waits; the chat (ToolCallBlock) and, for file
// edits, the editor (Editor + editorReview) show the request and answer it.
// Module-level, like ConfirmDialog, so no props need threading through.
//
// A file edit (write_file) is reviewed as hunks: each can be accepted or
// rejected on its own. Once none is left pending, the approval resolves:
// everything accepted → allow, everything rejected → deny, otherwise
// a partial result with the merged content.
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
  /** run_command: the full command line. */
  command?: string;
  /** Why this is asked manually, e.g. the AI reviewer couldn't decide. */
  note?: string;
  /** write_file on an existing file: the reviewable diff. */
  edit?: EditReview;
}

export type ManualDecision =
  | { kind: 'allow' }
  | { kind: 'deny' }
  | {
      kind: 'partial';
      /** The original with only the accepted changes applied. */
      content: string;
      accepted: ReviewHunk[];
      rejected: ReviewHunk[];
    };

const [state, setState] = createStore<{ pending: Record<string, PendingApproval> }>({
  pending: {},
});

const resolvers = new Map<string, (decision: ManualDecision) => void>();
const revealListeners = new Set<(absPath: string) => void>();
const writtenListeners = new Set<(absPath: string, content: string) => void>();

/*
 * ---------------------------------------------------------------------------
 * Asking
 * ---------------------------------------------------------------------------
 */

/** Builds the review of a write_file call; null when there's nothing to diff. */
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

/** Waits for the user's decision on one tool call. File edits open in the editor. */
export function requestApproval(request: PendingApproval): Promise<ManualDecision> {
  return new Promise((resolve) => {
    resolvers.set(request.toolCallId, resolve);
    setState('pending', request.toolCallId, request);
    if (request.edit) revealEdit(request.toolCallId);
  });
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
  const resolve = resolvers.get(toolCallId);
  resolvers.delete(toolCallId);
  setState(
    'pending',
    produce((pending) => {
      delete pending[toolCallId];
    })
  );
  resolve?.(decision);
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
    accepted: hunks.filter((h) => h.state === 'accepted'),
    rejected: hunks.filter((h) => h.state === 'rejected'),
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

/** Every waiting approval answered "deny" (Stop pressed). */
export function denyAll(dialogId?: string) {
  for (const p of Object.values(state.pending)) {
    if (!dialogId || p.dialogId === dialogId) deny(p.toolCallId);
  }
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
