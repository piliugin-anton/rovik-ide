// plans.ts
//
// Plans as files. A Plan-mode run's plan is saved as Markdown in
// `.rovik/plans/` in the project, where it can be read, edited and kept. A
// revised plan in the same chat updates the same file. Implementing a plan
// (from the chat, or from the editor bar on a plan file) uses the file's
// current text, so edits made to it count.
import { pathExists, writeFileContent } from './commands';

export const PLANS_DIR = '.rovik/plans';

/** A path inside the project's plans folder. */
export function isPlanFile(path: string): boolean {
  return /[\\/]\.rovik[\\/]plans[\\/][^\\/]+\.md$/i.test(path);
}

function join(root: string, rel: string) {
  return `${root.replace(/[\\/]+$/, '')}/${rel}`;
}

/** "2026-10-04-add-a-users-table" from the request. */
function fileStem(request: string): string {
  const slug = request
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, ' ')
    .trim()
    .split(' ')
    .filter(Boolean)
    .slice(0, 7)
    .join('-')
    .slice(0, 60);
  const d = new Date();
  const date = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  return `${date}-${slug || 'plan'}`;
}

/**
 * Saves a plan and returns its path. `existing`: the chat's earlier plan
 * file, which a revision replaces; otherwise a new name that's free.
 */
export async function savePlanFile(opts: {
  root: string;
  request: string;
  plan: string;
  existing?: string;
}): Promise<string> {
  let path = opts.existing;
  if (!path) {
    const stem = fileStem(opts.request);
    path = join(opts.root, `${PLANS_DIR}/${stem}.md`);
    for (let n = 2; (await pathExists(path)) && n < 100; n++) {
      path = join(opts.root, `${PLANS_DIR}/${stem}-${n}.md`);
    }
  }
  const body = opts.plan.trim();
  // A title, if the plan doesn't start with one.
  const titled = /^#\s/.test(body)
    ? body
    : `# Plan: ${opts.request.split('\n')[0].slice(0, 100)}\n\n${body}`;
  await writeFileContent(path, `${titled}\n`);
  return path;
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
