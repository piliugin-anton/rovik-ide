// remoteNotices.ts
//
// What waits for the user in a chat on a host, in the corner (U06, 6.9).
// An approval or a question from one notifies as a local question does,
// its Show opening the chat. A local chat's approvals stay in the chat, as
// before.
//
// While a host connects, as the app starts or when its link came back, its
// chats' approvals and questions are told again all at once. They don't
// notify one by one: once the host has caught up (`onCaughtUp`), one
// notice sums up what waits on it, and those told before it went away
// fold into it. Settling what it counted counts again. A host that doesn't
// catch up (a pull that failed) gets its notice a while after it connected.
import { createEffect, createRoot, untrack } from 'solid-js';
import { approvalsWaiting } from './approvals';
import { chatApi } from './chatApi';
import { chatStore } from './chatStore';
import { cloud, hostName, onCaughtUp, onCloudEvent } from './cloud';
import { dismiss, notifications, notify, type NotificationOptions } from './notifications';
import { questionsWaiting } from './questions';
import { plural } from './textUtils';
import type { ChatPlace } from '../types';

/** What a notification's Show brings up: a chat, or the chat list. */
export type ShowTarget = { dialogId: string } | { list: true };

/** Something a chat waits on the user for, newly asked. */
export interface Waiting {
  kind: 'approval' | 'question';
  dialogId: string;
  toolCallId: string;
  /** The chat it shows in: a subagent's lead's. */
  chat: string;
  /** Its own notification; `host`: the name of the chat's host, if remote. */
  notice: (host?: string) => NotificationOptions;
  /** Whether it still waits: it may be settled while its chat is looked up. */
  pending: () => boolean;
}

export const approvalNoticeId = (dialogId: string, toolCallId: string) =>
  `approval-${dialogId}-${toolCallId}`;
export const questionNoticeId = (dialogId: string, toolCallId: string) =>
  `question-${dialogId}-${toolCallId}`;
const summaryId = (host: string) => `waiting-on-${host}`;

/** One thing that waits, as a host's notice counts it. */
interface Counted {
  kind: Waiting['kind'];
  chat: string;
  /** The notification it had on its own. */
  noticeId: string;
  /** Questions: how many the call asks. */
  count: number;
}

/** How long a host that connected has to catch up before what waits on it is told. */
const CATCH_UP_WAIT = 30_000;
/** The hosts whose waiting things are told one by one: caught up, or waited for. */
const live = new Set<string>();
const catchUps = new Map<string, ReturnType<typeof setTimeout>>();

/** Orders what was asked against what each host's notice read (`summed`). */
let clock = 0;
const summed = new Map<string, number>();
/** What each host's notice counted, by notification id. */
const counted = new Map<string, Set<string>>();
/** Hosts whose notice is being made, and what came for them meanwhile. */
const summing = new Map<string, { waiting: Waiting; seen: number }[]>();
/** Chats being looked up for something asked: a notice waits for them. */
const lookups = new Set<Promise<void>>();

let seeded: Promise<unknown> = Promise.resolve();
let show: (target: ShowTarget) => void = () => {};
let started = false;

/**
 * Sums up what waits on each host as it catches up, once what waited
 * already (`agent_status`) is in the stores (`seeded`). `showChat` is what
 * a notice's Show does.
 */
export function initRemoteNotices(options: {
  seeded: Promise<unknown>;
  show: (target: ShowTarget) => void;
}) {
  seeded = options.seeded;
  show = options.show;
  if (started) return;
  started = true;
  onCaughtUp(goLive);
  onCloudEvent((event) => {
    if (event.type !== 'host') return;
    if (event.state === 'ready') connected(event.host);
    else away(event.host);
  });
  // A host that connected before the window listened, as the app starts.
  createRoot(() =>
    createEffect(() => {
      for (const host of cloud.hosts ?? []) {
        if (host.state === 'ready') untrack(() => connected(host.id));
      }
    })
  );
}

/** The host connected: what waits on it is told once it caught up, or in a while. */
function connected(host: string) {
  if (live.has(host) || catchUps.has(host)) return;
  const timer = setTimeout(() => goLive(host), CATCH_UP_WAIT);
  // Node's (tests): it mustn't keep the process alive.
  (timer as { unref?: () => void }).unref?.();
  catchUps.set(host, timer);
}

/** The host went away: when it's back, what it tells again folds into a notice. */
function away(host: string) {
  clearTimeout(catchUps.get(host));
  catchUps.delete(host);
  live.delete(host);
}

/** One notice sums up what waits on the host; what comes after is told on its own. */
function goLive(host: string) {
  clearTimeout(catchUps.get(host));
  catchUps.delete(host);
  if (live.has(host)) return;
  live.add(host);
  void sumUp(host);
}

/** Where a chat is: absent for one on this computer. */
async function placeOf(dialogId: string): Promise<ChatPlace | undefined> {
  const known = chatStore.summaryOf(dialogId);
  if (known) return known.place;
  try {
    return (await chatApi.getDialog(dialogId))?.place ?? undefined;
  } catch {
    return undefined;
  }
}

/**
 * Tells of something newly asked: a local question notifies now, a local
 * approval doesn't; a remote chat's notifies if its host is live, and
 * otherwise waits for the notice that sums up.
 */
export function tellWaiting(waiting: Waiting): void {
  const seen = ++clock;
  const known = chatStore.summaryOf(waiting.chat);
  if (known) return decide(waiting, known.place, seen);
  const lookup = placeOf(waiting.chat).then((place) => decide(waiting, place, seen));
  lookups.add(lookup);
  void lookup.finally(() => lookups.delete(lookup));
}

function decide(waiting: Waiting, place: ChatPlace | undefined, seen: number) {
  if (!waiting.pending()) return;
  if (!place) {
    if (waiting.kind === 'question') notify(waiting.notice());
    return;
  }
  const host = place.host;
  // Its host connecting: the notice once it caught up tells of it.
  if (!live.has(host)) return;
  const meanwhile = summing.get(host);
  if (meanwhile) {
    meanwhile.push({ waiting, seen });
    return;
  }
  // Asked before the host's notice read the stores: it counted it.
  if ((summed.get(host) ?? 0) >= seen) return;
  notify(waiting.notice(hostName(host, cloud.hosts)));
}

/** What waits now, in any chat. */
function waitingNow(): Counted[] {
  return [
    ...approvalsWaiting().map((p) => ({
      kind: 'approval' as const,
      chat: p.parentDialogId ?? p.dialogId,
      noticeId: approvalNoticeId(p.dialogId, p.toolCallId),
      count: 1,
    })),
    ...questionsWaiting().map((q) => ({
      kind: 'question' as const,
      chat: q.dialogId,
      noticeId: questionNoticeId(q.dialogId, q.toolCallId),
      count: Math.max(q.questions.length, 1),
    })),
  ];
}

/** One notice sums up what waits on the host. */
async function sumUp(host: string) {
  const meanwhile: { waiting: Waiting; seen: number }[] = [];
  summing.set(host, meanwhile);
  try {
    await seeded;
    await Promise.allSettled([...lookups]);
    const now = waitingNow();
    summed.set(host, ++clock);
    const places = await Promise.all(now.map((w) => placeOf(w.chat)));
    const here = now.filter((_, i) => places[i]?.host === host);
    counted.set(host, new Set(here.map((w) => w.noticeId)));
    showSummary(host, here);
    // One notice: those told one by one before the host went away fold into it.
    for (const w of here) dismiss(w.noticeId);
  } finally {
    summing.delete(host);
  }
  // Asked while the notice was made, after it read the stores: on their own.
  for (const { waiting, seen } of meanwhile) {
    if (live.has(host)) decide(waiting, { kind: 'host', host }, seen);
  }
}

function showSummary(host: string, here: Counted[]) {
  const id = summaryId(host);
  if (here.length === 0) {
    dismiss(id);
    return;
  }
  const sum = (kind: Counted['kind']) =>
    here.filter((w) => w.kind === kind).reduce((n, w) => n + w.count, 0);
  const approvals = sum('approval');
  const questions = sum('question');
  const parts = [
    approvals ? plural(approvals, 'approval') : '',
    questions ? plural(questions, 'question') : '',
  ].filter(Boolean);
  const chats = [...new Set(here.map((w) => w.chat))];
  const where = chats.length > 1 ? `, in ${chats.length} chats` : '';
  notify({
    id,
    kind: 'info',
    title: `Waiting for you on ${hostName(host, cloud.hosts)}`,
    message: `${parts.join(' and ')}${where}.`,
    actions: [
      {
        label: 'Show',
        primary: true,
        run: () => show(chats.length === 1 ? { dialogId: chats[0] } : { list: true }),
      },
    ],
  });
}

/**
 * Something waiting was settled: a host's notice still up counts what it
 * summed up again, or goes once none of it waits.
 */
export function waitingSettled(): void {
  const up = new Set(notifications().map((n) => n.id));
  const now = waitingNow();
  for (const [host, ids] of counted) {
    if (!up.has(summaryId(host))) continue;
    showSummary(
      host,
      now.filter((w) => ids.has(w.noticeId))
    );
  }
}
