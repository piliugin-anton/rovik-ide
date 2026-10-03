// questions.ts
//
// The agent's questions to the user (the ask_user tool, Plan mode). Like an
// approval, a question suspends the run: the tool call waits here until the
// user answers in its card in the chat (QuestionCard), types a reply in the
// composer, or stops. The answer goes back to the model as the tool's result.
import { createStore } from 'solid-js/store';

export interface QuestionOption {
  label: string;
  description?: string;
  /** The agent's suggestion. */
  recommended?: boolean;
}

export interface Question {
  question: string;
  /** Why it matters, briefly. */
  reason?: string;
  options: QuestionOption[];
  /** "Other" with the user's own words. */
  allowCustom: boolean;
}

/** One question's answer. */
export type QuestionAnswer =
  | { kind: 'option'; label: string }
  | { kind: 'custom'; text: string }
  /** "You decide". */
  | { kind: 'agent' };

export type QuestionsOutcome =
  | { answers: QuestionAnswer[] }
  /** Typed in the composer instead of answering in the card. */
  | { reply: string }
  | { stopped: true };

export interface PendingQuestions {
  toolCallId: string;
  dialogId: string;
  questions: Question[];
}

/** At most this many questions per call, options per question. */
const MAX_QUESTIONS = 3;
const MAX_OPTIONS = 6;

const [state, setState] = createStore<{ pending: Record<string, PendingQuestions> }>({
  pending: {},
});
const resolvers = new Map<string, (outcome: QuestionsOutcome) => void>();

/** Waits for the user's answer to a call's questions. */
export function askUser(request: PendingQuestions): Promise<QuestionsOutcome> {
  return new Promise((resolve) => {
    resolvers.set(request.toolCallId, resolve);
    setState('pending', request.toolCallId, request);
  });
}

export function pendingQuestions(toolCallId: string): PendingQuestions | undefined {
  return state.pending[toolCallId];
}

/** The questions waiting in a chat, if any (the composer answers them). */
export function pendingQuestionsIn(dialogId: string): PendingQuestions | undefined {
  return Object.values(state.pending).find((p) => p.dialogId === dialogId);
}

export function answerQuestions(toolCallId: string, outcome: QuestionsOutcome) {
  const resolve = resolvers.get(toolCallId);
  resolvers.delete(toolCallId);
  setState('pending', toolCallId, undefined!);
  resolve?.(outcome);
}

/** Stop pressed: every waiting question ends the run. */
export function stopQuestions(dialogId?: string) {
  for (const p of Object.values(state.pending)) {
    if (!dialogId || p.dialogId === dialogId) answerQuestions(p.toolCallId, { stopped: true });
  }
}

/**
 * The questions from the tool's arguments, tolerating the shapes models
 * produce: { questions: [...] }, or one question at the top level; options
 * as objects or plain strings.
 */
export function parseQuestions(args: Record<string, unknown>): Question[] {
  const raw = Array.isArray(args.questions) ? args.questions : [args];
  const out: Question[] = [];
  for (const q of raw.slice(0, MAX_QUESTIONS)) {
    if (!q || typeof q !== 'object') continue;
    const r = q as Record<string, unknown>;
    const question = typeof r.question === 'string' ? r.question.trim() : '';
    if (!question) continue;
    const options = (Array.isArray(r.options) ? r.options : [])
      .map((o): QuestionOption | null => {
        if (typeof o === 'string') return o.trim() ? { label: o.trim() } : null;
        if (!o || typeof o !== 'object') return null;
        const opt = o as Record<string, unknown>;
        const label = typeof opt.label === 'string' ? opt.label.trim() : '';
        if (!label) return null;
        return {
          label,
          description: typeof opt.description === 'string' ? opt.description : undefined,
          recommended: opt.recommended === true,
        };
      })
      .filter((o): o is QuestionOption => o !== null)
      .slice(0, MAX_OPTIONS);
    out.push({
      question,
      reason: typeof r.reason === 'string' && r.reason.trim() ? r.reason.trim() : undefined,
      options,
      // Without options the only way to answer is in words.
      allowCustom: r.allow_custom !== false || options.length === 0,
    });
  }
  return out;
}

/** What the model reads as the tool's result. */
export function describeOutcome(questions: Question[], outcome: QuestionsOutcome): string {
  if ('stopped' in outcome) return 'Error: the user stopped the task instead of answering.';
  if ('reply' in outcome) {
    return (
      'The user replied in their own words instead of picking options:\n\n' +
      `${outcome.reply}\n\n` +
      'Treat this as their answer to your questions. For anything it leaves open, decide yourself and say so in the plan.'
    );
  }
  const lines = ['The user answered:'];
  questions.forEach((q, i) => {
    const a = outcome.answers[i] ?? { kind: 'agent' };
    const answer =
      a.kind === 'option'
        ? a.label
        : a.kind === 'custom'
          ? `"${a.text}" (in their own words)`
          : 'left it to you: decide, and say what you chose and why in the plan';
    lines.push(`${i + 1}. ${q.question}\n   → ${answer}`);
  });
  lines.push('', 'Treat these answers as requirements. Continue planning.');
  return lines.join('\n');
}
