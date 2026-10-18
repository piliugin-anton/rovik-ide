// SettingsControls.tsx
//
// The small controls the Settings pages share: a "Label  [control]" line,
// a segmented choice, a number field that saves what it shows, and a
// status line that says whether a setting has an effect right now.
import { For, type JSX } from 'solid-js';
import { numberFieldValue } from '../lib/subagentSettings';

/** One "Label  [control]" line. */
export function SettingRow(props: { label: string; children: JSX.Element }) {
  return (
    <div class="setting-row">
      <span class="setting-row-label">{props.label}</span>
      <div class="setting-row-control">{props.children}</div>
    </div>
  );
}

/** One choice among a few, each a button. */
export function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string; title?: string }[];
  disabled?: boolean;
  onChange: (value: T) => void;
}) {
  return (
    <div class="segmented" role="radiogroup" aria-label={props.label}>
      <For each={props.options}>
        {(option) => (
          <button
            type="button"
            class="segmented-option"
            role="radio"
            aria-checked={props.value === option.value}
            classList={{ selected: props.value === option.value }}
            title={option.title}
            disabled={props.disabled}
            onClick={() => props.onChange(option.value)}
          >
            {option.label}
          </button>
        )}
      </For>
    </div>
  );
}

/**
 * A number with a label before it. Empty saves `undefined` (the
 * placeholder says what that means: a default, or no limit); a value is
 * kept within `min`..`max`, whole when the step is.
 */
export function NumberField(props: {
  label: string;
  value: number | undefined;
  placeholder: number | string;
  min: number;
  max: number;
  step?: number;
  disabled?: boolean;
  /** Said of the field, for assistive technology, when the label isn't enough. */
  ariaLabel?: string;
  onChange: (value: number | undefined) => void;
}) {
  return (
    <label class="setting-number-field">
      <span>{props.label}</span>
      <input
        type="number"
        class="settings-input setting-number"
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        value={props.value ?? ''}
        placeholder={String(props.placeholder)}
        disabled={props.disabled}
        aria-label={props.ariaLabel}
        onChange={(e) => {
          const value = numberFieldValue(
            e.currentTarget.value,
            props.min,
            props.max,
            props.step ?? 1
          );
          // Shows what is saved: when that is the value it had already, the
          // field wouldn't be redrawn and would keep the text typed.
          e.currentTarget.value = value === undefined ? '' : String(value);
          props.onChange(value);
        }}
      />
    </label>
  );
}

/**
 * Whether a setting has an effect now, and why not when it hasn't: "No
 * effect now: virtual files aren't mounted…". A warning when it hasn't.
 */
export function SettingStatus(props: { warning?: boolean; children: JSX.Element }) {
  return (
    <p class="settings-status" classList={{ warning: props.warning }} role="status">
      {props.children}
    </p>
  );
}
