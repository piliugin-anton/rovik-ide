// QuestionPanel.tsx
//
// The agent's waiting questions (lib/questions.ts), answered one at a time
// in a panel docked at the bottom of the message list, above the input:
//
//   Question 1 of 3 → 2 of 3 → 3 of 3 → Your answers → Send answers
//
// Choosing an option (or "You decide", or Enter in "Other…") moves to the
// next question; Back returns with the earlier choice still selected; the
// review lists every answer with "Change". Nothing reaches the agent until
// "Send answers". The message's own card (QuestionCard) only points here
// while it waits, and records the decisions once answered.
import { createSignal, For, Index, Show } from 'solid-js';
import { answerQuestions, type PendingQuestions, type QuestionAnswer } from '../lib/questions';

export default function QuestionPanel(props: { pending: PendingQuestions }) {
  const questions = () => props.pending.questions;
  const count = () => questions().length;
  /** 0..count-1: a question; count: the review. */
  const [step, setStep] = createSignal(0);
  const [answers, setAnswers] = createSignal<(QuestionAnswer | undefined)[]>([]);
  const [custom, setCustom] = createSignal<string[]>([]);
  /** Editing from the review: answering goes straight back to it. */
  const [fromReview, setFromReview] = createSignal(false);

  const reviewing = () => step() >= count();
  const current = () => questions()[step()];
  const answerAt = (i: number) => answers()[i];

  function answer(i: number, a: QuestionAnswer) {
    setAnswers((all) => {
      const next = [...all];
      next[i] = a;
      return next;
    });
    if (fromReview()) {
      setFromReview(false);
      setStep(count());
    } else {
      setStep(i + 1);
    }
  }

  function setCustomAt(i: number, text: string) {
    setCustom((all) => {
      const next = [...all];
      next[i] = text;
      return next;
    });
  }

  function send() {
    answerQuestions(props.pending.dialogId, props.pending.toolCallId, {
      answers: questions().map((_, i) => answerAt(i) ?? { kind: 'agent' }),
    });
  }

  const describe = (a: QuestionAnswer | undefined) =>
    !a
      ? ''
      : a.kind === 'option'
        ? a.label
        : a.kind === 'custom'
          ? `“${a.text}”`
          : 'Left to the agent';

  return (
    <div class="question-panel" role="dialog" aria-label="The agent's questions">
      <div class="question-panel-head">
        <Show when={step() > 0}>
          <button
            type="button"
            class="question-panel-back"
            onClick={() => {
              setFromReview(false);
              setStep((s) => s - 1);
            }}
            title="Back to the previous question"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M10 3.5 5.5 8l4.5 4.5" />
            </svg>
            Back
          </button>
        </Show>
        <span class="question-panel-step">
          {reviewing()
            ? 'Your answers'
            : count() > 1
              ? `Question ${step() + 1} of ${count()}`
              : 'The agent has a question'}
        </span>
        <Show when={count() > 1}>
          <span class="question-panel-dots" aria-hidden="true">
            <Index each={questions()}>
              {(_, i) => (
                <span
                  class="question-panel-dot"
                  classList={{ done: !!answerAt(i), here: i === step() }}
                />
              )}
            </Index>
          </span>
        </Show>
        <button
          type="button"
          class="link-button question-panel-stop"
          onClick={() =>
            answerQuestions(props.pending.dialogId, props.pending.toolCallId, { stopped: true })
          }
          title="Stop the task instead of answering"
        >
          Stop
        </button>
      </div>

      {/* One question at a time */}
      <Show when={!reviewing() && current()} keyed>
        {(q) => {
          const i = step();
          return (
            <div class="question-panel-body">
              <div class="question-text">{q.question}</div>
              <Show when={q.reason}>
                <div class="question-reason">{q.reason}</div>
              </Show>
              <div class="question-options">
                <For each={q.options}>
                  {(option) => (
                    <button
                      type="button"
                      class="question-option"
                      classList={{
                        chosen:
                          answerAt(i)?.kind === 'option' &&
                          (answerAt(i) as { label: string }).label === option.label,
                      }}
                      onClick={() => answer(i, { kind: 'option', label: option.label })}
                    >
                      <span class="question-option-label">
                        {option.label}
                        <Show when={option.recommended}>
                          <span class="question-recommended">Recommended</span>
                        </Show>
                      </span>
                      <Show when={option.description}>
                        <span class="question-option-description">{option.description}</span>
                      </Show>
                    </button>
                  )}
                </For>
                <Show when={q.allowCustom}>
                  <div
                    class="question-custom"
                    classList={{ chosen: answerAt(i)?.kind === 'custom' }}
                  >
                    <input
                      type="text"
                      placeholder="Other… (Enter to continue)"
                      value={
                        custom()[i] ??
                        (answerAt(i)?.kind === 'custom'
                          ? (answerAt(i) as { text: string }).text
                          : '')
                      }
                      onInput={(e) => setCustomAt(i, e.currentTarget.value)}
                      onKeyDown={(e) => {
                        const text = e.currentTarget.value.trim();
                        if (e.key === 'Enter' && text) {
                          e.preventDefault();
                          answer(i, { kind: 'custom', text });
                        }
                      }}
                    />
                  </div>
                </Show>
                <button
                  type="button"
                  class="question-option agent"
                  classList={{ chosen: answerAt(i)?.kind === 'agent' }}
                  onClick={() => answer(i, { kind: 'agent' })}
                >
                  <span class="question-option-label">You decide</span>
                </button>
              </div>
            </div>
          );
        }}
      </Show>

      {/* Review: everything, changeable, then send */}
      <Show when={reviewing()}>
        <div class="question-panel-body">
          <div class="question-review">
            <Index each={questions()}>
              {(q, i) => (
                <div class="question-review-row">
                  <div class="question-review-text">
                    <span class="question-decision-q">{q().question}</span>
                    <span
                      class="question-decision-a"
                      classList={{ muted: answerAt(i)?.kind === 'agent' }}
                    >
                      {describe(answerAt(i))}
                    </span>
                  </div>
                  <button
                    type="button"
                    class="link-button"
                    onClick={() => {
                      setFromReview(true);
                      setStep(i);
                    }}
                  >
                    Change
                  </button>
                </div>
              )}
            </Index>
          </div>
          <div class="question-actions">
            <button type="button" class="chat-action-button primary" onClick={send}>
              Send answers
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
