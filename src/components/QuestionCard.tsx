// QuestionCard.tsx
//
// The agent's ask_user call where it happened in the chat (lib/questions.ts).
// While it waits, it lists the questions and points to the panel above the
// input, where they're answered one at a time (QuestionPanel). Answered, it
// records the decisions: question → answer.
import { createMemo, For, Show } from 'solid-js';
import type { ChatMessage, ChatToolCall } from '../types';
import { parseQuestions, pendingQuestions, type QuestionAnswer } from '../lib/questions';

export default function QuestionCard(props: {
  dialogId: string | null;
  toolCall: ChatToolCall;
  result?: ChatMessage;
  running?: boolean;
}) {
  const pending = () =>
    props.dialogId ? pendingQuestions(props.dialogId, props.toolCall.id) : undefined;
  const questions = createMemo(() => {
    try {
      return pending()?.questions ?? parseQuestions(JSON.parse(props.toolCall.arguments));
    } catch {
      return [];
    }
  });

  /** The stored outcome, for an answered card. */
  const answered = () => props.result?.meta?.questions;

  return (
    <div class="question-card" classList={{ waiting: !!pending(), done: !pending() }}>
      <div class="question-card-head">
        <svg
          width="14"
          height="14"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          aria-hidden="true"
        >
          <circle cx="8" cy="8" r="6.25" />
          <path d="M6.2 6.3a1.9 1.9 0 1 1 2.6 1.75c-.5.22-.8.6-.8 1.15v.3M8 11.4v.1" />
        </svg>
        <span class="question-card-title">
          {pending()
            ? questions().length > 1
              ? `The agent has ${questions().length} questions`
              : 'The agent has a question'
            : 'The agent asked'}
        </span>
      </div>

      {/* Waiting: answered in the panel above the input (QuestionPanel). */}
      <Show when={pending()}>
        <For each={questions()}>{(q) => <div class="question-waiting">{q.question}</div>}</For>
        <div class="question-reason">Answer below, above the message box.</div>
      </Show>

      {/* Answered: the decisions. */}
      <Show when={!pending() && props.result}>
        <Show
          when={answered()}
          fallback={<div class="question-reason">{props.result!.content.split('\n')[0]}</div>}
        >
          {(a) => (
            <Show
              when={'answers' in a().outcome}
              fallback={
                <div class="question-decision">
                  {'reply' in a().outcome ? (
                    <>
                      <span class="question-decision-note">Replied:</span>{' '}
                      <span class="question-decision-a">
                        {(a().outcome as { reply: string }).reply}
                      </span>
                    </>
                  ) : (
                    <span class="question-decision-note">Stopped instead of answering.</span>
                  )}
                </div>
              }
            >
              <For each={a().questions}>
                {(q, i) => {
                  const ans = () => (a().outcome as { answers: QuestionAnswer[] }).answers[i()];
                  return (
                    <div class="question-decision">
                      <span class="question-decision-q">{q.question}</span>
                      <span
                        class="question-decision-a"
                        classList={{ muted: ans()?.kind === 'agent' }}
                      >
                        {ans()?.kind === 'option'
                          ? (ans() as { label: string }).label
                          : ans()?.kind === 'custom'
                            ? `“${(ans() as { text: string }).text}”`
                            : 'Left to the agent'}
                      </span>
                    </div>
                  );
                }}
              </For>
            </Show>
          )}
        </Show>
      </Show>

      <Show when={!pending() && !props.result && !props.running}>
        <div class="question-reason">Not answered.</div>
      </Show>
    </div>
  );
}
