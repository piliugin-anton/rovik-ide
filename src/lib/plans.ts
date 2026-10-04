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

/*
 * submit_plan (Plan mode): the plan goes in the tool call, not in a reply,
 * and is checked before it's saved. A reply about the conversation can no
 * longer overwrite the plan, and "Implement" is only offered once there is
 * a real plan.
 */

/** What's wrong with a plan, if anything (empty: it's fine). */
export function planProblems(markdown: string): string[] {
  const text = markdown.trim();
  const problems: string[] = [];
  if (!text) return ['The plan is empty.'];
  const first = text.split('\n').find((l) => l.trim());
  if (!first || !/^#\s+\S/.test(first)) problems.push('Start with a "# " title line.');
  const steps = /^##\s+.*\bsteps?\b.*$/im.exec(text);
  if (!steps) {
    problems.push('Add a "## Steps" section with the steps as a numbered list.');
  } else {
    const after = text.slice(steps.index + steps[0].length);
    const section = after.split(/^##\s/m)[0];
    const numbered = section.match(/^\s*\d+[.)]\s+\S/gm) ?? [];
    if (numbered.length < 2)
      problems.push('List the steps under "## Steps" as a numbered list (1., 2., ...).');
  }
  // Code belongs in the implementation, not the plan.
  let codeLines = 0;
  let longest = 0;
  for (const block of text.match(/```[\s\S]*?```/g) ?? []) {
    const lines = block.split('\n').length - 2;
    codeLines += lines;
    longest = Math.max(longest, lines);
  }
  if (longest > 15 || codeLines > 40) {
    problems.push(
      'Leave the code out: say what changes and where, in words; short snippets (a signature, a config line) are fine.'
    );
  }
  if (text.length > 40_000) problems.push('Shorten it: the plan is over 40,000 characters.');
  return problems;
}

export const SUBMIT_PLAN_SCHEMA = {
  type: 'function',
  function: {
    name: 'submit_plan',
    description:
      'Save your plan. Pass the complete plan as Markdown: a "# " title, a short summary, "## Steps" as a ' +
      'numbered list (each step: what changes and in which files, no code), "## Decisions", how to verify, and ' +
      'risks. It is checked and saved to the project, where the user can edit and implement it. When the plan ' +
      'changes, call it again with the complete revised plan (not just the changes). Keep your chat reply short: ' +
      'the plan itself goes here, not in the reply.',
    parameters: {
      type: 'object',
      properties: {
        plan: { type: 'string', description: 'The complete plan in Markdown.' },
      },
      required: ['plan'],
    },
  },
};
