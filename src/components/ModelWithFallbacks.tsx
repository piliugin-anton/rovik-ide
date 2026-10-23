// ModelWithFallbacks.tsx
//
// A model choice and its fallbacks, as the Subagents page uses them for the
// default subagent model and each kind (lib/subagentSettings.ts). Besides a
// model, a choice can be a special value ("Same as the chat", "Default"); a
// row that offers none (Thorough's kinds) says a model is still to choose.
// Models marked "no tools" can't be picked (every subagent needs tools); a
// saved model that was turned off or removed stays visible, marked, and is
// skipped when the subagent runs.
import { createMemo, For, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import { formatModelLabel } from '../lib/aiSettings';
import { sameModel } from '../lib/permissions';
import { isConnector, usesRovikTools } from '../lib/connectors';

/** Below this context window, a warning shows: subagents read a lot. */
export const SMALL_CONTEXT_TOKENS = 32_000;

interface ModelOption {
  key: string;
  selection: ModelSelection;
  label: string;
  provider: string;
  usable: boolean;
  contextTokens?: number;
}

const keyOf = (sel: ModelSelection) => `model:${sel.providerId}::${sel.modelId}`;

/**
 * The models a subagent can use: enabled ones that take tools, by
 * provider. A connector's models (lib/connectors.ts) are among them only
 * when it runs Rovik's tools: with its own, it has none of the subagent's.
 */
export function modelOptions(providers: Provider[]): ModelOption[] {
  return providers.flatMap((provider) =>
    provider.models.map((model) => ({
      key: keyOf({ providerId: provider.id, modelId: model.id }),
      selection: { providerId: provider.id, modelId: model.id },
      label: formatModelLabel(provider.name, model),
      provider: isConnector(provider)
        ? `Connector · ${provider.name}`
        : provider.name || provider.baseUrl,
      usable:
        model.enabled && !model.noTools && (!isConnector(provider) || usesRovikTools(provider)),
      contextTokens: model.contextOverride ?? model.contextLength,
    }))
  );
}

export default function ModelWithFallbacks<S extends string>(props: {
  providers: Provider[];
  label: string;
  value: S | ModelSelection;
  /** The special choices offered before the models ("Same as the chat"…). */
  specials: { value: S; label: string }[];
  /** Shown while the value is a special not offered: no model chosen yet. */
  unset?: string;
  fallbacks: ModelSelection[];
  onChange: (value: S | ModelSelection) => void;
  onFallbacksChange: (fallbacks: ModelSelection[]) => void;
  disabled?: boolean;
}) {
  const all = createMemo(() => modelOptions(props.providers));
  const usable = createMemo(() => all().filter((o) => o.usable));
  const groups = createMemo(() => {
    const map = new Map<string, ModelOption[]>();
    for (const o of usable()) map.set(o.provider, [...(map.get(o.provider) ?? []), o]);
    return [...map.entries()];
  });
  const optionFor = (sel: ModelSelection) => all().find((o) => sameModel(o.selection, sel));
  const selected = () =>
    typeof props.value === 'string' ? undefined : optionFor(props.value as ModelSelection);
  const valueKey = () =>
    typeof props.value === 'string'
      ? `special:${props.value}`
      : (selected()?.key ?? keyOf(props.value as ModelSelection));

  function choose(key: string) {
    if (key.startsWith('special:')) {
      props.onChange(key.slice('special:'.length) as S);
      return;
    }
    const option = usable().find((o) => o.key === key);
    if (!option) return;
    props.onChange(option.selection);
    // A model can't be its own fallback.
    props.onFallbacksChange(props.fallbacks.filter((f) => !sameModel(f, option.selection)));
  }

  const fallbackRows = () =>
    props.fallbacks.map((sel) => ({
      sel,
      option: optionFor(sel),
    }));

  const candidates = () =>
    usable().filter(
      (o) =>
        !(
          typeof props.value !== 'string' && sameModel(o.selection, props.value as ModelSelection)
        ) && !props.fallbacks.some((f) => sameModel(f, o.selection))
    );

  const smallContext = () => {
    const c = selected()?.contextTokens;
    return c !== undefined && c > 0 && c < SMALL_CONTEXT_TOKENS;
  };

  return (
    <div class="model-with-fallbacks">
      <select
        class="settings-select"
        aria-label={props.label}
        value={valueKey()}
        disabled={props.disabled}
        onChange={(e) => choose(e.currentTarget.value)}
      >
        <Show
          when={
            typeof props.value === 'string' && !props.specials.some((s) => s.value === props.value)
          }
        >
          <option value={valueKey()} disabled>
            {props.unset ?? 'Choose a model…'}
          </option>
        </Show>
        <For each={props.specials}>
          {(s) => <option value={`special:${s.value}`}>{s.label}</option>}
        </For>
        {/* A saved model that can't be used anymore stays visible, marked. */}
        <Show when={typeof props.value !== 'string' && !selected()?.usable}>
          <option value={valueKey()}>
            {selected()?.label ?? (props.value as ModelSelection).modelId} (turned off)
          </option>
        </Show>
        <For each={groups()}>
          {([provider, options]) => (
            <optgroup label={provider}>
              <For each={options}>{(o) => <option value={o.key}>{o.label}</option>}</For>
            </optgroup>
          )}
        </For>
      </select>
      <Show when={typeof props.value !== 'string' && !selected()?.usable}>
        <p class="settings-hint setting-explain warning">
          This model is turned off, removed, or can’t use tools, so it’s skipped:{' '}
          {props.specials.length > 0
            ? 'the next model is used instead.'
            : 'only the fallbacks here stand in for it.'}
        </p>
      </Show>
      <Show when={smallContext()}>
        <p class="settings-hint setting-explain warning">
          This model’s context window is under 32k tokens: subagents read a lot, and may run out of
          room.
        </p>
      </Show>
      <div class="fallback-list">
        <For each={fallbackRows()}>
          {(row, i) => (
            <div class="fallback-row">
              <span class="fallback-order">{i() + 1}.</span>
              <span class="fallback-label" classList={{ off: !row.option?.usable }}>
                {row.option?.label ?? row.sel.modelId}
                {row.option?.usable ? '' : ' (turned off)'}
              </span>
              <button
                class="icon-button"
                disabled={props.disabled}
                onClick={() =>
                  props.onFallbacksChange(props.fallbacks.filter((f) => !sameModel(f, row.sel)))
                }
                aria-label={`Remove fallback ${row.option?.label ?? row.sel.modelId}`}
                title="Remove"
              >
                ×
              </button>
            </div>
          )}
        </For>
        <Show when={candidates().length > 0}>
          <select
            class="settings-select"
            value=""
            disabled={props.disabled}
            aria-label={`${props.label}: add a fallback`}
            onChange={(e) => {
              const option = candidates().find((o) => o.key === e.currentTarget.value);
              if (option) props.onFallbacksChange([...props.fallbacks, option.selection]);
              e.currentTarget.value = '';
            }}
          >
            <option value="">
              {props.fallbacks.length ? 'Add another fallback…' : 'Add a fallback (optional)…'}
            </option>
            <For each={candidates()}>{(o) => <option value={o.key}>{o.label}</option>}</For>
          </select>
        </Show>
      </div>
    </div>
  );
}
