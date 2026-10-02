// aiApproval.ts
//
// "By AI reviewer" approval. Two ways to ask, depending on the model:
//
//   - Chat models get a text prompt and answer {"decision", "reason"}.
//   - Jev (TypeSafe's System One model), with "Use typed input", gets the
//     action as JSON `state` plus named yes/no (Noul) questions on the
//     System One API, and answers a probability per question. Code turns
//     those into a decision with fixed thresholds: approve when every check
//     is clearly yes, reject when any is clearly no, and ask the user in
//     between. Jev returns no text, so the reason shown is the checks.
//
// The reviewer model, then each fallback, is tried until one answers. If
// none does, or Jev is unsure, the caller asks the user: nothing runs
// unreviewed.
//
// Typed requests go to the reviewer's provider like chat requests do: its
// base URL + /systemone, with its API key and model id.
//
// System One reference: https://docs.typesafe.ai and OpenRouter's Jev
// recipes for gating tool calls and permission prompts.
import { invoke } from '@tauri-apps/api/core';
import type { ChatMessage, ModelProfile } from '../types';
import { completeOnce, LEAN_THEN_PLAIN, stripThinking } from './oneShot';

/** Which approval group the action belongs to. */
export type ReviewGroup = 'commands' | 'edits' | 'tools';

export interface AiReviewInput {
  /** Identifies the review in the terminal log (the tool call id). */
  reviewId: string;
  group: ReviewGroup;
  /** The user's latest request in this chat, for intent. */
  userRequest: string;
  /** Absolute project folder. */
  project: string;
  toolName: string;
  /** One-line summary, e.g. "Run `npm test`". */
  summary: string;
  argsJson: string;
  /** File edits: the change as a compact diff (empty for a new file). */
  editDiff?: string;
  /** File edits: the path as the agent gave it. */
  editPath?: string;
  isNewFile?: boolean;
}

export type AiReviewResult =
  | { decided: true; approved: boolean; reason: string; model: string }
  /** The reviewer answered but wasn't sure either way: ask the user. */
  | { decided: false; unsure: true; reason: string; model: string }
  | { decided: false; unsure?: false; errors: string[] };

const MAX_ARGS_CHARS = 6_000;
const MAX_REQUEST_CHARS = 2_000;
/** Keeps a typed request well inside Jev's 32k-token context on OpenRouter. */
const MAX_DIFF_CHARS = 24_000;

/*
 * ---------------------------------------------------------------------------
 * Text prompt (chat models)
 * ---------------------------------------------------------------------------
 */

const SYSTEM_PROMPT = `You review actions an AI coding agent wants to take in a user's project, inside their IDE. Decide whether the action should run.

Approve when the action is a reasonable step toward what the user asked and its effects are contained to the project.
Reject when it is destructive beyond what the task needs (deleting or overwriting unrelated files, rewriting git history, force pushes), exfiltrates data or secrets, installs or runs untrusted code from the internet, touches files outside the project, changes system settings, or clearly doesn't serve the user's request.
When unsure about something irreversible, reject.

Reply with only a JSON object, no other text:
{"decision": "approve" | "reject", "reason": "<one short sentence>"}`;

const cut = (text: string, max: number) =>
  text.length > max ? `${text.slice(0, max)}\n… (${text.length - max} more characters)` : text;

function buildPrompt(input: AiReviewInput): ChatMessage[] {
  const request = input.userRequest.trim().slice(0, MAX_REQUEST_CHARS) || '(not available)';
  // For edits the diff says more than the whole new file.
  const details =
    input.group === 'edits' && input.editDiff
      ? `Diff:\n${cut(input.editDiff, MAX_ARGS_CHARS)}`
      : `Arguments:\n${cut(input.argsJson, MAX_ARGS_CHARS)}`;

  return [
    {
      id: 'approval-review',
      role: 'user',
      content: [
        `User's request:\n${request}`,
        '',
        `Project: ${input.project}`,
        `Action: ${input.summary}`,
        `Tool: ${input.toolName}`,
        details,
        '',
        'Decision as JSON:',
      ].join('\n'),
    },
  ];
}

/** Pulls {"decision", "reason"} out of a reply, tolerating fences and extra text. */
export function parseDecision(raw: string): { approved: boolean; reason: string } | null {
  const text = stripThinking(raw);
  const start = text.indexOf('{');
  const end = text.lastIndexOf('}');
  if (start >= 0 && end > start) {
    try {
      const parsed = JSON.parse(text.slice(start, end + 1));
      const decision = String(parsed.decision ?? '').toLowerCase();
      if (decision === 'approve' || decision === 'reject') {
        return {
          approved: decision === 'approve',
          reason: String(parsed.reason ?? '').trim() || 'No reason given.',
        };
      }
    } catch {
      // Fall through to the plain-word check.
    }
  }
  // Some models answer with just the word.
  const word = text.match(/^\W*(approve|approved|reject|rejected)\b/i)?.[1]?.toLowerCase();
  if (word) return { approved: word.startsWith('approve'), reason: 'No reason given.' };
  return null;
}

async function reviewWithText(
  profile: ModelProfile,
  input: AiReviewInput,
  errors: string[]
): Promise<AiReviewResult | null> {
  const messages = buildPrompt(input);
  for (const options of LEAN_THEN_PLAIN(300)) {
    const result = await completeOnce({ profile, systemPrompt: SYSTEM_PROMPT, messages, options });
    if (!result.ok) {
      errors.push(`${profile.label}: ${result.error}`);
      continue;
    }
    const decision = parseDecision(result.text);
    if (decision) return { decided: true, ...decision, model: profile.label };
    errors.push(`${profile.label}: unclear reply`);
  }
  return null;
}

/*
 * ---------------------------------------------------------------------------
 * Typed input (Jev / System One)
 * ---------------------------------------------------------------------------
 */

/** Every check at or above this: approved. */
export const APPROVE_AT = 0.9;
/** Any check at or below this: rejected. In between: the user decides. */
export const REJECT_AT = 0.1;

interface NoulQuestion {
  type: 'noul';
  instructions: string;
  /** Shown in the decision's reason. */
  label: string;
}

/*
 * One proposition per question, phrased so yes means safe to run, each
 * naming the `state` fields it depends on. The task question is skipped
 * when there's no task text (Jev would read a missing task as a mismatch).
 */
const QUESTIONS: Record<ReviewGroup, Record<string, NoulQuestion>> = {
  commands: {
    reversible: {
      type: 'noul',
      label: 'reversible',
      instructions:
        'Every command in `commands` only reads or changes files inside `project` and can be undone with git or by rerunning it. It does not push, publish, deploy, delete files outside the project, change system settings, or send data to a network service.',
    },
    serves_task: {
      type: 'noul',
      label: 'fits the task',
      instructions: 'Running `commands` is a reasonable next step toward `task`.',
    },
  },
  edits: {
    serves_task: {
      type: 'noul',
      label: 'fits the task',
      instructions:
        'The change in `edit.diff` to the file `edit.path` is a reasonable step toward `task`.',
    },
    scoped: {
      type: 'noul',
      label: 'stays in scope',
      instructions:
        'The change in `edit.diff` only touches code that `task` is about. It does not delete or rewrite unrelated code, tests, or configuration.',
    },
    safe: {
      type: 'noul',
      label: 'no secrets or exfiltration',
      instructions:
        'The change in `edit.diff` does not add credentials or secrets, does not weaken security checks, and does not add network calls that send data somewhere `task` does not ask for.',
    },
  },
  tools: {
    serves_task: {
      type: 'noul',
      label: 'fits the task',
      instructions:
        'Calling the tool `tool.name` with `tool.arguments` is a reasonable next step toward `task`.',
    },
    safe: {
      type: 'noul',
      label: 'no secrets or exfiltration',
      instructions:
        '`tool.arguments` does not target credential or secret files (such as .env files, keys, or tokens) and does not send project code, secrets, or personal data to an outside service.',
    },
  },
};

function typedState(input: AiReviewInput): Record<string, unknown> {
  const task = input.userRequest.trim().slice(0, MAX_REQUEST_CHARS);
  const base: Record<string, unknown> = { project: input.project, ...(task ? { task } : {}) };
  const args = parseArgs(input.argsJson);

  switch (input.group) {
    case 'commands':
      return { ...base, commands: [typeof args.command === 'string' ? args.command : ''] };
    case 'edits':
      return {
        ...base,
        edit: {
          path: input.editPath ?? args.path ?? '',
          new_file: !!input.isNewFile,
          // A new file has no diff: its content is the change.
          diff: input.isNewFile
            ? cut(typeof args.content === 'string' ? args.content : '', MAX_DIFF_CHARS)
            : cut(input.editDiff ?? '', MAX_DIFF_CHARS),
        },
      };
    case 'tools':
      return { ...base, tool: { name: input.toolName, arguments: args } };
  }
}

function parseArgs(argsJson: string): Record<string, unknown> {
  try {
    const parsed = argsJson.trim() ? JSON.parse(argsJson) : {};
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return { raw: argsJson };
  }
}

/** A model that only speaks System One (Jev), so a text prompt can't work. */
export function isJevModel(modelId: string): boolean {
  // Jev Router is a chat model that routes to other models.
  return /(^|[/~:-])jev([-_.]|$)/i.test(modelId) && !/jev-router/i.test(modelId);
}

interface SystemOneResponse {
  answers?: Record<string, { type?: string; noul?: unknown } | undefined>;
}

async function reviewWithTyped(
  profile: ModelProfile,
  input: AiReviewInput,
  errors: string[]
): Promise<AiReviewResult | null> {
  const state = typedState(input);
  const asked = Object.entries(QUESTIONS[input.group]).filter(
    ([key]) => key !== 'serves_task' || 'task' in state
  );
  const questions = Object.fromEntries(
    asked.map(([key, q]) => [key, { type: q.type, instructions: q.instructions }])
  );

  let response: SystemOneResponse;
  try {
    response = await invoke<SystemOneResponse>('system_one_request', {
      // The provider's base URL and the model as configured; the backend
      // adds /systemone the way chat requests add /chat/completions.
      baseUrl: profile.baseUrl,
      apiKey: profile.apiKey,
      model: profile.model,
      body: { state, questions },
      timeoutMs: 15_000,
    });
  } catch (err) {
    errors.push(`${profile.label}: ${String(err)}`);
    return null;
  }

  // Every asked question needs a probability, or the answer isn't usable.
  const scores: [NoulQuestion, number][] = [];
  for (const [key, q] of asked) {
    const p = response.answers?.[key]?.noul;
    if (typeof p !== 'number' || !(p >= 0 && p <= 1)) {
      errors.push(`${profile.label}: no usable answer for "${key}"`);
      return null;
    }
    scores.push([q, p]);
  }

  const shown = (list: [NoulQuestion, number][]) =>
    list.map(([q, p]) => `${q.label} ${Math.round(p * 100)}%`).join(', ');

  const failed = scores.filter(([, p]) => p <= REJECT_AT);
  if (failed.length > 0) {
    return {
      decided: true,
      approved: false,
      reason: `Failed: ${shown(failed)}.`,
      model: profile.label,
    };
  }
  if (scores.every(([, p]) => p >= APPROVE_AT)) {
    return {
      decided: true,
      approved: true,
      reason: `Checks passed: ${shown(scores)}.`,
      model: profile.label,
    };
  }
  return {
    decided: false,
    unsure: true,
    reason: `${profile.label} wasn’t sure (${shown(scores)}).`,
    model: profile.label,
  };
}

/*
 * ---------------------------------------------------------------------------
 * Entry point
 * ---------------------------------------------------------------------------
 */

export async function reviewWithAi(
  models: ModelProfile[],
  input: AiReviewInput,
  /** Whether to use typed input for a model. */
  typedFor: (profile: ModelProfile) => boolean
): Promise<AiReviewResult> {
  const started = Date.now();
  logReview('log_ai_review_started', {
    reviewId: input.reviewId,
    tool: input.toolName,
    summary: input.summary,
    group: input.group,
    models: models.map((m) => `${m.label}${typedFor(m) ? ' [typed]' : ''}`),
  });

  const errors: string[] = [];
  let result: AiReviewResult = { decided: false, errors };
  for (const profile of models) {
    const answer = typedFor(profile)
      ? await reviewWithTyped(profile, input, errors)
      : await reviewWithText(profile, input, errors);
    // An unsure answer is an answer: it goes to the user, not to a fallback.
    if (answer) {
      result = answer;
      break;
    }
  }

  logReview('log_ai_review_finished', {
    reviewId: input.reviewId,
    outcome: result.decided
      ? result.approved
        ? 'approved'
        : 'rejected'
      : result.unsure
        ? 'unsure (asking the user)'
        : 'failed (asking the user)',
    model: 'model' in result ? result.model : null,
    reason: 'reason' in result ? result.reason : result.errors.join('; ') || 'no model answered',
    durationMs: Date.now() - started,
  });
  return result;
}

/** Terminal log line via the backend; never gets in the review's way. */
function logReview(command: string, args: Record<string, unknown>) {
  void invoke(command, args).catch((err) => console.warn('AI review log failed:', err));
}

/** The change as a compact diff for the reviewer: changed lines only, with positions. */
export function compactDiff(
  hunks: { origStart: number; deleted: string[]; added: string[] }[]
): string {
  return hunks
    .map((h) =>
      [
        `@@ line ${h.origStart} @@`,
        ...h.deleted.map((l) => `-${l}`),
        ...h.added.map((l) => `+${l}`),
      ].join('\n')
    )
    .join('\n');
}
