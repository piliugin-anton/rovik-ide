// ConnectorControls.tsx
//
// The rows a connector's form is made of (ClaudeCodeFields.tsx,
// CodexFields.tsx), each labelled as on the other Settings pages: a choice
// of a setting's values with the chosen one's line under it, and a text
// field with its own.
import { For, Show } from 'solid-js';
import type { Choice } from '../lib/connectors';
import { SettingRow } from './SettingsControls';

/** A select of `choices`; under it, the chosen one's line, all of it as its tooltip. */
export function ChoiceSelect<T extends string>(props: {
  label: string;
  choices: Choice<T>[];
  value: T;
  onChange: (value: T) => void;
}) {
  const chosen = () => props.choices.find((c) => c.value === props.value);
  return (
    <>
      <SettingRow label={props.label}>
        <select
          class="settings-select"
          aria-label={props.label}
          value={props.value}
          onChange={(e) => props.onChange(e.currentTarget.value as T)}
        >
          <For each={props.choices}>
            {(c) => (
              <option value={c.value} title={c.hint}>
                {c.label}
              </option>
            )}
          </For>
        </select>
      </SettingRow>
      <Show when={chosen()}>
        {(c) => (
          <p class="settings-hint setting-explain" title={c().detail ?? c().hint}>
            {c().hint}
          </p>
        )}
      </Show>
    </>
  );
}

/** A text field on its own row, with its line under it when it has one. */
export function TextRow(props: {
  label: string;
  value: string;
  placeholder: string;
  onInput: (value: string) => void;
  /** Its line, under it, and that line's tooltip. */
  explain?: string;
  title?: string;
  type?: 'text' | 'password';
}) {
  return (
    <>
      <SettingRow label={props.label}>
        <input
          type={props.type ?? 'text'}
          class="settings-input"
          aria-label={props.label}
          value={props.value}
          onInput={(e) => props.onInput(e.currentTarget.value)}
          placeholder={props.placeholder}
          spellcheck={false}
        />
      </SettingRow>
      <Show when={props.explain}>
        <p class="settings-hint setting-explain" title={props.title}>
          {props.explain}
        </p>
      </Show>
    </>
  );
}
