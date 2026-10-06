// aiApproval.ts
//
// "By AI reviewer" approval runs in the backend (src-tauri/src/agent_run/
// review.rs). What the settings show about it lives here.

/** Every check at or above this: approved (review.rs: APPROVE_AT). */
export const APPROVE_AT = 0.9;
/** Any check at or below this: rejected; in between, the user decides (REJECT_AT). */
export const REJECT_AT = 0.1;

/** A model that only speaks System One (Jev), so a text prompt can't work. */
export function isJevModel(modelId: string): boolean {
  // Jev Router is a chat model that routes to other models.
  return /(^|[/~:-])jev([-_.]|$)/i.test(modelId) && !/jev-router/i.test(modelId);
}
