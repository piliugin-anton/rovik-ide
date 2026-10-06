// The history sent to the model is fitted in Rust (chat_db/history.rs), to
// what the context window leaves after the system prompt, tool schemas and
// the room kept for the reply (agent.rs, reply_reserve). Only the window is
// decided here.

/** Used when neither the server nor the user gave a context window (as DEFAULT_CONTEXT_TOKENS in agent.rs). */
export const DEFAULT_CONTEXT_TOKENS = 32_000;

/** The model's context window, if it's known. */
export function knownContextTokens(contextTokens: number | undefined): number | undefined {
  return contextTokens && contextTokens > 0 ? contextTokens : undefined;
}
