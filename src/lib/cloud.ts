// The hosts' own events on `cloud-event`, apart from the chats' (the app's
// `CloudEvent`, rovik-client): a message's files going up to its host, a
// project being pushed there, a remote chat's changes looking different
// after its result came in or landed, or its project synced, a chat's
// move to a host or back, and a host's connection changing. And the moves'
// commands (6.8).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { StartRequest } from './agentRuns';
import type { HostState } from './hosts';

export type CloudEvent =
  | { type: 'uploading'; dialogId: string; messageId: string; sent: number; total: number }
  | { type: 'pushing'; dialogId: string; project: string; sent: number }
  | { type: 'changed'; dialogId: string }
  | {
      type: 'moving';
      dialogId: string;
      /** "pausing", "pushing", "uploading", "importing", then "moved", "failed" or "cancelled". */
      step: string;
      message?: string;
    }
  | {
      type: 'movingBack';
      dialogId: string;
      /** "pausing", "fetching", "releasing", "pulling", "landing", then "moved", "failed" or "cancelled". */
      step: string;
      message?: string;
    }
  | { type: 'host'; host: string; state: HostState };

/** What moving a chat to a host would do, and whether it can (`MovePreflight`). */
export interface MovePreflight {
  /** Why it can't move now, if it can't. */
  blocked?: string;
  /** What will wait there for the user, from the permissions and the host's policy. */
  waits: string[];
  /** Its CLM context becomes trimming: the host has no virtual files. */
  trimming: boolean;
  /** What the project's push carries that the user hears of first. */
  notes: string[];
  /** A parallel group's "Redo sequentially" won't be there after the move. */
  redoLost: boolean;
  /** The files its messages name that the host lacks, and their size. */
  files: number;
  fileBytes: number;
}

export const movePreflight = (dialogId: string, host: string) =>
  invoke<MovePreflight>('cloud_move_preflight', { dialogId, host });

/**
 * Moves the chat to `host`. `start` is how a Continue would start it: its run
 * picks up there with it, if it paused for the move.
 */
export const moveChat = (dialogId: string, host: string, start: StartRequest | null) =>
  invoke<void>('cloud_move_chat', { dialogId, host, start });

/**
 * Moves the chat back from its host. `start` is how a Continue would start
 * it: its run picks up here with it, if it paused for the move.
 */
export const moveBack = (dialogId: string, start: StartRequest | null) =>
  invoke<void>('cloud_move_back', { dialogId, start });

/**
 * "Cancel move": the chat stays where it was. A move back can't be cancelled
 * once its host has let the chat go.
 */
export const cancelMove = (dialogId: string) => invoke<void>('cloud_cancel_move', { dialogId });

/** The latest step of a chat's move, there or back, that `event` reports, if it reports one. */
export function moveStep(event: CloudEvent, dialogId: string): string | null {
  const moves = event.type === 'moving' || event.type === 'movingBack';
  return moves && event.dialogId === dialogId ? event.step : null;
}

/** Whether the isolation bar of chat `dialogId` should look again after `event`. */
export function barChanged(event: CloudEvent, dialogId: string | null | undefined): boolean {
  return event.type === 'changed' && !!dialogId && event.dialogId === dialogId;
}

/** Calls `handler` with each cloud event, until the function it returns is called. */
export function onCloudEvent(handler: (event: CloudEvent) => void): () => void {
  const listening = listen<CloudEvent>('cloud-event', (e) => handler(e.payload));
  return () => void listening.then((stop) => stop());
}
