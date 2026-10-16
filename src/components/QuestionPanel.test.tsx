// Component test: answers given in the question panel survive the panel
// going away (another sidebar tab, another chat) and coming back, until
// they're sent. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import QuestionPanel from './QuestionPanel';
import {
  pendingQuestions,
  questionsDraft,
  registerQuestions,
  type PendingQuestions,
} from '../lib/questions';

const sent: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    sent.push({ cmd, args });
  },
};

const pending: PendingQuestions = {
  dialogId: 'chat-q',
  toolCallId: 'ask-1',
  questions: ['color', 'size', 'shape'].map((what) => ({
    question: `Which ${what}?`,
    options: [{ label: `${what} A` }, { label: `${what} B` }],
    allowCustom: true,
  })),
};

const step = (root: HTMLElement) => root.querySelector('.question-panel-step')?.textContent;
const option = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('.question-option')].find((b) =>
    b.textContent?.startsWith(label)
  )!;

test('answers so far come back when the panel does, and go once sent', async () => {
  registerQuestions(pending);
  const first = container();
  const disposeFirst = render(() => <QuestionPanel pending={pending} />, first);
  option(first, 'color B').click();
  option(first, 'size A').click();
  assert.equal(step(first), 'Question 3 of 3');
  // The sidebar shows Files: the chat, and the panel with it, go away.
  disposeFirst();

  const again = container();
  const dispose = render(() => <QuestionPanel pending={pending} />, again);
  assert.equal(step(again), 'Question 3 of 3', 'back at the question it was on');
  option(again, 'shape A').click();
  assert.equal(step(again), 'Your answers');
  assert.match(again.textContent ?? '', /color B/);
  assert.match(again.textContent ?? '', /size A/);
  [...again.querySelectorAll('button')].find((b) => b.textContent === 'Send answers')!.click();
  await Promise.resolve();
  const answered = sent.find((s) => s.cmd === 'agent_answer_questions');
  assert.ok(answered, 'sent');
  assert.deepEqual(answered.args.outcome?.answers ?? answered.args.answers, [
    { kind: 'option', label: 'color B' },
    { kind: 'option', label: 'size A' },
    { kind: 'option', label: 'shape A' },
  ]);
  dispose();
  assert.equal(pendingQuestions('chat-q', 'ask-1'), undefined, 'no longer waiting');
  assert.equal(questionsDraft('chat-q', 'ask-1'), undefined, 'the draft went with them');
});

test('a typed answer is sent as typed: no text checking (macOS would curl its quotes)', () => {
  const typed: PendingQuestions = { ...pending, toolCallId: 'ask-typed' };
  registerQuestions(typed);
  const root = container();
  const dispose = render(() => <QuestionPanel pending={typed} />, root);
  const other = root.querySelector<HTMLInputElement>('.question-custom input');
  assert.ok(other, 'the input for an answer of your own');
  assert.equal(other.getAttribute('spellcheck'), 'false');
  dispose();
});
