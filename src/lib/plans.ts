// plans.ts
//
// Plans as files. A Plan-mode run's plan is saved as Markdown in
// `.rovik/plans/` in the project, where it can be read, edited and kept. A
// revised plan in the same chat updates the same file. Implementing a plan
// (from the chat, or from the editor bar on a plan file, which saves it
// first) sends its path: the backend reads the file as it is then, so edits
// made to it count, and writes the request the model gets
// (src-tauri/crates/rovik-core/src/agent_run/plan.rs, `expand_implementation`).
//
// Saving a submitted plan happens in the backend (src-tauri/src/agent_run/plan.rs).
//
// A remote chat's plan is in its worktree on its host: it's read through
// `chat_read_plan`, which reads a local chat's file too, and opens
// read-only (K09).
//
// A workspace chat's plan is saved in its first folder, with a line under
// its title naming the chat's folders (`Folders: api, web`), by which
// Implement chooses the chat that carries it out (D28, `chatForPlan`).

import type { ChatMessage } from '../types';
import { chatApi } from './chatApi';
import { chatStore } from './chatStore';
import { cloud, hostName } from './cloud';
import { openInEditor } from './openInEditor';
import { notify } from './notifications';

/**
 * Starts the line under a workspace chat's plan's title that names its
 * folders (D28; `FOLDERS_LINE` in plan.rs).
 */
const FOLDERS_LINE = 'Folders: ';

/**
 * The folders a plan names on the line under its title (`Folders: api, web`):
 * a workspace chat wrote it, across them. Null for a plan without the line.
 */
export function planFolders(text: string): string[] | null {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  if (!lines[0]?.startsWith('# ')) return null;
  const line = lines.slice(1).find((l) => l.trim() !== '') ?? '';
  if (!line.startsWith(FOLDERS_LINE)) return null;
  const names = line
    .slice(FOLDERS_LINE.length)
    .split(',')
    .map((name) => name.trim())
    .filter((name) => name !== '');
  return names.length > 0 ? names : null;
}

/** Where Implement carries out a plan whose `Folders:` line names folders (D28). */
export type PlanChat =
  /** The open chat: a workspace chat of the window's workspace with every folder named. */
  | { kind: 'open' }
  /** A new workspace chat across the folders named, one folder too. */
  | { kind: 'new'; folders: string[] }
  /** None: the window has no workspace, or it lacks a folder named; `why` says which. */
  | { kind: 'none'; why: string };

/**
 * D28: the chat that carries out a plan naming the folders `named`. The open
 * chat when it's a workspace chat of the window's workspace (`workspace`, its
 * id and its folders' names) that works in each of them (`open.names`: the
 * names it checked, or `all`); otherwise a new workspace chat across them,
 * even one folder: a folder chat would read `api/src/x.ts` as
 * `api/api/src/x.ts`. A name the workspace lacks (a folder may be in several
 * workspaces, D5) starts nothing, nor does a window without a workspace.
 */
export function chatForPlan(
  named: readonly string[],
  workspace: { id: string; names: readonly string[] } | null,
  open: { projectId?: string | null; names: readonly string[] | 'all' | null } | null
): PlanChat {
  if (!workspace) {
    const why = `The plan works in a workspace’s folders (${named.join(', ')}): open the workspace to implement it.`;
    return { kind: 'none', why };
  }
  const lacking = named.filter((name) => !workspace.names.includes(name));
  if (lacking.length > 0) {
    return {
      kind: 'none',
      why: `The plan works in ${lacking.join(', ')}, which this workspace doesn’t have.`,
    };
  }
  const names = open?.projectId === workspace.id ? open.names : null;
  const inOpen = names === 'all' || (names !== null && named.every((n) => names.includes(n)));
  return inOpen ? { kind: 'open' } : { kind: 'new', folders: [...named] };
}

/** A path inside the project's plans folder. */
export function isPlanFile(path: string): boolean {
  return /[\\/]\.rovik[\\/]plans[\\/][^\\/]+\.md$/i.test(path);
}

/**
 * The message that implements a saved plan, in Agent mode: a short line for
 * the chat, naming it as the chat does (`shown`, `chatPathOf`), and the
 * plan's path, from which the backend sends the model the plan in full.
 */
export function implementationMessage(
  file: string,
  shown: string
): { content: string; meta: NonNullable<ChatMessage['meta']> } {
  return {
    content: `Implement the plan in \`${shown}\`.`,
    meta: { mode: 'agent', implementsPlan: true, planPath: file },
  };
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
    await openInEditor({ path, focus: true });
    return;
  }
  try {
    const text = await readPlan(dialogId, path);
    const name = path.split(/[\\/]/).pop() || path;
    await openInEditor({
      path: `plan:${dialogId}:${path}`,
      focus: true,
      marks: {
        compare: {
          title: `${name} (on ${hostName(place.host, cloud.hosts)})`,
          fileName: name,
          before: text,
          after: text,
        },
      },
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
  /** The plan file (absolute), saved. */
  path: string;
  /** The folders its `Folders:` line names (`planFolders`): a workspace chat's plan. */
  folders?: string[] | null;
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

/** The request `takeImplementPlanRequest` would take, left waiting: which chat takes it decides whether it can now. */
export function peekImplementPlanRequest(): ImplementPlanRequest | undefined {
  return pending[pending.length - 1];
}
