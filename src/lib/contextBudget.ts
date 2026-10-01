// The history sent to the model is fitted in Rust (chat_db/history.rs).
// Only the budget size is decided here, from the model's context window.

/** Used when neither the server nor the user gave a context window. */
export const DEFAULT_CONTEXT_TOKENS = 32_000;

/** Share of the window for history; the rest covers the system prompt, tool schemas and the reply. */
const HISTORY_SHARE = 0.7;
const MIN_HISTORY_TOKENS = 2_000;

export function historyBudget(contextTokens: number | undefined): number {
  const window = contextTokens && contextTokens > 0 ? contextTokens : DEFAULT_CONTEXT_TOKENS;
  return Math.max(MIN_HISTORY_TOKENS, Math.floor(window * HISTORY_SHARE));
}
