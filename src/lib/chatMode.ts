// chatMode.ts
//
// How the agent handles the next message, chosen next to the model picker:
//
//   Agent  works on the task: reads, edits, runs commands, checks its
//          changes. Decides open questions itself (as before).
//   Plan   investigates without changing anything, asks the user about
//          what really affects the design (ask_user, lib/questions.ts),
//          and ends with a written plan, decisions included. "Implement
//          this plan" then hands it to Agent mode.
//
// A run keeps the mode of the message that started it, so switching the
// selector mid-run (or before a Continue) doesn't change a run under way.
// What Plan mode tells the model and allows is in the backend
// (src-tauri/src/agent_run/plan.rs).
import { createSignal } from 'solid-js';

export type ChatMode = 'agent' | 'plan';

const KEY = 'rovik.chatMode';

function load(): ChatMode {
  try {
    return localStorage.getItem(KEY) === 'plan' ? 'plan' : 'agent';
  } catch {
    return 'agent';
  }
}

const [mode, setModeSignal] = createSignal<ChatMode>(load());

/** The mode the next message is sent in (the selector). */
export const chatMode = mode;

export function setChatMode(next: ChatMode) {
  setModeSignal(next);
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // not remembered across restarts, that's all
  }
}

/** The text sent to implement a plan (Agent mode): the plan in full, kept in view for the whole run. */
export function implementationRequest(plan: string, file?: string): string {
  return [
    file
      ? `Implement this plan. It's saved in ${file}; the user may have edited it, and this is its current version.`
      : 'Implement this plan.',
    '',
    '<plan>',
    plan.trim(),
    '</plan>',
    '',
    "First create your checklist with the todo tool from the plan's steps, then work through them in order, " +
      'marking each in progress and done. Follow the plan and its decisions. If a step turns out wrong or ' +
      'unnecessary, mark it skipped or changed with the reason instead of silently doing something else. ' +
      'If you change files the plan doesn’t mention, say why in your final message.',
  ].join('\n');
}
