// ModeSelector.tsx: Agent / Plan / Autopilot, left of the model picker
// under the chat input (lib/chatMode.ts). A switch rather than a menu:
// every choice is always visible, and switching is one click. A mode the
// chat's model can't run (`modeUnavailable`) is dimmed, its reason its
// tooltip, and can't be picked.
import { For } from 'solid-js';
import { chatMode, setChatMode, type ChatMode } from '../lib/chatMode';

const MODES: { id: ChatMode; label: string; hint: string; icon: string }[] = [
  {
    id: 'agent',
    label: 'Agent',
    hint: 'Agent: works on the task, editing files and running checks',
    // a play-like arrow: does the work
    icon: 'M5 3.5v9l7-4.5-7-4.5Z',
  },
  {
    id: 'plan',
    label: 'Plan',
    hint: 'Plan: looks into the task without changing anything, asks what only you can decide, and writes a plan',
    // a checklist
    icon: 'M6.5 4.5h7M6.5 8h7M6.5 11.5h7M2.75 4.5h.5M2.75 8h.5M2.75 11.5h.5',
  },
  {
    id: 'autopilot',
    label: 'Autopilot',
    hint: 'Autopilot: one message is a whole task. It plans, reviews, implements and checks it without asking, and writes down what it couldn’t decide',
    // fast-forward: goes on by itself
    icon: 'M2 3.5v9L7.5 8 2 3.5ZM8 3.5v9L13.5 8 8 3.5Z',
  },
];

/** Drawn filled rather than stroked. */
const FILLED: ChatMode[] = ['agent', 'autopilot'];

export default function ModeSelector(props: {
  /** Why a mode can't run on the chat's model, if it can't. */
  unavailable?: (mode: ChatMode) => string | null;
}) {
  const reason = (mode: ChatMode) => props.unavailable?.(mode) ?? null;
  return (
    <div class="mode-selector" role="radiogroup" aria-label="Chat mode">
      <For each={MODES}>
        {(mode) => (
          <button
            type="button"
            role="radio"
            class="mode-option"
            classList={{
              active: chatMode() === mode.id,
              plan: mode.id === 'plan',
              autopilot: mode.id === 'autopilot',
              unavailable: !!reason(mode.id),
            }}
            aria-checked={chatMode() === mode.id}
            // Not `disabled`: a disabled button shows no tooltip in WebKit.
            aria-disabled={!!reason(mode.id)}
            title={reason(mode.id) ?? mode.hint}
            onClick={() => {
              if (!reason(mode.id)) setChatMode(mode.id);
            }}
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill={FILLED.includes(mode.id) ? 'currentColor' : 'none'}
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d={mode.icon} />
            </svg>
            {mode.label}
          </button>
        )}
      </For>
    </div>
  );
}
