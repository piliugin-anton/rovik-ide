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
    "First create your checklist with the todo tool from the plan's steps, with the first step in_progress, then work through them in order, " +
      'one step at a time, each with its own todo call: call todo alone to mark a step in_progress before starting it, ' +
      'do the work, then call todo alone again to mark it done before the next step. Never save up finished steps for one update at the end. ' +
      'For each step, read the files it names before changing them, and make ' +
      'the changes it describes to the functions, types and settings it names. Follow the plan and its decisions. ' +
      'If a step turns out wrong or unnecessary (a file or function isn’t where the plan says, or the change ' +
      'doesn’t fit the code), mark it skipped or changed with the reason instead of silently doing something else. ' +
      'When the steps are done, run the checks in the plan’s Validation section that you can run (build, ' +
      'typecheck, tests, lint), add or update the tests it asks for, and fix what fails. In your final message, ' +
      'say which checks passed, which you couldn’t run and why, what is left to check by hand, and why you changed ' +
      'any files the plan doesn’t mention.',
  ].join('\n');
}
