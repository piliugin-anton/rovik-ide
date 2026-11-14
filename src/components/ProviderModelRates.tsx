import { createMemo, createSignal, For, Show } from 'solid-js';
import type { ModelRates, ProviderModel } from '../types';
import { formatHarnessRate, parseHarnessRate, savedHarnessRates } from '../lib/harnessRates';

const fields: { key: keyof ModelRates; label: string }[] = [
  { key: 'input', label: 'Input price' },
  { key: 'cachedInput', label: 'Cached input price' },
  { key: 'output', label: 'Output price' },
];

/** Reusable model prices edited with the provider, instead of on every verified task. */
export default function ProviderModelRates(props: {
  models: ProviderModel[];
  onChange: (models: ProviderModel[]) => void;
}) {
  const [selected, setSelected] = createSignal('');
  const model = createMemo(
    () =>
      props.models.find((entry) => entry.id === selected()) ??
      props.models.find((entry) => entry.enabled) ??
      props.models[0]
  );
  function update(field: keyof ModelRates, raw: string) {
    const current = model();
    if (!current) return;
    const value = parseHarnessRate(raw);
    const rates = { ...current.harnessRates };
    if (value === undefined) delete rates[field];
    else rates[field] = value;
    props.onChange(
      props.models.map((entry) =>
        entry.id === current.id
          ? { ...entry, harnessRates: Object.keys(rates).length ? rates : undefined }
          : entry
      )
    );
  }
  return (
    <Show when={props.models.length > 0}>
      <details class="settings-section settings-details">
        <summary class="settings-details-summary">
          <span class="settings-section-title">Verification pricing</span>
          <span class="settings-hint">Saved per model</span>
        </summary>
        <p class="settings-hint">
          Verified plans and Autopilot reuse these prices. Enter your provider’s price per million
          tokens in its billing currency, for example 2.50. Up to six decimal places are supported.
          Enter zero explicitly for a free model; a blank price is unknown.
        </p>
        <div class="profile-form">
          <label>
            Model
            <select
              class="settings-select"
              aria-label="Model for verification pricing"
              value={model()?.id ?? ''}
              onChange={(event) => setSelected(event.currentTarget.value)}
            >
              <For each={props.models}>
                {(entry) => <option value={entry.id}>{entry.id}</option>}
              </For>
            </select>
          </label>
          <For each={fields}>
            {(field) => (
              <label>
                {field.label}
                <input
                  class="settings-input"
                  type="number"
                  inputMode="decimal"
                  min="0"
                  step="0.000001"
                  aria-label={`${field.label} per million tokens`}
                  placeholder="Unknown"
                  value={formatHarnessRate(model()?.harnessRates?.[field.key])}
                  onChange={(event) => {
                    update(field.key, event.currentTarget.value);
                    event.currentTarget.value = formatHarnessRate(
                      model()?.harnessRates?.[field.key]
                    );
                  }}
                />
              </label>
            )}
          </For>
          <p class="settings-hint" role="status">
            {savedHarnessRates(model())
              ? 'All three prices are configured. Save this provider to reuse them for new tasks.'
              : 'All three prices are needed for verified tasks. Ordinary chat remains available.'}
          </p>
        </div>
      </details>
    </Show>
  );
}
