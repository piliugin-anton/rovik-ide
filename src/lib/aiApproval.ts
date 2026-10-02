// aiApproval.ts
//
// "By AI" approval: a separate model reviews each action that needs
// approval and decides approve/reject. The approval model, then each
// fallback in order, is tried until one gives a clear decision. If none
// does, the caller falls back to asking the user, so nothing ever runs
// unreviewed.
import type { ChatMessage, ModelProfile } from '../types';
import { completeOnce, LEAN_THEN_PLAIN, stripThinking } from './oneShot';

export interface AiReviewInput {
  /** The user's latest request in this chat, for intent. */
  userRequest: string;
  toolName: string;
  /** One-line summary, e.g. "Run `npm test`". */
  summary: string;
  argsJson: string;
}

export type AiReviewResult =
  | { decided: true; approved: boolean; reason: string; model: string }
  | { decided: false; errors: string[] };

const MAX_ARGS_CHARS = 6_000;
const MAX_REQUEST_CHARS = 2_000;

const SYSTEM_PROMPT = `You review actions an AI coding agent wants to take in a user's project, inside their IDE. Decide whether the action should run.

Approve when the action is a reasonable step toward what the user asked and its effects are contained to the project.
Reject when it is destructive beyond what the task needs (deleting or overwriting unrelated files, rewriting git history, force pushes), exfiltrates data or secrets, installs or runs untrusted code from the internet, touches files outside the project, changes system settings, or clearly doesn't serve the user's request.
When unsure about something irreversible, reject.

Reply with only a JSON object, no other text:
{"decision": "approve" | "reject", "reason": "<one short sentence>"}`;

function buildPrompt(input: AiReviewInput): ChatMessage[] {
  const args =
    input.argsJson.length > MAX_ARGS_CHARS
      ? `${input.argsJson.slice(0, MAX_ARGS_CHARS)}\n… (${input.argsJson.length - MAX_ARGS_CHARS} more characters)`
      : input.argsJson;
  const request = input.userRequest.trim().slice(0, MAX_REQUEST_CHARS) || '(not available)';

  return [
    {
      id: 'approval-review',
      role: 'user',
      content: [
        `User's request:\n${request}`,
        '',
        `Action: ${input.summary}`,
        `Tool: ${input.toolName}`,
        `Arguments:\n${args}`,
        '',
        'Decision as JSON:',
      ].join('\n'),
    },
  ];
}

/*
 * ---------------------------------------------------------------------------
 * Typed input (Typesafe Jev)
 * ---------------------------------------------------------------------------
 *
 * With "Use typed input", the review goes out as one JSON object instead of
 * the text prompt above. The whole format lives here: `TypedReviewInput`
 * and `buildTypedInput` are the only things to change if the model expects
 * different field names.
 */

export interface TypedReviewInput {
  type: 'tool_approval_request';
  version: 1;
  /** What the user asked for in the chat, for intent. */
  user_request: string;
  action: {
    /** "command" for run_command, "tool" for every other tool. */
    kind: 'command' | 'tool';
    tool: string;
    /** run_command only: the command line. */
    command?: string;
    /** One-line summary, e.g. "Write src/App.tsx (120 lines)". */
    summary: string;
    /** The tool arguments as an object; long strings are cut. */
    arguments: Record<string, unknown>;
  };
  /** The shape the reply must have. */
  response_schema: {
    decision: 'approve | reject';
    reason: 'string';
  };
}

const TYPED_SYSTEM_PROMPT = `Input: one JSON object of type "tool_approval_request" describing an action an AI coding agent wants to take in the user's project. Decide whether it should run, following the review policy below.

${SYSTEM_PROMPT.split('\n\nReply with')[0]}

Output: only a JSON object matching "response_schema": {"decision": "approve" | "reject", "reason": "<one short sentence>"}`;

/** Long string values (file contents) are cut so the request stays small. */
function trimStrings(value: unknown): unknown {
  if (typeof value === 'string') {
    return value.length > MAX_ARGS_CHARS
      ? `${value.slice(0, MAX_ARGS_CHARS)}… (${value.length - MAX_ARGS_CHARS} more characters)`
      : value;
  }
  if (Array.isArray(value)) return value.map(trimStrings);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, trimStrings(v)]));
  }
  return value;
}

export function buildTypedInput(input: AiReviewInput): TypedReviewInput {
  let args: Record<string, unknown>;
  try {
    const parsed = input.argsJson.trim() ? JSON.parse(input.argsJson) : {};
    args =
      parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : { value: parsed };
  } catch {
    // Unparseable arguments still reach the reviewer, as text.
    args = { raw: input.argsJson };
  }
  const isCommand = input.toolName === 'run_command';

  return {
    type: 'tool_approval_request',
    version: 1,
    user_request: input.userRequest.trim().slice(0, MAX_REQUEST_CHARS),
    action: {
      kind: isCommand ? 'command' : 'tool',
      tool: input.toolName,
      ...(isCommand && typeof args.command === 'string' ? { command: args.command } : {}),
      summary: input.summary,
      arguments: trimStrings(args) as Record<string, unknown>,
    },
    response_schema: { decision: 'approve | reject', reason: 'string' },
  };
}

function buildTypedPrompt(input: AiReviewInput): ChatMessage[] {
  return [
    {
      id: 'approval-review',
      role: 'user',
      content: JSON.stringify(buildTypedInput(input)),
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

export async function reviewWithAi(
  models: ModelProfile[],
  input: AiReviewInput,
  opts: { typedInput?: boolean } = {}
): Promise<AiReviewResult> {
  const errors: string[] = [];
  const messages = opts.typedInput ? buildTypedPrompt(input) : buildPrompt(input);
  const systemPrompt = opts.typedInput ? TYPED_SYSTEM_PROMPT : SYSTEM_PROMPT;

  for (const profile of models) {
    for (const options of LEAN_THEN_PLAIN(300)) {
      const result = await completeOnce({
        profile,
        systemPrompt,
        messages,
        options,
      });
      if (!result.ok) {
        errors.push(`${profile.label}: ${result.error}`);
        continue;
      }
      const decision = parseDecision(result.text);
      if (decision) return { decided: true, ...decision, model: profile.label };
      errors.push(`${profile.label}: unclear reply`);
    }
  }

  return { decided: false, errors };
}
