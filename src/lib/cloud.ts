// The hosts' own events on `cloud-event`, apart from the chats' (the app's
// `CloudEvent`, rovik-client): a message's files going up to its host, a
// project being pushed there, a remote chat's changes looking different
// after its result came in or landed, or its project synced, a chat's
// move to a host or back, and a host's connection changing. And the moves'
// commands (6.8).
//
// `cloud` keeps what the chats show of it: the hosts' names and
// connections, and each chat's move as it goes (U02); which hosts have
// caught up since they connected, and each chat's upload (U04). A host
// that caught up before the window listened says so in its summary, and
// `onCaughtUp` tells of each host as it catches up (U06). `initCloud`
// starts it as the app starts, and keeps the chats' places with it.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createStore, produce } from 'solid-js/store';
import type { StartRequest } from './agentRuns';
import { chatStore } from './chatStore';
import type { HostState, Tone } from './hosts';
import type { ChatPlace } from '../types';

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
  /** A host's connection changed; one that turned this computer away says why. */
  | { type: 'host'; host: string; state: HostState; problem?: string }
  /** The chats mirrored from the host have pulled since it connected (K23). */
  | { type: 'caughtUp'; host: string };

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
  /**
   * The host's Rovik is too old to take the chat, and the user is one of its
   * administrators there, who upgrade it for everyone.
   */
  upgrade?: boolean;
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

/** A host as the chats show it: its name, and its connection once deployed. */
export interface HostSummary {
  id: string;
  name: string;
  deployed: boolean;
  state?: HostState;
  /**
   * Its mirrored chats pulled since it connected: for a window that missed
   * `caughtUp`, as the hosts connect while the app starts.
   */
  caughtUp?: boolean;
  /**
   * A terminal can open on it: a host of the settings, which SSH reaches,
   * not a daemon that the developer setting names by its socket.
   */
  terminal?: boolean;
  /** It works on its own (H02): its chats start in Autopilot. */
  autonomous?: boolean;
}

export const hostSummaries = () => invoke<HostSummary[]>('hosts_summaries');

/** A chat's move as it goes, from `cloud-event`. */
export interface MoveProgress {
  /** "there": to `host`; "back": from it. */
  direction: 'there' | 'back';
  /** Known when this app started the move. */
  host?: string;
  /** Its latest step (`CloudEvent`'s `moving` and `movingBack`), or "starting". */
  step: string;
  message?: string;
  /** The project's pack: the bytes sent so far. */
  pushed?: number;
}

/** The steps a move ends with. */
const ENDS = new Set(['moved', 'failed', 'cancelled']);

/** Whether a move is over, or none was seen. */
export const moveEnded = (move: MoveProgress | undefined) => !move || ENDS.has(move.step);

/** A remote send's files going up before its message (6.11). */
export interface UploadProgress {
  messageId: string;
  sent: number;
  total: number;
}

interface CloudState {
  hosts: HostSummary[];
  /** Each chat's latest move this session: an ended one stays until the next. */
  moves: Record<string, MoveProgress>;
  /**
   * The hosts whose chats have pulled since they connected: only then is a
   * remote chat's draft from before a restart judged (K23).
   */
  caughtUp: Record<string, true>;
  /** Each chat's upload while its send's files go up. */
  uploads: Record<string, UploadProgress>;
}

const [cloud, setCloud] = createStore<CloudState>({
  hosts: [],
  moves: {},
  caughtUp: {},
  uploads: {},
});

/** The hosts the chats know of, and the chats' moves. */
export { cloud };

let started: Promise<void> | null = null;

/** Keeps `cloud`, and the chats' places, from `cloud-event`: once, as the app starts. */
export function initCloud(): Promise<void> {
  started ??= (async () => {
    await listen<CloudEvent>('cloud-event', (e) => track(e.payload));
    await refreshHosts();
  })();
  return started;
}

/** The hosts' names and connections again, as after the hosts page changed them. */
export async function refreshHosts(): Promise<void> {
  try {
    const hosts = await hostSummaries();
    setCloud('hosts', hosts);
    // One that caught up before this window listened.
    for (const host of hosts) {
      if (host.caughtUp && host.state === 'ready') markCaughtUp(host.id);
    }
  } catch (err) {
    console.error('Hosts unavailable:', err);
  }
}

const caughtUpListeners = new Set<(host: string) => void>();

/** Calls `listener` with each host as it catches up, until the function it returns is called. */
export function onCaughtUp(listener: (host: string) => void): () => void {
  caughtUpListeners.add(listener);
  return () => void caughtUpListeners.delete(listener);
}

/** The host caught up, as its event or its summary says: once until it goes away. */
function markCaughtUp(host: string) {
  if (cloud.caughtUp[host]) return;
  setCloud('caughtUp', host, true);
  for (const listener of [...caughtUpListeners]) listener(host);
}

function track(event: CloudEvent) {
  switch (event.type) {
    case 'host':
      // A host the list doesn't have yet (the list read before it was
      // added, or before the app's hosts connected): read again, which
      // brings its state with it.
      if (!cloud.hosts.some((h) => h.id === event.host)) {
        void refreshHosts();
      } else {
        setCloud('hosts', (h) => h.id === event.host, 'state', event.state);
      }
      // Away, it has to catch up again once it's back.
      if (event.state !== 'ready') {
        setCloud(
          'caughtUp',
          produce((hosts) => {
            delete hosts[event.host];
          })
        );
      }
      return;
    case 'caughtUp':
      markCaughtUp(event.host);
      return;
    case 'uploading':
      if (event.sent >= event.total) clearUpload(event.dialogId);
      else {
        const { messageId, sent, total } = event;
        setCloud('uploads', event.dialogId, { messageId, sent, total });
      }
      return;
    case 'moving':
    case 'movingBack': {
      const direction = event.type === 'moving' ? 'there' : 'back';
      const was = cloud.moves[event.dialogId];
      const same = was?.direction === direction && !ENDS.has(was.step);
      setCloud('moves', event.dialogId, {
        direction,
        host: same ? was.host : undefined,
        step: event.step,
        message: event.message,
        pushed: same ? was.pushed : undefined,
      });
      // Where the move puts the chat, as it starts and once it's over.
      if (event.step === 'pausing' || ENDS.has(event.step)) {
        void chatStore.refreshPlace(event.dialogId);
      }
      return;
    }
    case 'pushing':
      if (!moveEnded(cloud.moves[event.dialogId])) {
        setCloud('moves', event.dialogId, 'pushed', event.sent);
      }
      return;
  }
}

/** A send's upload is over: it went up, or the send ended without it. */
export function clearUpload(dialogId: string) {
  setCloud(
    'uploads',
    produce((uploads) => {
      delete uploads[dialogId];
    })
  );
}

/**
 * Moves the chat to `host`, or back from it: read-only until the move ends.
 * Resolves once it has; a rejection's reason is the move's.
 */
export async function startMove(
  dialogId: string,
  direction: 'there' | 'back',
  host: string,
  start: StartRequest | null
): Promise<void> {
  setCloud('moves', dialogId, {
    direction,
    host,
    step: 'starting',
    message: undefined,
    pushed: undefined,
  });
  chatStore.setPlace(dialogId, { kind: direction === 'there' ? 'moving' : 'movingBack', host });
  try {
    if (direction === 'there') await moveChat(dialogId, host, start);
    else await moveBack(dialogId, start);
  } catch (err) {
    // Refused before it began: no step said so.
    if (!ENDS.has(cloud.moves[dialogId]?.step ?? '')) {
      setCloud('moves', dialogId, { step: 'failed', message: String(err) });
    }
    throw err;
  } finally {
    await chatStore.refreshPlace(dialogId);
  }
}

/**
 * The chat is on a host that works on its own (H02): its messages go in
 * Autopilot unless the user picks another mode for it.
 */
export function worksOnItsOwn(place: ChatPlace | undefined, hosts: HostSummary[]): boolean {
  if (place?.kind !== 'host') return false;
  return !!hosts.find((h) => h.id === place.host)?.autonomous;
}

/** A host's name, or its id when this app doesn't know it. */
export function hostName(id: string, hosts: HostSummary[]): string {
  return hosts.find((h) => h.id === id)?.name ?? id;
}

/** What the chat's header says of where it is. */
export function placeLabel(
  place: ChatPlace | undefined,
  hosts: HostSummary[]
): { label: string; tone: Tone; title: string } {
  if (!place) {
    return { label: 'This computer', tone: 'muted', title: 'This chat runs on this computer.' };
  }
  const name = hostName(place.host, hosts);
  const readOnly = 'It’s read-only until the move ends.';
  switch (place.kind) {
    case 'moving':
      return {
        label: `Moving to ${name}…`,
        tone: 'warn',
        title: `This chat is moving to ${name}. ${readOnly}`,
      };
    case 'movingBack':
      return {
        label: `Moving back from ${name}…`,
        tone: 'warn',
        title: `This chat is moving back from ${name}. ${readOnly}`,
      };
    case 'host':
      switch (hosts.find((h) => h.id === place.host)?.state) {
        case 'ready':
          return {
            label: name,
            tone: 'ok',
            title: `This chat runs on ${name}, and goes on while this computer is away.`,
          };
        case 'offline':
          return {
            label: `${name} (offline)`,
            tone: 'error',
            title: `This chat is on ${name}, which can’t be reached now. What it does there shows when ${name} is back.`,
          };
        case 'version-mismatch':
          return {
            label: `${name} (needs a newer app)`,
            tone: 'error',
            title: `${name} serves only newer versions of this app: update it to reach this chat.`,
          };
        case 'refused':
          return {
            label: `${name} (refused)`,
            tone: 'error',
            title: `${name} turned this computer away: Settings → Cloud hosts says why. What this chat does there shows once it's fixed.`,
          };
        default:
          return { label: name, tone: 'warn', title: `Connecting to ${name}…` };
      }
  }
}

/**
 * Why the chat can't change now, if it can't: a moving chat is read-only,
 * as is one whose host can't be reached (6.10).
 */
export function readOnlyReason(place: ChatPlace | undefined, hosts: HostSummary[]): string | null {
  if (!place) return null;
  const name = hostName(place.host, hosts);
  switch (place.kind) {
    case 'moving':
      return `This chat is moving to ${name}: it’s read-only until the move ends.`;
    case 'movingBack':
      return `This chat is moving back from ${name}: it’s read-only until the move ends.`;
    case 'host':
      // 6.10: what it shows is the mirror's, until its host is back.
      switch (hosts.find((h) => h.id === place.host)?.state) {
        case 'offline':
          return `${name} is offline: this chat is read-only until it’s back.`;
        case 'version-mismatch':
          return `${name} serves only newer versions of this app: update it to write in this chat.`;
        case 'refused':
          return `${name} turned this computer away: this chat is read-only until that's fixed in Settings → Cloud hosts.`;
        default:
          return null;
      }
  }
}

const THERE_STEPS: Record<string, string> = {
  starting: 'Starting…',
  pausing: 'Pausing its run at a safe point…',
  pushing: 'Sending the project…',
  uploading: 'Sending the files its messages name…',
  importing: 'Handing the chat over…',
  moved: 'Moved.',
  failed: 'The move failed.',
  cancelled: 'The move was cancelled: the chat is still on this computer.',
};

const BACK_STEPS: Record<string, string> = {
  starting: 'Starting…',
  pausing: 'Pausing its run at a safe point…',
  fetching: 'Fetching its latest result…',
  releasing: 'Asking the server to let it go…',
  pulling: 'Bringing its messages back…',
  landing: 'Bringing its changes back…',
  moved: 'Back on this computer.',
  failed: 'The move back failed.',
  cancelled: 'The move back was cancelled: the chat is still on the server.',
};

/** A move's latest step, in words. */
export function moveStepLabel(move: MoveProgress): string {
  const steps = move.direction === 'there' ? THERE_STEPS : BACK_STEPS;
  return steps[move.step] ?? move.step;
}
