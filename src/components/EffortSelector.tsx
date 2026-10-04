// EffortSelector.tsx: the reasoning effort, next to the model picker
// (lib/reasoning.ts). No label: each choice says what it is ("High effort"),
// and it only shows for models that take an effort.
import { createSignal, For, onCleanup, Show } from 'solid-js';
import { EFFORTS, type Effort } from '../lib/reasoning';

export default function EffortSelector(props: {
  value: Effort;
  onChange: (effort: Effort) => void;
}) {
  const [open, setOpen] = createSignal(false);
  let root: HTMLDivElement | undefined;
  const current = () => EFFORTS.find((e) => e.id === props.value) ?? EFFORTS[0];

  const onOutside = (e: PointerEvent) => {
    if (root && !root.contains(e.target as Node)) setOpen(false);
  };
  window.addEventListener('pointerdown', onOutside, true);
  onCleanup(() => window.removeEventListener('pointerdown', onOutside, true));

  return (
    <div class="effort-selector" ref={root}>
      <button
        type="button"
        class="model-picker-trigger effort-trigger"
        classList={{ set: props.value !== 'default' }}
        aria-haspopup="listbox"
        aria-expanded={open()}
        title={`Reasoning effort: ${current().hint}`}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') setOpen(false);
        }}
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M8 1.75a4.25 4.25 0 0 0-2.5 7.7V11h5V9.45A4.25 4.25 0 0 0 8 1.75ZM6 13.25h4M6.75 14.75h2.5" />
        </svg>
        <span class="effort-trigger-label">{current().label}</span>
        <svg
          class="model-picker-caret"
          width="10"
          height="10"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M4 10l4-4 4 4" />
        </svg>
      </button>
      <Show when={open()}>
        <div class="effort-menu" role="listbox" aria-label="Reasoning effort">
          <For each={EFFORTS}>
            {(effort) => (
              <button
                type="button"
                role="option"
                class="effort-option"
                classList={{ active: effort.id === props.value }}
                aria-selected={effort.id === props.value}
                onClick={() => {
                  props.onChange(effort.id);
                  setOpen(false);
                }}
              >
                <span class="effort-option-label">{effort.label}</span>
                <span class="effort-option-hint">{effort.hint}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
