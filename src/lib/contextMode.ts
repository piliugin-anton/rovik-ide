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

/** Saves the mode a chat is about to use, if it never chose one. */
export function ensureContextMode(dialogId: string): ContextMode {
  const mode = contextModeFor(dialogId);
  if (!modes()[dialogId]) {
    setModes((m) => ({ ...m, [dialogId]: mode }));
    void invoke('clm_set_mode', { dialogId, mode }).catch(() => undefined);
  }
  return mode;
}
