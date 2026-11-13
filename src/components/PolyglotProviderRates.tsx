import { For } from 'solid-js';

export type ProviderRateField = 'input' | 'cachedInput' | 'output';

/** Explicit prices shared by saved-plan and per-task verification review. */
export default function PolyglotProviderRates(props: {
  values: Record<ProviderRateField, string>;
  onChange: (field: ProviderRateField, value: string) => void;
  disabled?: boolean;
}) {
  return (
    <details>
      <summary>Provider rates and supported API</summary>
      <p class="settings-hint">
        Uses the configured provider through the host's credential flow. Enter rates in microunits
        per million tokens, including zero explicitly for a free local model. The reviewed prices
        and provider identity stay fixed for this task; credentials are resolved again on resume.
      </p>
      <For
        each={[
          { key: 'input' as const, label: 'Input rate' },
          { key: 'cachedInput' as const, label: 'Cached input rate' },
          { key: 'output' as const, label: 'Output rate' },
        ]}
      >
        {(field) => (
          <label class="setting-row">
            {field.label}
            <input
              class="settings-input"
              aria-label={field.label}
              type="number"
              min="0"
              step="1"
              value={props.values[field.key]}
              disabled={props.disabled}
              onInput={(event) => props.onChange(field.key, event.currentTarget.value)}
            />
          </label>
        )}
      </For>
    </details>
  );
}
