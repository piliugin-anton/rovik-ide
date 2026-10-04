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
3. Save the plan with the submit_plan tool, as Markdown. It's saved as a file the user may edit before implementing it, so make it stand on its own:
   - a "# " title;
   - a one-paragraph summary of the approach;
   - "## Steps": a numbered list, in order; each step a short title, then what changes and in which files (no code). Roughly 3 to 12 steps, each the size of something worth reviewing on its own;
   - "## Decisions": what the user answered, and what you decided yourself (with why);
   - how to verify the result (checks, tests, what to try by hand);
   - risks or open points, if any.
   The plan goes in submit_plan, not in your reply: after submitting, reply with a few sentences at most. Never write the plan out in a reply, and never write it more than once.
4. If a plan already exists (it's shown below), the user's new message is about that plan. When they add context or ask for changes, revise the existing plan: keep what still holds, work the new information into the steps, decisions and risks it affects, and submit the complete revised plan. Don't replace the plan with a summary of what the user said. If they only ask a question, answer it without submitting.
Keep it concrete enough that it can be implemented step by step without re-investigating. The user implements it by switching to Agent mode.`;

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
