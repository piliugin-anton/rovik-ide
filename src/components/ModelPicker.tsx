// ModelPicker.tsx
//
// The chat's model, from the model providers' models and the connectors'
// (lib/connectors.ts): a popover with search, and a switch to see all of
// them, the providers' alone or the connectors' alone. Each model wears
// its source's icon (ModelSourceIcon.tsx), so what runs a chat is clear
// at a glance: a provider's model is called over its API; a connector's
// model hands the task to a harness such as Claude Code.
//
// Replaces the native <select>: WebKitGTK draws <option> lists with the
// GTK theme (often light), so they can't be reliably styled dark. The
// search matters once a provider like OpenRouter returns hundreds of
// models.
import { createEffect, createMemo, createSignal, For, onCleanup, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import type { SettingsPage } from '../lib/appSettings';
import { formatModelLabel } from '../lib/aiSettings';
import { isConnector, kindOfProvider, connectorTitle } from '../lib/connectors';
import ModelSourceIcon, { type ModelSource } from './ModelSourceIcon';

interface ModelPickerProps {
  providers: Provider[];
  selection: ModelSelection | null;
  onSelect: (selection: ModelSelection) => void;
  /** Opens the settings on the page that manages the models shown. */
  onOpenSettings: (page: Extract<SettingsPage, 'providers' | 'connectors'>) => void;
}

export interface PickerItem extends ModelSelection {
  key: string;
  label: string;
  source: ModelSource;
  /** The provider's or connector's name, for the search. */
  provider: string;
}

/** Which models the list shows. */
export type SourceFilter = 'all' | 'provider' | 'connector';

export const SOURCE_FILTERS: { value: SourceFilter; label: string; title: string }[] = [
  { value: 'all', label: 'All', title: 'Every enabled model' },
  { value: 'provider', label: 'Model providers', title: 'Models called over an API' },
  {
    value: 'connector',
    label: 'Connectors',
    title: 'Coding harnesses installed where the chat runs',
  },
];

/** Every enabled model, as the picker lists it, providers first. */
export function pickerItems(providers: Provider[]): PickerItem[] {
  const items = providers.flatMap((p) => {
    const connector = isConnector(p);
    const name = p.name || (connector ? connectorTitle(kindOfProvider(p)) : p.baseUrl);
    return p.models
      .filter((m) => m.enabled)
      .map((m) => ({
        providerId: p.id,
        modelId: m.id,
        key: `${p.id}::${m.id}`,
        label: connector ? `${name}/${m.id}` : formatModelLabel(p.name, m),
        source: (connector ? 'connector' : 'provider') as ModelSource,
        provider: name,
      }));
  });
  return [
    ...items.filter((i) => i.source === 'provider'),
    ...items.filter((i) => i.source === 'connector'),
  ];
}

/** The items a search and a source filter leave. */
export function filterItems(
  items: PickerItem[],
  query: string,
  source: SourceFilter
): PickerItem[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return items.filter((it) => {
    if (source !== 'all' && it.source !== source) return false;
    if (words.length === 0) return true;
    const hay = `${it.label} ${it.provider}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

export default function ModelPicker(props: ModelPickerProps) {
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal('');
  const [source, setSource] = createSignal<SourceFilter>('all');
  const [highlight, setHighlight] = createSignal(0);

  let rootRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;
  let listRef: HTMLDivElement | undefined;

  const items = createMemo<PickerItem[]>(() => pickerItems(props.providers));
  const hasBoth = createMemo(
    () =>
      items().some((i) => i.source === 'connector') && items().some((i) => i.source === 'provider')
  );
  const filtered = createMemo(() => filterItems(items(), query(), source()));
  const counts = createMemo(() => ({
    all: items().length,
    provider: items().filter((i) => i.source === 'provider').length,
    connector: items().filter((i) => i.source === 'connector').length,
  }));

  const current = createMemo(() => {
    const sel = props.selection;
    return sel
      ? items().find((it) => it.providerId === sel.providerId && it.modelId === sel.modelId)
      : undefined;
  });

  /** The page that manages what the list shows: connectors when it shows only them. */
  const managePage = () =>
    source() === 'connector' ||
    (source() === 'all' && items().length > 0 && counts().provider === 0)
      ? 'connectors'
      : 'providers';

  function openPicker() {
    if (items().length === 0) {
      props.onOpenSettings('providers');
      return;
    }
    setQuery('');
    // The current model's source, so it's in view; all when it isn't listed.
    setSource('all');
    const idx = current() ? filtered().indexOf(current()!) : 0;
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

  function pickSource(next: SourceFilter) {
    setSource(next);
    setHighlight(0);
    searchRef?.focus();
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
      case 'Tab': {
        // Tab cycles the sources, so the switch works from the keyboard.
        if (!hasBoth()) return;
        e.preventDefault();
        const order: SourceFilter[] = ['all', 'provider', 'connector'];
        const at = order.indexOf(source());
        pickSource(order[(at + (e.shiftKey ? order.length - 1 : 1)) % order.length]);
        break;
      }
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
        <Show when={current()}>{(item) => <ModelSourceIcon source={item().source} />}</Show>
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
            placeholder={`Search ${filtered().length === items().length ? items().length : `${filtered().length} of ${items().length}`} models`}
            spellcheck={false}
          />
          <Show when={hasBoth()}>
            <div class="model-picker-sources segmented" role="radiogroup" aria-label="Which models">
              <For each={SOURCE_FILTERS}>
                {(f) => (
                  <button
                    type="button"
                    class="segmented-option"
                    role="radio"
                    aria-checked={source() === f.value}
                    classList={{ selected: source() === f.value }}
                    data-source={f.value}
                    title={f.title}
                    onClick={() => pickSource(f.value)}
                  >
                    <Show when={f.value !== 'all'}>
                      <ModelSourceIcon source={f.value as ModelSource} size={11} />
                    </Show>
                    <span class="model-picker-source-label">{f.label}</span>
                    <span class="model-picker-source-count">{counts()[f.value]}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
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
                    connector: item.source === 'connector',
                  }}
                  role="option"
                  aria-selected={item.key === current()?.key}
                  data-index={i()}
                  data-source={item.source}
                  title={item.label}
                  onMouseMove={() => setHighlight(i())}
                  onClick={() => choose(item)}
                >
                  <ModelSourceIcon source={item.source} />
                  <span class="model-picker-option-label">{item.label}</span>
                </div>
              )}
            </For>
          </div>
          <button
            type="button"
            class="model-picker-manage"
            onClick={() => {
              const page = managePage();
              setOpen(false);
              props.onOpenSettings(page);
            }}
          >
            {managePage() === 'connectors' ? 'Manage connectors…' : 'Manage providers…'}
          </button>
        </div>
      </Show>
    </div>
  );
}
