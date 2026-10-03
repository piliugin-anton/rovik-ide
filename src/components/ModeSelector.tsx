// ModeSelector.tsx: Agent / Plan, left of the model picker under the chat
// input (lib/chatMode.ts). A two-way switch rather than a menu: both
// choices are always visible, and switching is one click.
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
];

export default function ModeSelector() {
  return (
    <div class="mode-selector" role="radiogroup" aria-label="Chat mode">
      <For each={MODES}>
        {(mode) => (
          <button
            type="button"
            role="radio"
            class="mode-option"
            classList={{ active: chatMode() === mode.id, plan: mode.id === 'plan' }}
            aria-checked={chatMode() === mode.id}
            title={mode.hint}
            onClick={() => setChatMode(mode.id)}
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill={mode.id === 'agent' ? 'currentColor' : 'none'}
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
