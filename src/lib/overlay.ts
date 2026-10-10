// overlay.ts
//
// An isolated chat's changes (src-tauri/src/overlay): the chat works in its
// own view of the project, and its changes reach the project only when they
// land. This lists them, shows each against the project, and discards them.

import { invoke } from '@tauri-apps/api/core';

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

export interface OverlayStatus {
  /** The chat has isolated changes (or an overlay with none yet). */
  isolated: boolean;
  /** Why its changes didn't land, if they didn't. */
  pending?: { reason: string; conflicts: Conflict[] } | null;
  /** Its latest landing, for Undo. */
  lastLanding?: LastLanding | null;
  /** Where the chat sees the project, while it's mounted. */
  view?: string | null;
  files: ChangedFile[];
  used: number;
  limit: number;
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
