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

/** Tools Plan mode doesn't offer (or allow, if a model calls them anyway). */
export const PLAN_BLOCKED_TOOLS = new Set(['write_file', 'edit_file', 'run_command']);

/** Questions one run may ask; past that, the agent decides. */
export const MAX_QUESTIONS_PER_RUN = 3;

/** Added to the system prompt in Plan mode. */
export const PLAN_MODE_INSTRUCTIONS = `## Plan mode

The user chose Plan mode: plan the task, don't carry it out. You can read files, list folders, search the code and use the web; you can't change files or run commands.

1. Investigate first: read the relevant code, find the existing patterns and conventions the change should follow.
2. Ask only what the code can't tell you and what really changes the design, the behavior or the scope (two existing approaches to choose between, an unclear requirement, a trade-off only the user can make). Use the ask_user tool, early, with all your questions in one call, as concrete options with a short reason. Don't ask about names, formatting or anything you can decide from the repository: decide it and say so in the plan. Never ask a question in plain text and stop; use ask_user.
3. End with the plan as your final message, in Markdown:
   - a one-paragraph summary of the approach;
   - the steps, in order, each naming the files it changes and what changes in them;
   - "Decisions": what the user answered, and what you decided yourself (with why);
   - how to verify the result (checks, tests, what to try by hand);
   - risks or open points, if any.
Keep it concrete enough that it can be implemented step by step without re-investigating. The user implements it by switching to Agent mode.`;
