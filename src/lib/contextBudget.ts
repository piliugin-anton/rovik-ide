// contextBudget.ts
//
// Fits a chat's history into the model's context window before sending.
// Without this, every message and every tool result ever produced in a chat
// goes out on every turn: long chats overflow the window (provider errors)
// and cost grows each turn.
//
// Only the copy sent to the model is trimmed; the stored chat is untouched.
//
// Order of trimming, cheapest information first:
//  1. Old tool results (before the current request) are cut to a short head,
//     and their screenshots dropped. The agent can re-run a tool.
//  2. Oldest whole turns are dropped (a user message and everything up to
//     the next one), so a tool call is never separated from its result.
//  3. As a last resort, tool results within the current request are cut,
//     oldest first, keeping the most recent one intact.
import type { ChatMessage } from '../types';

/** Used when the server doesn't report the model's context window. */
export const DEFAULT_CONTEXT_TOKENS = 32_000;

/** Share of the window for history; the rest covers the system prompt, tool schemas and the reply. */
const HISTORY_SHARE = 0.7;
const MIN_HISTORY_TOKENS = 2_000;

/** Rough cost of one image for vision models. */
const IMAGE_TOKENS = 1_000;
/** Per-message overhead (role, separators). */
const MESSAGE_OVERHEAD = 4;
/** How much of a trimmed tool result to keep. */
const TOOL_KEEP_CHARS = 1_200;

/**
 * No tokenizer here, so estimate. ~3 characters per token is deliberately
 * pessimistic (English prose is ~4; code and Cyrillic are closer to 2–3),
 * so the estimate errs towards sending less rather than overflowing.
 */
function textTokens(text: string): number {
  return Math.ceil(text.length / 3);
}

export function estimateTokens(message: ChatMessage, vision: boolean): number {
  let tokens = MESSAGE_OVERHEAD + textTokens(message.content ?? '');
  for (const call of message.toolCalls ?? []) {
    tokens += textTokens(call.name) + textTokens(call.arguments);
  }
  if (vision) tokens += (message.images?.length ?? 0) * IMAGE_TOKENS;
  return tokens;
}

export function historyBudget(contextTokens: number | undefined): number {
  const window = contextTokens && contextTokens > 0 ? contextTokens : DEFAULT_CONTEXT_TOKENS;
  return Math.max(MIN_HISTORY_TOKENS, Math.floor(window * HISTORY_SHARE));
}

function truncateToolResult(message: ChatMessage): ChatMessage {
  const content = message.content ?? '';
  const hasImages = !!message.images?.length;
  if (content.length <= TOOL_KEEP_CHARS && !hasImages) return message;

  const parts = [content.length > TOOL_KEEP_CHARS ? content.slice(0, TOOL_KEEP_CHARS) : content];
  if (content.length > TOOL_KEEP_CHARS) {
    parts.push(
      `[Truncated to fit the context window: ${content.length - TOOL_KEEP_CHARS} more characters ` +
        'omitted. Call the tool again if you need the full output.]'
    );
  }
  if (hasImages) parts.push('[Screenshot omitted to fit the context window.]');

  return { ...message, content: parts.join('\n\n'), images: undefined };
}

export interface FitResult {
  messages: ChatMessage[];
  /** Whether anything was trimmed. */
  trimmed: boolean;
  droppedTurns: number;
  estimatedTokens: number;
  budget: number;
}

export function fitHistoryToBudget(
  history: ChatMessage[],
  opts: { contextTokens?: number; vision: boolean }
): FitResult {
  const budget = historyBudget(opts.contextTokens);
  const cost = (list: ChatMessage[]) =>
    list.reduce((sum, m) => sum + estimateTokens(m, opts.vision), 0);

  let messages = history;
  let total = cost(messages);
  const done = (trimmed: boolean, droppedTurns = 0): FitResult => ({
    messages,
    trimmed,
    droppedTurns,
    estimatedTokens: total,
    budget,
  });

  if (total <= budget) return done(false);

  // The current request starts at the last user message; everything from
  // there on is the agent loop in progress and is kept whole if at all possible.
  let currentStart = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === 'user') {
      currentStart = i;
      break;
    }
  }
  if (currentStart < 0) currentStart = 0;

  // 1. Shrink old tool results.
  messages = messages.map((m, i) =>
    i < currentStart && m.role === 'tool' ? truncateToolResult(m) : m
  );
  total = cost(messages);
  if (total <= budget) return done(true);

  // 2. Drop the oldest whole turns.
  const turnStarts: number[] = [];
  for (let i = 0; i < currentStart; i++) {
    if (i === 0 || messages[i].role === 'user') turnStarts.push(i);
  }
  let cut = 0;
  let droppedTurns = 0;
  for (let t = 0; t < turnStarts.length && total > budget; t++) {
    const nextStart = turnStarts[t + 1] ?? currentStart;
    total -= cost(messages.slice(turnStarts[t], nextStart));
    cut = nextStart;
    droppedTurns++;
  }
  if (cut > 0) {
    messages = messages.slice(cut);
    currentStart -= cut;
    // Tell the model why the conversation seems to start mid-way.
    const first = messages[0];
    if (first?.role === 'user') {
      messages = [
        {
          ...first,
          content:
            '[Earlier messages in this chat were omitted to fit the context window.]\n\n' +
            first.content,
        },
        ...messages.slice(1),
      ];
    }
    total = cost(messages);
  }
  if (total <= budget) return done(true, droppedTurns);

  // 3. Last resort: shrink tool results of the current request, oldest
  //    first, sparing the most recent one the model is working from.
  const toolIndexes = messages.flatMap((m, i) =>
    i >= currentStart && m.role === 'tool' ? [i] : []
  );
  messages = [...messages];
  for (const i of toolIndexes.slice(0, -1)) {
    if (total <= budget) break;
    messages[i] = truncateToolResult(messages[i]);
    total = cost(messages);
  }

  // Still over: send it anyway and let the provider decide; trimming the
  // user's own words or the latest tool result would change the task.
  return done(true, droppedTurns);
}
