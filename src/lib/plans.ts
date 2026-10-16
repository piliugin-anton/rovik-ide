// plans.ts
//
// Plans as files. A Plan-mode run's plan is saved as Markdown in
// `.rovik/plans/` in the project, where it can be read, edited and kept. A
// revised plan in the same chat updates the same file. Implementing a plan
// (from the chat, or from the editor bar on a plan file) uses the file's
// current text, so edits made to it count.
//
// Saving a submitted plan happens in the backend (src-tauri/src/agent_run/plan.rs).
//
// A remote chat's plan is in its worktree on its host: it's read through
// `chat_read_plan`, which reads a local chat's file too, and opens
// read-only (K09).

import { chatApi } from './chatApi';
import { chatStore } from './chatStore';
import { cloud, hostName } from './cloud';
import { requestCompare, requestOpenFile } from './editorBridge';
import { notify } from './notifications';

/** A path inside the project's plans folder. */
export function isPlanFile(path: string): boolean {
  return /[\\/]\.rovik[\\/]plans[\\/][^\\/]+\.md$/i.test(path);
}

/** The plan's path for display: relative to the project. */
export function relativePlanPath(path: string, root: string | null): string {
  if (!root) return path;
  const r = root.replace(/[\\/]+$/, '');
  return path.startsWith(r) ? path.slice(r.length + 1) : path;
}

/** The chat's plan as it is now: from its host, for a remote chat. */
export function readPlan(dialogId: string, path: string): Promise<string> {
  return chatApi.readPlan(dialogId, path);
}

/**
 * Opens the chat's plan: a local chat's file in the editor, where it can be
 * edited; a remote chat's read-only, from its host.
 */
export async function openPlan(dialogId: string | null, path: string): Promise<void> {
  const place = dialogId ? chatStore.summaryOf(dialogId)?.place : undefined;
  if (!dialogId || !place) {
    requestOpenFile({ path });
    return;
  }
  try {
    const text = await readPlan(dialogId, path);
    const name = path.split(/[\\/]/).pop() || path;
    requestCompare({
      id: `plan:${dialogId}:${path}`,
      title: `${name} (on ${hostName(place.host, cloud.hosts)})`,
      fileName: name,
      before: text,
      after: text,
    });
  } catch (err) {
    notify({
      kind: 'error',
      title: 'The plan couldn’t be read',
      message: String(err),
      timeout: 8000,
    });
  }
}

/*
 * Implementing a plan from the editor: App sends it; the chat (which may
 * not be mounted yet) takes it once it's ready, like "Add to chat".
 */

export interface ImplementPlanRequest {
  /** The plan file (absolute). */
  path: string;
  /** Its text as it is now, unsaved edits included. */
  content: string;
}

const pending: ImplementPlanRequest[] = [];
const listeners = new Set<() => void>();

export function requestImplementPlan(request: ImplementPlanRequest) {
  pending.push(request);
  for (const l of [...listeners]) l();
}

export function onImplementPlanRequest(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function takeImplementPlanRequest(): ImplementPlanRequest | undefined {
  return pending.splice(0).pop();
}
