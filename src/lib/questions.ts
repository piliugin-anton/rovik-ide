// questions.ts
//
// The agent's questions to the user (the ask_user tool, Plan mode). Like an
// approval, a question suspends the run (in the backend: agent_run): the
// tool call waits until the user answers in the panel above the input
// (QuestionPanel), types a reply in the composer, or stops. The answer goes
// back to the run, and to the model as the tool's result.
import { invoke } from '@tauri-apps/api/core';
import { createStore, produce } from 'solid-js/store';

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

// Keyed by chat and call: two chats can wait on the same call id.
const [state, setState] = createStore<{ pending: Record<string, PendingQuestions> }>({
  pending: {},
});

const keyOf = (dialogId: string, toolCallId: string) => `${dialogId}\n${toolCallId}`;

/** A run waits for the user's answer to a call's questions. */
export function registerQuestions(request: PendingQuestions) {
  setState('pending', keyOf(request.dialogId, request.toolCallId), request);
}

/** Answered, or no longer waited for (the run stopped). */
export function clearQuestions(dialogId: string, toolCallId: string) {
  const key = keyOf(dialogId, toolCallId);
  if (!state.pending[key]) return;
  setState(
    'pending',
    produce((pending) => {
      delete pending[key];
    })
  );
}

export function pendingQuestions(
  dialogId: string,
  toolCallId: string
): PendingQuestions | undefined {
  return state.pending[keyOf(dialogId, toolCallId)];
}

/** The chats the agent's questions wait in. */
export function chatsWaitingOnQuestions(): Record<string, true> {
  const chats: Record<string, true> = {};
  for (const p of Object.values(state.pending)) chats[p.dialogId] = true;
  return chats;
}

/** The questions waiting in a chat, if any (the composer answers them). */
export function pendingQuestionsIn(dialogId: string): PendingQuestions | undefined {
  return Object.values(state.pending).find((p) => p.dialogId === dialogId);
}

export function answerQuestions(dialogId: string, toolCallId: string, outcome: QuestionsOutcome) {
  clearQuestions(dialogId, toolCallId);
  void invoke('agent_answer_questions', { dialogId, toolCallId, outcome }).catch((err) =>
    console.error('Answer not delivered:', err)
  );
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
