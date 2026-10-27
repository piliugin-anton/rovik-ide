// subagents.ts
//
// Subagents as the chat shows them (src-tauri/src/agent_run/subagent*.rs).
// A subagent is a child chat that answers one of the lead's `task` calls; it
// isn't in the chat list. Its block in the lead's chat (ToolCallBlock) shows
// it live from the backend's events, and its result's `meta.subagent` once
// it's done. Its own messages (the transcript) load when the block opens it
// and follow its run from then on; they never go to the chat store, so a
// subagent's chat never shows up as a chat or as unread.
import { batch } from 'solid-js';
import { createStore, produce } from 'solid-js/store';
import type { ChatMessage, ConnectorKind } from '../types';
import { chatApi, fromRow, type MessageRow } from './chatApi';
import { merged, type MessagePatch } from './chatStore';
import { isRequestMessage } from './requests';
import { connectorTitle, connectorUsage } from './connectors';
import { SUBAGENT_KINDS } from './subagentSettings';
import { plural } from './textUtils';

/** What a finished `task` result keeps about its subagent (subagent.rs `SubResult::meta`). */
export interface SubagentMeta {
  kind: string;
  status: 'done' | 'partial' | 'blocked' | 'failed' | 'stopped' | 'unformatted';
  childDialogId: string | null;
  model: string | null;
  modelsTried: string[];
  toolCalls: number;
  ms: number;
  error?: string | null;
  /** Tokens its providers reported, and the cost when they said (S17). */
  usage?: SubagentUsage | null;
}

export interface SubagentUsage {
  inputTokens: number;
  outputTokens: number;
  cost?: number;
}

/** A parallel group's result (task kind "workers"): each worker's step and outcome. */
export interface GroupMeta {
  name: string;
  conflict: boolean;
  workers: {
    step: number;
    label: string;
    status: string;
    childDialogId?: string | null;
    landed?: string[];
  }[];
}

/** A subagent while it runs, from the backend's events. */
export interface LiveSubagent {
  childDialogId: string;
  kind: string;
  model: string;
  goal: string;
  toolCalls: number;
  currentTool: string | null;
  /** Set when it ended (its result is about to arrive). */
  status?: string;
}

interface Transcript {
  messages: ChatMessage[];
  loading: boolean;
  error?: string;
}

const [state, setState] = createStore<{
  /** By lead chat, then `task` call id. */
  live: Record<string, Record<string, LiveSubagent>>;
  /** Child chat id → its lead chat and call. */
  children: Record<string, { dialogId: string; toolCallId: string }>;
  /** Child chat id → its messages, once opened. */
  transcripts: Record<string, Transcript>;
}>({ live: {}, children: {}, transcripts: {} });

/*
 * ---------------------------------------------------------------------------
 * Events (lib/agentRuns.ts)
 * ---------------------------------------------------------------------------
 */

/** The backend's `subagentStarted` event (lib/agentRuns.ts). */
export interface SubagentStarted {
  dialogId: string;
  toolCallId: string;
  childDialogId: string;
  kind: string;
  model: string;
  goal: string;
}

/** The backend's `subagentProgress` event. */
export interface SubagentProgress {
  dialogId: string;
  toolCallId: string;
  toolCalls: number;
  currentTool: string | null;
  model: string;
}

/** The backend's `subagentEnded` event. */
export interface SubagentEnded {
  dialogId: string;
  toolCallId: string;
  childDialogId: string;
  status: string;
}

export function noteSubagentStarted(e: SubagentStarted) {
  setState(
    produce((s) => {
      (s.live[e.dialogId] ??= {})[e.toolCallId] = {
        childDialogId: e.childDialogId,
        kind: e.kind,
        model: e.model,
        goal: e.goal,
        toolCalls: 0,
        currentTool: null,
      };
      s.children[e.childDialogId] = { dialogId: e.dialogId, toolCallId: e.toolCallId };
    })
  );
}

export function noteSubagentProgress(e: SubagentProgress) {
  if (!state.live[e.dialogId]?.[e.toolCallId]) return;
  setState('live', e.dialogId, e.toolCallId, {
    toolCalls: e.toolCalls,
    currentTool: e.currentTool,
    model: e.model,
  });
}

export function noteSubagentEnded(e: SubagentEnded) {
  if (!state.live[e.dialogId]?.[e.toolCallId]) return;
  setState('live', e.dialogId, e.toolCallId, { status: e.status, currentTool: null });
}

/** A subagent's chat known from a reloaded page's status (agent_status). */
export function noteRunningSubagent(childDialogId: string, dialogId: string, toolCallId: string) {
  if (state.children[childDialogId]) return;
  setState('children', childDialogId, { dialogId, toolCallId });
}

/** A running subagent as `agent_status` describes it (mod.rs `RunningSubagent`). */
export interface RunningSubagent {
  dialogId: string;
  parentDialogId: string;
  parentCallId: string;
  kind?: string;
  goal?: string;
  model?: string;
  startedAt?: number;
  toolCalls?: number;
}

/**
 * A reloaded page picks up a running subagent (P1.5): with what it's doing,
 * its block shows it working (and its approvals) as if it had seen it start.
 */
export function seedRunningSubagent(sub: RunningSubagent) {
  noteRunningSubagent(sub.dialogId, sub.parentDialogId, sub.parentCallId);
  if (!sub.kind || state.live[sub.parentDialogId]?.[sub.parentCallId]) return;
  noteSubagentStarted({
    dialogId: sub.parentDialogId,
    toolCallId: sub.parentCallId,
    childDialogId: sub.dialogId,
    kind: sub.kind,
    model: sub.model ?? '',
    goal: sub.goal ?? '',
  });
  if (sub.toolCalls)
    setState('live', sub.parentDialogId, sub.parentCallId, { toolCalls: sub.toolCalls });
}

/** The subagent chat answering a lead's `task` call, if known. */
export function childOf(dialogId: string | null, toolCallId: string): string | null {
  if (!dialogId) return null;
  const live = state.live[dialogId]?.[toolCallId];
  if (live) return live.childDialogId;
  for (const [child, link] of Object.entries(state.children))
    if (link.dialogId === dialogId && link.toolCallId === toolCallId) return child;
  return null;
}

/** Whether a chat is a subagent's (its events don't go to the chat store). */
export const isSubagentChat = (dialogId: string) => !!state.children[dialogId];

/**
 * P5.4: what arrives while a transcript's first page loads (rows, and
 * patches to rows that may be in the page), applied in order once it lands:
 * it's newer than the page.
 */
type Arrival = { rows: ChatMessage[] } | { messageId: string; patch: Partial<ChatMessage> };
const arrivedWhileLoading = new Map<string, Arrival[]>();
/** The load in flight per transcript: one that lands after it's forgotten is dropped. */
const loads = new Map<string, object>();

/** `later` over `page`, by id: a later row replaces its earlier copy. */
export function mergeById(page: ChatMessage[], later: ChatMessage[]): ChatMessage[] {
  const out = [...page];
  for (const m of later) {
    const i = out.findIndex((o) => o.id === m.id);
    if (i >= 0) out[i] = { ...out[i], ...m };
    else out.push(m);
  }
  return out;
}

function replay(page: ChatMessage[], arrivals: Arrival[]): ChatMessage[] {
  let out = page;
  for (const a of arrivals) {
    if ('rows' in a) out = mergeById(out, a.rows);
    else out = out.map((m) => (m.id === a.messageId ? { ...m, ...a.patch } : m));
  }
  return out;
}

function arrive(childDialogId: string, arrival: Arrival) {
  arrivedWhileLoading.set(childDialogId, [
    ...(arrivedWhileLoading.get(childDialogId) ?? []),
    arrival,
  ]);
}

/** Messages a subagent's run stored: shown if its transcript is open. */
export function receiveSubagentMessages(
  childDialogId: string,
  rows: (MessageRow & { pending?: boolean })[]
) {
  const t = state.transcripts[childDialogId];
  if (!t) return;
  const incoming = rows.map((row) => ({
    ...fromRow(row),
    ...(row.pending ? { pending: true } : {}),
  }));
  if (t.loading) return arrive(childDialogId, { rows: incoming });
  const known = new Set(t.messages.map((m) => m.id));
  const added = incoming.filter((m) => !known.has(m.id));
  if (added.length)
    setState('transcripts', childDialogId, 'messages', (list) => [...list, ...added]);
}

export function patchSubagentMessage(
  childDialogId: string,
  messageId: string,
  patch: MessagePatch
) {
  const t = state.transcripts[childDialogId];
  if (!t) return;
  const { status, ...fields } = patch;
  const next: Partial<ChatMessage> = { ...fields };
  if ('status' in patch) next.status = status ?? undefined;
  if (fields.finalized) next.pending = false;
  // P5.4: while the page loads, the row may be in it: patched once it lands.
  if (t.loading) return arrive(childDialogId, { messageId, patch: next });
  const i = t.messages.findIndex((m) => m.id === messageId);
  if (i < 0) return;
  // Tool calls and meta merged into what's there (as in the chat store): a
  // call's block opened in "Show its work" stays open as the call streams.
  const { toolCalls, meta, ...plain } = next;
  batch(() => {
    setState('transcripts', childDialogId, 'messages', i, plain);
    if ('toolCalls' in next)
      setState('transcripts', childDialogId, 'messages', i, 'toolCalls', merged(toolCalls));
    if ('meta' in next) setState('transcripts', childDialogId, 'messages', i, 'meta', merged(meta));
  });
}

/*
 * ---------------------------------------------------------------------------
 * Reading (reactive)
 * ---------------------------------------------------------------------------
 */

export const liveSubagent = (
  dialogId: string | null,
  toolCallId: string
): LiveSubagent | undefined => (dialogId ? state.live[dialogId]?.[toolCallId] : undefined);

/** Whether subagents of the chat's run are running (not ended yet). */
export const runsSubagents = (dialogId: string | null): boolean =>
  !!dialogId && Object.values(state.live[dialogId] ?? {}).some((live) => !live.status);

/**
 * A parallel group's workers while they run: the live entries under
 * "<call>#<label>" (group.rs keys each worker's events so), by label.
 */
export function liveWorkersOf(
  dialogId: string | null,
  toolCallId: string
): { label: string; live: LiveSubagent }[] {
  if (!dialogId) return [];
  const prefix = `${toolCallId}#`;
  return Object.entries(state.live[dialogId] ?? {})
    .filter(([key]) => key.startsWith(prefix))
    .map(([key, live]) => ({ label: key.slice(prefix.length), live }));
}

export const transcript = (childDialogId: string | null | undefined): Transcript | undefined =>
  childDialogId ? state.transcripts[childDialogId] : undefined;

/** Loads a subagent's chat (once; it follows the run afterwards). */
export async function openTranscript(childDialogId: string, dialogId: string, toolCallId: string) {
  noteRunningSubagent(childDialogId, dialogId, toolCallId);
  // P5.4: after an error, opening it again tries again.
  const t = state.transcripts[childDialogId];
  if (t && !t.error) return;
  const load = {};
  loads.set(childDialogId, load);
  arrivedWhileLoading.delete(childDialogId);
  setState('transcripts', childDialogId, { messages: [], loading: true, error: undefined });
  let messages: ChatMessage[] = [];
  let error: string | undefined;
  try {
    messages = (await chatApi.getMessages(childDialogId, undefined, 500)).messages;
  } catch (err) {
    error = String(err);
  }
  // Forgotten (or loaded again) meanwhile: this load isn't wanted any more.
  if (loads.get(childDialogId) !== load) return;
  loads.delete(childDialogId);
  const later = arrivedWhileLoading.get(childDialogId) ?? [];
  arrivedWhileLoading.delete(childDialogId);
  setState(
    'transcripts',
    childDialogId,
    error === undefined
      ? { messages: replay(messages, later), loading: false }
      : { messages: [], loading: false, error }
  );
}

/**
 * P5.9: a finished subagent's live entry, once its result is shown (the
 * result keeps what the block needs; the chat link stays in `children`).
 */
export function forgetLive(dialogId: string | null, toolCallId: string, childDialogId: string) {
  // Only that subagent's: a later call may have reused the id.
  if (!dialogId || state.live[dialogId]?.[toolCallId]?.childDialogId !== childDialogId) return;
  setState(
    produce((s) => {
      delete s.live[dialogId]![toolCallId];
      if (Object.keys(s.live[dialogId]!).length === 0) delete s.live[dialogId];
    })
  );
}

/**
 * P5.9: a group's workers' live entries once the group's result is shown.
 * By the call's prefix: a worker that ended without a result (its task
 * panicked) has no `childDialogId` in the result to name it by, and a
 * group's call id is never reused.
 */
export function forgetLiveWorkers(dialogId: string | null, toolCallId: string) {
  if (!dialogId || !state.live[dialogId]) return;
  const prefix = `${toolCallId}#`;
  const keys = Object.keys(state.live[dialogId]).filter((k) => k.startsWith(prefix));
  if (keys.length === 0) return;
  setState(
    produce((s) => {
      for (const key of keys) delete s.live[dialogId]![key];
      if (Object.keys(s.live[dialogId]!).length === 0) delete s.live[dialogId];
    })
  );
}

/** P5.9: a transcript no block shows any more (opening it loads it again). */
export function forgetTranscript(childDialogId: string | null | undefined) {
  if (!childDialogId) return;
  arrivedWhileLoading.delete(childDialogId);
  loads.delete(childDialogId);
  holders.delete(childDialogId);
  if (!state.transcripts[childDialogId]) return;
  setState(
    produce((s) => {
      delete s.transcripts[childDialogId];
    })
  );
}

/** P5.9: how many blocks show each transcript. */
const holders = new Map<string, number>();

/**
 * A block's hold on the transcript it shows. One subagent's chat can be on
 * two blocks (a follow-up `task` continues the same child under a new
 * call), so it's forgotten only when the last block showing it lets go.
 */
export function transcriptHold() {
  let held: string | null = null;
  const release = () => {
    if (!held) return;
    const left = (holders.get(held) ?? 1) - 1;
    if (left > 0) holders.set(held, left);
    else forgetTranscript(held);
    held = null;
  };
  return {
    /** Shows `childDialogId` (loading it if it isn't yet); held until `release`. */
    open(childDialogId: string, dialogId: string, toolCallId: string) {
      if (held !== childDialogId) {
        release();
        held = childDialogId;
        holders.set(childDialogId, (holders.get(childDialogId) ?? 0) + 1);
      }
      return openTranscript(childDialogId, dialogId, toolCallId);
    },
    release,
  };
}

/*
 * ---------------------------------------------------------------------------
 * Words
 * ---------------------------------------------------------------------------
 */

/** The kinds' labels (lib/subagentSettings.ts), and a parallel group's. */
const KIND_LABELS: Record<string, string> = Object.fromEntries([
  ...SUBAGENT_KINDS.map((k) => [k.id, k.label]),
  ['workers', 'Parallel workers'],
]);

export const kindLabel = (kind: string | undefined) =>
  (kind && KIND_LABELS[kind]) || (kind ? kind[0].toUpperCase() + kind.slice(1) : 'Subagent');

const STATUS_LABELS: Record<SubagentMeta['status'], string> = {
  done: 'done',
  partial: 'out of budget',
  blocked: 'blocked',
  failed: 'failed',
  stopped: 'stopped',
  unformatted: 'unformatted report',
};

export const statusLabel = (status: string | undefined) =>
  (status && STATUS_LABELS[status as SubagentMeta['status']]) || status || '';

/** "P/strong (after 1 failed)": the model that answered, after the ones tried before it. */
export function modelLabel<M extends string | null>(
  model: M,
  modelsTried: string[] | undefined
): M | string {
  const failed = (modelsTried?.length ?? 1) - 1;
  return failed > 0 ? `${model} (after ${failed} failed)` : model;
}

/** "12 tool calls · 41 s" */
export function workLabel(toolCalls: number, ms?: number): string {
  const calls = plural(toolCalls, 'tool call');
  if (ms === undefined) return calls;
  const s = Math.round(ms / 1000);
  const time = s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${calls} · ${time}`;
}

/**
 * "the model · 12 tool calls · 40 s", what's known of an IDE subagent's
 * work once it's done (a record's line: the tests, the tester, the
 * acceptance check, the planners and the judge), and when its budget cut it.
 */
export function workFacts(meta: {
  model?: string;
  toolCalls?: number;
  ms?: number;
  partial?: boolean;
}): string {
  const seconds = meta.ms !== undefined ? `${Math.round(meta.ms / 1000)} s` : '';
  const calls =
    meta.toolCalls !== undefined
      ? `${meta.toolCalls} tool call${meta.toolCalls === 1 ? '' : 's'}`
      : '';
  return [meta.model, calls, seconds, meta.partial ? 'cut short by its budget' : '']
    .filter(Boolean)
    .join(' · ');
}

/** "12 tool calls · bash · gpt-5": a subagent's progress while it runs. */
export function liveLabel(
  p: Pick<LiveSubagent, 'toolCalls' | 'currentTool' | 'model'>,
  withModel = true
): string {
  const doing = p.currentTool ? ` · ${p.currentTool}` : '';
  return `${workLabel(p.toolCalls)}${doing}${withModel ? ` · ${p.model}` : ''}`;
}

/** "12.3k tokens · $0.012" */
export function usageLabel(u: SubagentUsage | null | undefined): string | null {
  if (!u) return null;
  const tokens = u.inputTokens + u.outputTokens;
  const short =
    tokens >= 1_000_000
      ? `${(tokens / 1_000_000).toFixed(1)}M`
      : tokens >= 1_000
        ? `${(tokens / 1_000).toFixed(1)}k`
        : String(tokens);
  const cost =
    u.cost !== undefined ? ` · $${u.cost < 0.01 ? u.cost.toFixed(4) : u.cost.toFixed(2)}` : '';
  return `${short} tokens${cost}`;
}

/** The chat's messages from its latest request on (a failed one doesn't count). */
function latestRequest(messages: ChatMessage[]): ChatMessage[] {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (isRequestMessage(m) && !m.failed) return messages.slice(i);
  }
  return messages;
}

/**
 * What the subagents of the chat's latest request used (its `task` results
 * and reviews), and a connector's harness, when they reported it; null for none.
 */
export function runUsage(messages: ChatMessage[]): SubagentUsage | null {
  let total: SubagentUsage | null = null;
  for (const m of latestRequest(messages)) {
    // P5.3: a check round's triage counts too, and a review's guide.
    const used = [
      m.meta?.subagent?.usage ?? m.meta?.review?.usage,
      m.meta?.review?.guide?.usage,
      m.meta?.verification?.triage?.usage,
      // A connector's harness says what its turn used (lib/connectors.ts).
      connectorUsage(m.meta?.connector),
    ];
    for (const u of used) {
      if (!u) continue;
      total ??= { inputTokens: 0, outputTokens: 0 };
      total.inputTokens += u.inputTokens;
      total.outputTokens += u.outputTokens;
      if (u.cost !== undefined) total.cost = (total.cost ?? 0) + u.cost;
    }
  }
  return total;
}

/**
 * Who `runUsage` counted, for its row's label: "Subagents", "Claude Code",
 * or "Subagents and Claude Code". A connector's chat has no subagents of
 * its own, and its usage isn't theirs.
 */
export function runUsageSource(messages: ChatMessage[]): string {
  let subagents = false;
  const connectors = new Set<string>();
  for (const m of latestRequest(messages)) {
    if (
      m.meta?.subagent?.usage ??
      m.meta?.review?.usage ??
      m.meta?.review?.guide?.usage ??
      m.meta?.verification?.triage?.usage
    )
      subagents = true;
    if (connectorUsage(m.meta?.connector)) {
      connectors.add(connectorTitle(m.meta?.connector?.kind as ConnectorKind | undefined));
    }
  }
  const names = [...(subagents ? ['Subagents'] : []), ...connectors];
  return names.length > 1
    ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
    : (names[0] ?? 'Subagents');
}

/** The report, without the framing line the lead reads (subagent.rs `for_lead`). */
export function reportBody(result: string): string {
  const blank = result.indexOf('\n\n');
  return result.startsWith('Report from a ') && blank >= 0 ? result.slice(blank + 2) : result;
}
