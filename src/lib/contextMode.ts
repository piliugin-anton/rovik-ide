// contextMode.ts
//
// How a chat's context is managed (src-tauri/src/clm):
// - "trimming": the app trims older history to fit the model's context
//   window: out-of-date content is cut to a note, old tool results to
//   their start, the oldest turns dropped (chat_db/history.rs);
// - "clm": Context Language Models. The agent's context is mirrored to a
//   file on the virtual volume (lib/virtualFiles.ts) that the agent edits
//   with its ordinary file tools; the edited file is its next context.
//   Every state of the file is a revision it (and the user) can go back to.
//
// The mode is saved per chat; a chat that never chose one uses the last
// mode picked anywhere.
import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';

export type ContextMode = 'trimming' | 'clm';

export const CONTEXT_MODES: { id: ContextMode; label: string; hint: string }[] = [
  {
    id: 'trimming',
    label: 'Trimming',
    hint: 'The app trims older history to fit the context window: stale outputs are cut, the oldest turns dropped.',
  },
  {
    id: 'clm',
    label: 'CLMs',
    hint: 'Context Language Models: the agent edits its own context as a file, with every revision kept.',
  },
];

/** The path the agent's file tools use for its context file. */
export const CONTEXT_PATH_PREFIX = '@context';

export interface ClmStatus {
  mode: ContextMode | null;
  /** Virtual files are mounted, so CLM mode can run. */
  available: boolean;
  /** Why not. */
  problem: string | null;
  path: string;
  realPath: string | null;
  revision: number | null;
  revisions: number;
  edits: number;
  /** The agent's own (edited) context is in use. */
  projected: boolean;
  tokens: number;
  budget: number;
}

export interface ContextRevision {
  number: number;
  kind: 'context' | 'edit' | 'restore';
  createdAt: number;
  tokens: number;
}

const DEFAULT_KEY = 'rovik.contextMode.default';

function readDefault(): ContextMode {
  try {
    return localStorage.getItem(DEFAULT_KEY) === 'clm' ? 'clm' : 'trimming';
  } catch {
    return 'trimming';
  }
}

const [defaultMode, setDefaultMode] = createSignal<ContextMode>(readDefault());
/** Modes of chats seen this session (the database has the rest). */
const [modes, setModes] = createSignal<Record<string, ContextMode>>({});

/** The mode a chat uses: its own, else the last one chosen. */
export function contextModeFor(dialogId: string | null | undefined): ContextMode {
  return (dialogId && modes()[dialogId]) || defaultMode();
}

/** Remembers a chat's saved mode (from `clmStatus`). */
export function noteContextMode(dialogId: string, mode: ContextMode | null) {
  if (mode && modes()[dialogId] !== mode) setModes((m) => ({ ...m, [dialogId]: mode }));
}

/** Chooses the chat's mode, and the default for chats that have none. */
export async function setContextMode(dialogId: string | null, mode: ContextMode) {
  setDefaultMode(mode);
  try {
    localStorage.setItem(DEFAULT_KEY, mode);
  } catch {
    // Not persisted; fine for this session.
  }
  if (!dialogId) return;
  setModes((m) => ({ ...m, [dialogId]: mode }));
  await invoke('clm_set_mode', { dialogId, mode });
}

export function clmStatus(dialogId: string): Promise<ClmStatus> {
  return invoke<ClmStatus>('clm_status', { dialogId });
}

export function listContextRevisions(dialogId: string): Promise<ContextRevision[]> {
  return invoke<ContextRevision[]>('clm_list_revisions', { dialogId });
}

export function readContextRevision(dialogId: string, number: number): Promise<string> {
  return invoke<string>('clm_read_revision', { dialogId, number });
}

/** Makes a revision the context file again; it applies to the next request. */
export function restoreContextRevision(dialogId: string, number: number): Promise<void> {
  return invoke<void>('clm_restore_revision', { dialogId, number });
}

/** Drops the agent's edited context: the chat is sent as stored again. */
export function resetContext(dialogId: string): Promise<void> {
  return invoke<void>('clm_reset', { dialogId });
}

/*
 * The agent's file tools on its context file
 */

/** Whether a file tool's path means the agent's context folder. */
export function isContextPath(path: string | undefined, realDir: string | null): boolean {
  if (!path) return false;
  const p = path.trim().replace(/\\/g, '/');
  if (p === CONTEXT_PATH_PREFIX || p.startsWith(`${CONTEXT_PATH_PREFIX}/`)) return true;
  if (!realDir) return false;
  const dir = realDir.replace(/\\/g, '/').replace(/\/+$/, '');
  return p === dir || p.startsWith(`${dir}/`);
}

/** The context folder's real path, from the context file's. */
export function contextDirOf(realPath: string | null): string | null {
  if (!realPath) return null;
  const at = Math.max(realPath.lastIndexOf('/'), realPath.lastIndexOf('\\'));
  return at > 0 ? realPath.slice(0, at) : null;
}

/** read_file / edit_file / write_file on the context folder. */
export async function executeContextTool(
  dialogId: string,
  name: string,
  args: Record<string, unknown>
): Promise<string> {
  const path = typeof args.path === 'string' ? args.path : '';
  const line = (v: unknown) => {
    const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : v;
    return typeof n === 'number' && Number.isFinite(n) ? Math.floor(n) : null;
  };
  try {
    switch (name) {
      case 'read_file':
        return await invoke<string>('clm_tool_read', {
          dialogId,
          path,
          startLine: line(args.start_line),
          endLine: line(args.end_line),
        });
      case 'edit_file':
        if (typeof args.old_text !== 'string') return 'Error: "old_text" is required.';
        if (typeof args.new_text !== 'string') return 'Error: "new_text" is required.';
        return await invoke<string>('clm_tool_edit', {
          dialogId,
          path,
          oldText: args.old_text,
          newText: args.new_text,
          startLine: line(args.start_line),
          endLine: line(args.end_line),
        });
      case 'write_file':
        if (typeof args.content !== 'string') return 'Error: "content" is required.';
        return await invoke<string>('clm_tool_write', { dialogId, path, content: args.content });
      default:
        return `Error: ${name} can't be used on your context folder.`;
    }
  } catch (err) {
    return `Error: ${String(err)}`;
  }
}

/** Tools whose repeat may redo work the agent compacted away. */
const RECALLABLE = new Set([
  'read_file',
  'list_dir',
  'search_code',
  'run_command',
  'web_search',
  'browse_url',
]);

/**
 * When the agent repeats a call whose earlier result it removed from its
 * context: a note offering the revision that still holds it.
 */
export async function recallHint(
  dialogId: string,
  toolCallId: string,
  name: string,
  argsJson: string
): Promise<string> {
  if (!RECALLABLE.has(name)) return '';
  try {
    const hint = await invoke<string | null>('clm_recall_hint', {
      dialogId,
      toolCallId,
      name,
      arguments: argsJson,
    });
    return hint ?? '';
  } catch {
    return '';
  }
}

/** Saves the mode a chat is about to use, if it never chose one. */
export function ensureContextMode(dialogId: string): ContextMode {
  const mode = contextModeFor(dialogId);
  if (!modes()[dialogId]) {
    setModes((m) => ({ ...m, [dialogId]: mode }));
    void invoke('clm_set_mode', { dialogId, mode }).catch(() => undefined);
  }
  return mode;
}
