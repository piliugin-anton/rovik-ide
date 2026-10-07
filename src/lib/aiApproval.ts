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

/** One check score in a Jev reviewer's reason ("fits the task 91%"). */
export interface ReviewCheck {
  label: string;
  percent: number;
}

/**
 * The scores in a typed reviewer's reason (review.rs: decide), e.g.
 * "Checks passed: fits the task 91%, no secrets or exfiltration 96%.".
 * Free-form reasons from a text reviewer give undefined.
 */
export function parseReviewChecks(reason: string): ReviewCheck[] | undefined {
  const m = /^(?:Checks passed|Failed): (.+)\.$/.exec(reason.trim());
  if (!m) return undefined;
  const checks: ReviewCheck[] = [];
  for (const part of m[1].split(', ')) {
    const c = /^(.+) (\d{1,3})%$/.exec(part);
    if (!c) return undefined;
    checks.push({ label: c[1], percent: Number(c[2]) });
  }
  return checks;
}
