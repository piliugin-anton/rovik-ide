// reasoning.ts
//
// Reasoning effort, sent only in the forms each model's provider accepts.
// Providers take it in different ways (src-tauri/src/agent.rs,
// REASONING_FIELDS), and strict ones reject what they don't know. So:
//
//   - "Auto" (the default) sends every form not known to be rejected; the
//     backend drops any the provider refuses and retries, then reports what
//     was accepted and rejected (the ReasoningSupport event), which is
//     stored with the model in the settings and used from then on.
//   - A 400 that names no field, fixed by sending none of them, shows that
//     one of the forms broke the request, not which: they become suspects,
//     tried one at a time (with the forms known to work) on the next
//     requests, so a working form isn't lost because of a sibling.
//   - A model that sends its reasoning in the stream is known to reason.
//   - A model whose provider rejected every form doesn't take an effort:
//     the selector hides for it.
//   - Rejected forms are tried again after 30 days, in case support was added.
//   - In the model's settings, reasoning can be turned off ("reasoning"
//     toggle, beside "vision" and "tools"); turning it on detects afresh.
//
// The effort itself is picked in the chat (EffortSelector), per model.
import type { ProviderModel } from '../types';

export type ReasoningField = 'reasoning_effort' | 'reasoning' | 'chat_template_kwargs' | 'thinking';
export const REASONING_FIELDS: ReasoningField[] = [
  'reasoning_effort',
  'reasoning',
  'chat_template_kwargs',
  'thinking',
];

export interface ReasoningLearned {
  accepted: ReasoningField[];
  /** With when it was rejected: tried again after RETRY_AFTER_MS. */
  rejected: { field: ReasoningField; at: number }[];
  /** The model sent reasoning in its reply. */
  reasons?: boolean;
  /** One of these broke a request (a 400 naming none): tried one at a time. */
  suspect?: ReasoningField[];
  at: number;
}

export interface ReasoningSetting {
  /** auto: detect; off: never send; a field: send just that one. */
  choice: 'auto' | 'off' | ReasoningField;
  learned?: ReasoningLearned;
}

export type Effort = 'default' | 'none' | 'low' | 'medium' | 'high';

export const EFFORTS: { id: Effort; label: string; hint: string }[] = [
  { id: 'default', label: 'Default effort', hint: "The provider's default: nothing is sent" },
  {
    id: 'none',
    label: 'No thinking',
    hint: 'Answer without reasoning first, where the model allows it',
  },
  { id: 'low', label: 'Low effort', hint: 'Brief reasoning: faster and cheaper' },
  { id: 'medium', label: 'Medium effort', hint: 'Balanced reasoning' },
  { id: 'high', label: 'High effort', hint: 'Thorough reasoning: slower, for harder tasks' },
];

const RETRY_AFTER_MS = 30 * 24 * 60 * 60 * 1000;

export function settingOf(model: Pick<ProviderModel, 'reasoning'> | undefined): ReasoningSetting {
  return model?.reasoning ?? { choice: 'auto' };
}

/** The forms to send with an effort, for this setting (empty: send nothing). */
export function formatsToSend(setting: ReasoningSetting, now = Date.now()): ReasoningField[] {
  if (setting.choice === 'off') return [];
  if (setting.choice !== 'auto') return [setting.choice];
  const rejectedRecently = new Set(
    (setting.learned?.rejected ?? []).filter((r) => now - r.at < RETRY_AFTER_MS).map((r) => r.field)
  );
  // Suspects are tried one at a time, beside the forms known to work: if it
  // fails, the suspect is the only unknown, so it's the one to blame.
  const suspects = (setting.learned?.suspect ?? []).filter((f) => !rejectedRecently.has(f));
  if (suspects.length > 0) {
    const working = (setting.learned?.accepted ?? []).filter((f) => !rejectedRecently.has(f));
    return REASONING_FIELDS.filter((f) => working.includes(f) || f === suspects[0]);
  }
  return REASONING_FIELDS.filter((f) => !rejectedRecently.has(f));
}

/** Whether the model takes a reasoning effort: known yes, known no, or not yet known. */
export function reasoningStatus(
  setting: ReasoningSetting,
  now = Date.now()
): 'yes' | 'no' | 'unknown' {
  if (setting.choice === 'off') return 'no';
  if (setting.choice !== 'auto') return 'yes';
  const l = setting.learned;
  if (!l) return 'unknown';
  if (l.reasons || l.accepted.length > 0) return 'yes';
  return formatsToSend(setting, now).length === 0 ? 'no' : 'unknown';
}

/** A request's outcome merged into what's known (accepting a form clears its rejection, and back). */
export function mergeLearned(
  prev: ReasoningLearned | undefined,
  accepted: string[],
  rejected: string[],
  unresolved: string[] = [],
  now = Date.now()
): ReasoningLearned {
  const valid = (f: string): f is ReasoningField => (REASONING_FIELDS as string[]).includes(f);
  const acc = new Set(prev?.accepted ?? []);
  const rej = new Map((prev?.rejected ?? []).map((r) => [r.field, r.at] as const));
  const suspect = new Set(prev?.suspect ?? []);
  const reject = (f: ReasoningField) => {
    rej.set(f, now);
    acc.delete(f);
    suspect.delete(f);
  };
  for (const f of accepted.filter(valid)) {
    acc.add(f);
    rej.delete(f);
    suspect.delete(f);
  }
  for (const f of rejected.filter(valid)) reject(f);
  // A bare 400 fixed by stripping these: forms already known to work aren't
  // to blame. One unknown left: it's the culprit. Several: suspects.
  const unknown = unresolved.filter(valid).filter((f) => !acc.has(f));
  if (unknown.length === 1) reject(unknown[0]);
  else for (const f of unknown) if (!rej.has(f)) suspect.add(f);
  const next: ReasoningLearned = {
    accepted: REASONING_FIELDS.filter((f) => acc.has(f)),
    rejected: [...rej].map(([field, at]) => ({ field, at })),
    reasons: prev?.reasons,
    ...(suspect.size ? { suspect: REASONING_FIELDS.filter((f) => suspect.has(f)) } : {}),
    at: now,
  };
  // Nothing new (a field known to work worked again, as on most requests):
  // the same object, so the settings aren't saved again after every reply.
  return prev && sameLearned(prev, next) ? prev : next;
}

/** The same knowledge, whenever it was learned. */
function sameLearned(a: ReasoningLearned, b: ReasoningLearned): boolean {
  const same = <T>(x: T[] = [], y: T[] = [], eq = (p: T, q: T) => p === q) =>
    x.length === y.length && x.every((v, i) => eq(v, y[i]));
  return (
    same(a.accepted, b.accepted) &&
    same(a.rejected, b.rejected, (p, q) => p.field === q.field && p.at === q.at) &&
    same(a.suspect, b.suspect) &&
    !!a.reasons === !!b.reasons
  );
}

/** For the settings tooltip. */
export function describeReasoning(setting: ReasoningSetting): string {
  if (setting.choice === 'off') return 'Reasoning off: no effort is sent to this model.';
  if (setting.choice !== 'auto') return `The effort is sent as ${setting.choice} only.`;
  const l = setting.learned;
  if (!l)
    return 'Detected automatically: what the provider accepts is learned on the first request with an effort.';
  const parts: string[] = [];
  if (l.accepted.length) parts.push(`Accepted: ${l.accepted.join(', ')}.`);
  if (l.rejected.length) parts.push(`Rejected: ${l.rejected.map((r) => r.field).join(', ')}.`);
  if (l.suspect?.length) parts.push(`Being checked one at a time: ${l.suspect.join(', ')}.`);
  if (l.reasons) parts.push('The model sends its reasoning.');
  if (!parts.length) parts.push('Nothing learned yet.');
  return `Detected: ${parts.join(' ')}`;
}

/* ---------------------------------------------------------------------------
 * The effort chosen in the chat, per model (this machine's choice).
 * ------------------------------------------------------------------------- */

const EFFORT_KEY = 'rovik.reasoningEffort';

export function loadEfforts(): Record<string, Effort> {
  try {
    return JSON.parse(localStorage.getItem(EFFORT_KEY) ?? '{}') ?? {};
  } catch {
    return {};
  }
}

export function saveEfforts(efforts: Record<string, Effort>) {
  try {
    localStorage.setItem(EFFORT_KEY, JSON.stringify(efforts));
  } catch {
    // not remembered
  }
}
