// subagents.ts
//
// Subagents as the chat shows them (src-tauri/src/agent_run/subagent*.rs).
// A subagent is a child chat that answers one of the lead's `task` calls; it
// isn't in the chat list. Its block in the lead's chat (ToolCallBlock) shows
// it live from the backend's events, and its result's `meta.subagent` once
// it's done. Its own messages (the transcript) load when the block opens it
// and follow its run from then on; they never go to the chat store, so a
// subagent's chat never shows up as a chat or as unread.
import { createStore, produce } from 'solid-js/store';
import type { ChatMessage } from '../types';
import { chatApi, fromRow, type MessageRow } from './chatApi';

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

export function noteSubagentStarted(e: {
  dialogId: string;
  toolCallId: string;
  childDialogId: string;
  kind: string;
  model: string;
  goal: string;
}) {
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

export function noteSubagentProgress(e: {
  dialogId: string;
  toolCallId: string;
  toolCalls: number;
  currentTool: string | null;
  model: string;
}) {
  if (!state.live[e.dialogId]?.[e.toolCallId]) return;
  setState('live', e.dialogId, e.toolCallId, {
    toolCalls: e.toolCalls,
    currentTool: e.currentTool,
    model: e.model,
  });
}

export function noteSubagentEnded(e: {
  dialogId: string;
  toolCallId: string;
  childDialogId: string;
  status: string;
}) {
  if (!state.live[e.dialogId]?.[e.toolCallId]) return;
  setState('live', e.dialogId, e.toolCallId, { status: e.status, currentTool: null });
}

/** A subagent's chat known from a reloaded page's status (agent_status). */
export function noteRunningSubagent(childDialogId: string, dialogId: string, toolCallId: string) {
  if (state.children[childDialogId]) return;
  setState('children', childDialogId, { dialogId, toolCallId });
}

/** Whether a chat is a subagent's (its events don't go to the chat store). */
export const isSubagentChat = (dialogId: string) => !!state.children[dialogId];

/** Messages a subagent's run stored: shown if its transcript is open. */
export function receiveSubagentMessages(
  childDialogId: string,
  rows: (MessageRow & { pending?: boolean })[]
) {
  const t = state.transcripts[childDialogId];
  if (!t || t.loading) return;
  const known = new Set(t.messages.map((m) => m.id));
  const added = rows
    .map((row) => ({ ...fromRow(row), ...(row.pending ? { pending: true } : {}) }))
    .filter((m) => !known.has(m.id));
  if (added.length)
    setState('transcripts', childDialogId, 'messages', (list) => [...list, ...added]);
}

export function patchSubagentMessage(
  childDialogId: string,
  messageId: string,
  patch: Omit<Partial<ChatMessage>, 'status'> & { status?: ChatMessage['status'] | null }
) {
  const t = state.transcripts[childDialogId];
  if (!t) return;
  const i = t.messages.findIndex((m) => m.id === messageId);
  if (i < 0) return;
  const { status, ...fields } = patch;
  const next: Partial<ChatMessage> = { ...fields };
  if ('status' in patch) next.status = status ?? undefined;
  if (fields.finalized) next.pending = false;
  setState('transcripts', childDialogId, 'messages', i, next);
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

export const transcript = (childDialogId: string | null | undefined): Transcript | undefined =>
  childDialogId ? state.transcripts[childDialogId] : undefined;

/** Loads a subagent's chat (once; it follows the run afterwards). */
export async function openTranscript(childDialogId: string, dialogId: string, toolCallId: string) {
  noteRunningSubagent(childDialogId, dialogId, toolCallId);
  if (state.transcripts[childDialogId]) return;
  setState('transcripts', childDialogId, { messages: [], loading: true });
  try {
    const page = await chatApi.getMessages(childDialogId, undefined, 500);
    setState('transcripts', childDialogId, { messages: page.messages, loading: false });
  } catch (err) {
    setState('transcripts', childDialogId, { messages: [], loading: false, error: String(err) });
  }
}

/*
 * ---------------------------------------------------------------------------
 * Words
 * ---------------------------------------------------------------------------
 */

const KIND_LABELS: Record<string, string> = { scout: 'Scout', reviewer: 'Change reviewer' };

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

/** "12 tool calls · 41 s" */
export function workLabel(toolCalls: number, ms?: number): string {
  const calls = `${toolCalls} tool call${toolCalls === 1 ? '' : 's'}`;
  if (ms === undefined) return calls;
  const s = Math.round(ms / 1000);
  const time = s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${calls} · ${time}`;
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

/**
 * What the subagents of the chat's latest request used (its `task` results
 * and reviews), when their providers reported it; null for none.
 */
export function runUsage(messages: ChatMessage[]): SubagentUsage | null {
  let from = 0;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === 'user' && !m.meta?.harness && !m.failed) {
      from = i;
      break;
    }
  }
  let total: SubagentUsage | null = null;
  for (const m of messages.slice(from)) {
    const u = m.meta?.subagent?.usage ?? m.meta?.review?.usage;
    if (!u) continue;
    total ??= { inputTokens: 0, outputTokens: 0 };
    total.inputTokens += u.inputTokens;
    total.outputTokens += u.outputTokens;
    if (u.cost !== undefined) total.cost = (total.cost ?? 0) + u.cost;
  }
  return total;
}

/** The report, without the framing line the lead reads (subagent.rs `for_lead`). */
export function reportBody(result: string): string {
  const blank = result.indexOf('\n\n');
  return result.startsWith('Report from a ') && blank >= 0 ? result.slice(blank + 2) : result;
}
