// toolResults.ts
//
// Which result message answers which tool call. A model may reuse a call id
// in a later reply ("call_0" every time), so a result belongs to the latest
// call with its id before it, and the index is keyed by the call itself
// (P5.8).
import type { ChatMessage, ChatToolCall } from '../types';

export interface ToolIndex {
  /** Each call's result message. */
  results: Map<ChatToolCall, ChatMessage>;
  /** The newest call with each id: live progress (keyed by id) is its. */
  latest: Map<string, ChatToolCall>;
}

export function indexToolResults(messages: readonly ChatMessage[]): ToolIndex {
  const results = new Map<ChatToolCall, ChatMessage>();
  const latest = new Map<string, ChatToolCall>();
  for (const m of messages) {
    if (m.role === 'tool') {
      const tc = m.tool_call_id ? latest.get(m.tool_call_id) : undefined;
      if (tc) results.set(tc, m);
    } else {
      for (const tc of m.toolCalls ?? []) latest.set(tc.id, tc);
    }
  }
  return { results, latest };
}
