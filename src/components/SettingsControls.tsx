// SettingsControls.tsx
//
// The small controls the Settings pages share: a "Label  [control]" line,
// a segmented choice, a number field that saves what it shows, a status
// line that says whether a setting has an effect right now, and the
// passphrase field of the file that keeps secrets where there's no keychain.
import { createSignal, For, Show, type JSX } from 'solid-js';
import { numberFieldValue } from '../lib/subagentSettings';
import { unlockSecrets } from '../lib/hosts';

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

/**
 * The passphrase of the file that keeps secrets where there's no keychain,
 * locked until it's given (Cloud hosts' `hosts_unlock`): Unlock opens it
 * for this session, then `onUnlocked` runs (what was waiting for it, saved
 * now). The passphrase isn't kept; what failed, the unlock or what ran
 * after it, is said under the field.
 */
export function SecretsUnlock(props: { onUnlocked: () => Promise<void> | void }) {
  const [passphrase, setPassphrase] = createSignal('');
  const [error, setError] = createSignal<string | null>(null);

  async function unlock() {
    setError(null);
    try {
      await unlockSecrets(passphrase());
      setPassphrase('');
      await props.onUnlocked();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <>
      <div class="host-unlock">
        <input
          type="password"
          value={passphrase()}
          onInput={(e) => setPassphrase(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && passphrase()) void unlock();
          }}
          placeholder="Passphrase"
          aria-label="passphrase of the secrets file"
        />
        <button
          class="chat-action-button primary"
          disabled={!passphrase()}
          onClick={() => void unlock()}
        >
          Unlock
        </button>
      </div>
      <Show when={error()}>
        <SettingStatus warning>{error()}</SettingStatus>
      </Show>
    </>
  );
}
