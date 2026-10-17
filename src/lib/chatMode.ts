// chatMode.ts
//
// How the agent handles the next message, chosen next to the model picker:
//
//   Agent  works on the task: reads, edits, runs commands, checks its
//          changes. Decides open questions itself (as before).
//   Plan   investigates without changing anything, asks the user about
//          what really affects the design (ask_user, lib/questions.ts),
//          and ends with a written plan (steps with a Files line each,
//          decisions, validation). "Implement
//          this plan" then hands it to Agent mode. A question asked in
//          Plan mode is answered in the reply; no plan is written for it.
//   Autopilot  one message is a whole task, done without asking: a plan,
//          its review, the implementation, the checks, the delivery
//          (docs/design/autopilot-plan.md). The backend drives its stages
//          (src-tauri/crates/rovik-core/src/agent_run/autopilot.rs).
//
// A run keeps the mode of the message that started it, so switching the
// selector mid-run (or before a Continue) doesn't change a run under way.
// What Plan mode tells the model and allows is in the backend
// (src-tauri/src/agent_run/plan.rs).
import { createSignal } from 'solid-js';

export type ChatMode = 'agent' | 'plan' | 'autopilot';

const MODES: ChatMode[] = ['agent', 'plan', 'autopilot'];

const KEY = 'rovik.chatMode';

/** A saved mode, or Agent for anything else. */
export function savedMode(saved: string | null): ChatMode {
  return MODES.includes(saved as ChatMode) ? (saved as ChatMode) : 'agent';
}

function load(): ChatMode {
  try {
    return savedMode(localStorage.getItem(KEY));
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
