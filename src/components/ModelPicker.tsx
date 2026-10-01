// ModelPicker.tsx
//
// Replaces the native <select>: WebKitGTK draws <option> lists with the GTK
// theme (often light), so they can't be reliably styled dark. This popover
// also adds search, which matters once a provider like OpenRouter returns
// hundreds of models.
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import { formatModelLabel } from '../lib/aiSettings';

interface ModelPickerProps {
  providers: Provider[];
  selection: ModelSelection | null;
  onSelect: (selection: ModelSelection) => void;
  onOpenSettings: () => void;
}

interface PickerItem extends ModelSelection {
  key: string;
  label: string;
}

export default function ModelPicker(props: ModelPickerProps) {
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal('');
  const [highlight, setHighlight] = createSignal(0);

  let rootRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;
  let listRef: HTMLDivElement | undefined;

  const items = createMemo<PickerItem[]>(() => {
    return props.providers.flatMap((p) =>
      p.models
        .filter((m) => m.enabled)
        .map((m) => ({
          providerId: p.id,
          modelId: m.id,
          key: `${p.id}::${m.id}`,
          label: formatModelLabel(p.name, m),
        }))
    );
  });

  const filtered = createMemo(() => {
    const words = query().trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (words.length === 0) return items();
    return items().filter((it) => {
      const hay = it.label.toLowerCase();
      return words.every((w) => hay.includes(w));
    });
  });

  const current = createMemo(() => {
    const sel = props.selection;
    return sel
      ? items().find((it) => it.providerId === sel.providerId && it.modelId === sel.modelId)
      : undefined;
  });

  function openPicker() {
    if (items().length === 0) {
      props.onOpenSettings();
      return;
    }
    setQuery('');
    const idx = current() ? items().indexOf(current()!) : 0;
    setHighlight(Math.max(0, idx));
    setOpen(true);
    queueMicrotask(() => {
      searchRef?.focus();
      scrollHighlightIntoView();
    });
  }

  function choose(item: PickerItem | undefined) {
    if (!item) return;
    props.onSelect({ providerId: item.providerId, modelId: item.modelId });
    setOpen(false);
  }

  function scrollHighlightIntoView() {
    listRef
      ?.querySelector<HTMLElement>(`[data-index="${highlight()}"]`)
      ?.scrollIntoView({ block: 'nearest' });
  }

  function onKeyDown(e: KeyboardEvent) {
    const count = filtered().length;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        if (count) setHighlight((h) => (h + 1) % count);
        scrollHighlightIntoView();
        break;
      case 'ArrowUp':
        e.preventDefault();
        if (count) setHighlight((h) => (h - 1 + count) % count);
        scrollHighlightIntoView();
        break;
      case 'Enter':
        e.preventDefault();
        choose(filtered()[highlight()]);
        break;
      case 'Escape':
        e.preventDefault();
        setOpen(false);
        break;
    }
  }

  // Close on any click outside the picker.
  createEffect(() => {
    if (!open()) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef && !rootRef.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    onCleanup(() => document.removeEventListener('mousedown', onDown));
  });

  return (
    <div class="model-picker" ref={rootRef}>
      <button
        type="button"
        class="model-picker-trigger"
        onClick={() => (open() ? setOpen(false) : openPicker())}
        aria-haspopup="listbox"
        aria-expanded={open()}
        title={current()?.label}
      >
        <span class="model-picker-value" classList={{ placeholder: !current() }}>
          {current()?.label ??
            (items().length === 0 ? 'No models yet, add a provider' : 'Select a model')}
        </span>
        <svg
          class="model-picker-caret"
          viewBox="0 0 16 16"
          width="12"
          height="12"
          aria-hidden="true"
        >
          <path
            d="M4 10 8 6l4 4"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>

      <Show when={open()}>
        <div class="model-picker-popover" onKeyDown={onKeyDown}>
          <input
            ref={searchRef}
            class="model-picker-search"
            value={query()}
            onInput={(e) => {
              setQuery(e.currentTarget.value);
              setHighlight(0);
            }}
            placeholder={`Search ${items().length} models`}
            spellcheck={false}
          />
          <div class="model-picker-list" ref={listRef} role="listbox">
            <For
              each={filtered()}
              fallback={<div class="model-picker-empty">No matching models</div>}
            >
              {(item, i) => (
                <div
                  class="model-picker-option"
                  classList={{
                    highlighted: i() === highlight(),
                    selected: item.key === current()?.key,
                  }}
                  role="option"
                  aria-selected={item.key === current()?.key}
                  data-index={i()}
                  title={item.label}
                  onMouseMove={() => setHighlight(i())}
                  onClick={() => choose(item)}
                >
                  {item.label}
                </div>
              )}
            </For>
          </div>
          <button
            type="button"
            class="model-picker-manage"
            onClick={() => {
              setOpen(false);
              props.onOpenSettings();
            }}
          >
            Manage providers…
          </button>
        </div>
      </Show>
    </div>
  );
}
