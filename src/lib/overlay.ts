// overlay.ts
//
// An isolated chat's changes (src-tauri/src/overlay): the chat works in its
// own view of the project, and its changes reach the project only when they
// land. This lists them, shows each against the project, and discards them.

import { invoke } from '@tauri-apps/api/core';
import type { GitConflictFile } from '../types';
import type { NotificationOptions } from './notifications';

export interface ChangedFile {
  path: string;
  change: 'added' | 'changed' | 'deleted' | 'moved';
  /** Moved: from where in the project. */
  from?: string | null;
}

export interface Conflict {
  path: string;
  reason: string;
}

export interface LastLanding {
  id: string;
  paths: string[];
  /** Seconds since 1970. */
  at: number;
}

export interface PeerConflict {
  path: string;
  /** The other chats' ids. */
  peers: string[];
  /** The same lines (false: the same file, lines can't be told apart). */
  sameLines: boolean;
}

export interface OverlayStatus {
  /** The chat has isolated changes (or an overlay with none yet). */
  isolated: boolean;
  /** Why its changes didn't land, if they didn't. */
  pending?: { reason: string; conflicts: Conflict[] } | null;
  /** Its latest landing, for Undo. */
  lastLanding?: LastLanding | null;
  /** Files other open chats are changing too, in a way that clashes. */
  peerConflicts: PeerConflict[];
  /** Files the project's newer changes were merged into (can be undone). */
  mergedIn: string[];
  /** Where the chat sees the project, while it's mounted. */
  view?: string | null;
  files: ChangedFile[];
  used: number;
  limit: number;
  /** Build output and dependencies in the chat's view (not landed). */
  byProducts: number;
  /** Parallel groups whose merge can be thrown away and redone one step at a time. */
  redoGroups?: string[];
}

export interface FileVersions {
  /** The project's file now (null: not there). */
  project: string | null;
  /** The chat's version (null: the chat deleted it). */
  chat: string | null;
}

export function overlayStatus(dialogId: string): Promise<OverlayStatus> {
  return invoke<OverlayStatus>('overlay_status', { dialogId });
}

export function overlayFile(dialogId: string, path: string): Promise<FileVersions> {
  return invoke<FileVersions>('overlay_file', { dialogId, path });
}

export function overlayDiscard(dialogId: string): Promise<void> {
  return invoke<void>('overlay_discard', { dialogId });
}

export interface LandResult {
  landed?: { id: string; paths: string[] } | null;
  conflicts: Conflict[];
  error?: string | null;
}

/** Lands the chat's changes now; `keepChat`: on conflicts, its version wins. */
export function overlayLand(dialogId: string, keepChat: boolean): Promise<LandResult> {
  return invoke<LandResult>('overlay_land', { dialogId, keepChat });
}

/** Undoes a landing; returns the files left as they are (changed since). */
export function overlayUndo(dialogId: string, id: string): Promise<Conflict[]> {
  return invoke<Conflict[]>('overlay_undo', { dialogId, id });
}

/** A conflicted file: the project's version ("ours"), the chat's ("theirs"). */
export function overlayConflictFile(dialogId: string, path: string): Promise<GitConflictFile> {
  return invoke<GitConflictFile>('overlay_conflict_file', { dialogId, path });
}

/**
 * Settles a conflicted file: `content` becomes the chat's version (null:
 * deleted), or `side` takes one whole version. Returns the conflicts left.
 */
export function overlayConflictResolve(
  dialogId: string,
  path: string,
  content: string | null,
  side?: 'ours' | 'theirs'
): Promise<Conflict[]> {
  return invoke<Conflict[]>('overlay_conflict_resolve', { dialogId, path, content, side });
}

/**
 * Hands the conflicts to the chat: markers go into its files. Returns the
 * files and the message to send it (empty when nothing could be handed over).
 */
/** Redo sequentially: the group's merge is thrown away; returns the message to send the chat. */
export function overlayRedoGroup(dialogId: string, group: string): Promise<string> {
  return invoke<string>('overlay_redo_group', { dialogId, group });
}

/**
 * Redo sequentially, once the user said yes: the group's merge is thrown
 * away, then the chat is asked to carry out its steps. Two steps, each
 * reported on its own (P5.9): when only the first worked, the files are
 * back and the user is told to ask the chat. Resolves with whether the
 * chat was asked.
 */
export async function redoGroup(
  dialogId: string,
  group: string,
  deps: {
    redo: (dialogId: string, group: string) => Promise<string>;
    askChat: (dialogId: string, request: string, shown: string) => Promise<boolean>;
    notify: (options: NotificationOptions) => unknown;
  }
): Promise<boolean> {
  let request: string;
  // What the chat shows as the user's message (round 3: not "Resolve the
  // conflicts", which is the conflicts bar's request).
  const shown = `Redo the group “${group}” one step at a time.`;
  try {
    request = await deps.redo(dialogId, group);
  } catch (e) {
    deps.notify({ kind: 'error', title: 'Couldn’t redo the group', message: String(e) });
    return false;
  }
  const title = 'The group was undone, but the chat couldn’t be asked to redo it';
  const rest =
    'Its files are back to what they were before it; ask the chat to carry out the steps again.';
  try {
    if (await deps.askChat(dialogId, request, shown)) return true;
    deps.notify({ kind: 'error', title, message: `The chat didn’t start. ${rest}` });
  } catch (e) {
    deps.notify({ kind: 'error', title, message: `${String(e)}. ${rest}` });
  }
  return false;
}

export function overlayConflictsToChat(
  dialogId: string
): Promise<{ files: string[]; request: string }> {
  return invoke('overlay_conflicts_to_chat', { dialogId });
}

/** What an undo did: files put back, and files left as they are because the chat changed them since. */
export interface UndoneMerge {
  restored: string[];
  changedSince: string[];
}

/** Takes the project's merged-in changes back out of the chat's files. */
export function overlayUndoMerge(dialogId: string): Promise<UndoneMerge> {
  return invoke<UndoneMerge>('overlay_undo_merge', { dialogId });
}

/** Takes markers just handed to the chat back out of these files (it couldn't take them). */
export function overlayUnmark(dialogId: string, files: string[]): Promise<UndoneMerge> {
  return invoke<UndoneMerge>('overlay_unmark', { dialogId, files });
}
