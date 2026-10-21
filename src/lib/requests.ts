// requests.ts
//
// Which of a chat's messages start a request (docs/design/steering-plan.md,
// 3.6): the user's own, but not the IDE's (`meta.harness`), nor one the user
// sent the run while it worked (`meta.steer`), which is part of the request
// it was sent into. The backend's rule is the same (history.rs
// `starts_request`).
import type { ChatMessage } from '../types';

/** A user message that starts a request. */
export function isRequestMessage(m: Pick<ChatMessage, 'role' | 'meta'>): boolean {
  return m.role === 'user' && !m.meta?.harness && !m.meta?.steer;
}

/** A message the user sent the run while it worked (steering). */
export function isSteerMessage(m: Pick<ChatMessage, 'role' | 'meta'>): boolean {
  return m.role === 'user' && !!m.meta?.steer;
}

/**
 * The steering messages of the chat's current request: those after its
 * latest request message, oldest first. A failed one (never sent to the
 * model) doesn't count.
 */
export function steersOfRequest(messages: ChatMessage[]): ChatMessage[] {
  const steers: ChatMessage[] = [];
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (isRequestMessage(m)) break;
    if (isSteerMessage(m) && !m.failed) steers.push(m);
  }
  return steers.reverse();
}
