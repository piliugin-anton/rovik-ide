// plans.ts
//
// Plans as files. A Plan-mode run's plan is saved as Markdown in
// `.rovik/plans/` in the project, where it can be read, edited and kept. A
// revised plan in the same chat updates the same file. Implementing a plan
// (from the chat, or from the editor bar on a plan file) uses the file's
// current text, so edits made to it count.
//
// Saving a submitted plan happens in the backend (src-tauri/src/agent_run/plan.rs).

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
