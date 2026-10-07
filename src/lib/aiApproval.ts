// aiApproval.ts
//
// "By AI reviewer" approval runs in the backend (src-tauri/src/agent_run/
// review.rs). What the settings and tool calls show about it lives here.
import type { ReviewCheck, ToolApproval } from '../types';

/** Every check at or above this: approved (review.rs: APPROVE_AT). */
export const APPROVE_AT = 0.9;
/** Any check at or below this: rejected; in between, the user decides (REJECT_AT). */
export const REJECT_AT = 0.1;

/** A model that only speaks System One (Jev), so a text prompt can't work. */
export function isJevModel(modelId: string): boolean {
  // Jev Router is a chat model that routes to other models.
  return /(^|[/~:-])jev([-_.]|$)/i.test(modelId) && !/jev-router/i.test(modelId);
}

/**
 * The scores in a typed reviewer's reason (review.rs: decide), e.g.
 * "Checks passed: fits the task 91%, no secrets or exfiltration 96%.".
 * For approvals stored before the scores were; free-form reasons from a
 * text reviewer give undefined.
 */
export function parseReviewChecks(reason: string): ReviewCheck[] | undefined {
  const m = /^(?:Checks passed|Failed): (.+)\.$/.exec(reason.trim());
  if (!m) return undefined;
  const checks: ReviewCheck[] = [];
  for (const part of m[1].split(', ')) {
    const c = /^(.+) (\d{1,3})%$/.exec(part);
    if (!c) return undefined;
    checks.push({ label: c[1], score: Number(c[2]) / 100 });
  }
  return checks;
}

/** The checks behind an AI approval: stored scores, else read from the reason. */
export function reviewChecks(approval: ToolApproval): ReviewCheck[] | undefined {
  if (approval.checks?.length) return approval.checks;
  return approval.reason ? parseReviewChecks(approval.reason) : undefined;
}
