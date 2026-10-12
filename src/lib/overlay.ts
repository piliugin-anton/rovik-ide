// overlay.ts
//
// An isolated chat's changes (src-tauri/src/overlay): the chat works in its
// own view of the project, and its changes reach the project only when they
// land. This lists them, shows each against the project, and discards them.

import { invoke } from '@tauri-apps/api/core';
import type { GitConflictFile } from '../types';

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

/**
 * Lands the chat's changes now. `keepChat`: on the conflicts shown (the
 * status's pending list), its version wins; a conflict that came up since
 * comes back in `conflicts` instead, and nothing lands.
 */
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
