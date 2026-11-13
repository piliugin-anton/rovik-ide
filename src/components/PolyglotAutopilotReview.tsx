import { createEffect, createSignal, For, Show } from 'solid-js';
import type { ModelProfile } from '../types';
import type { VerifiedDeliverySettings } from '../lib/autopilotSettings';
import {
  verificationInputProblem,
  type HarnessInspection,
  type HarnessTarget,
} from '../lib/polyglotHarness';
import { harnessProviderProblem, parseHarnessRates } from '../lib/polyglotPlan';
import PolyglotHarnessPanel from './PolyglotHarnessPanel';
import PolyglotReviewFrame from './PolyglotReviewFrame';
import PolyglotProviderRates, { type ProviderRateField } from './PolyglotProviderRates';

/** Per-task human review freezes only non-secret setup and provider prices. */
export default function PolyglotAutopilotReview(props: {
  target: HarnessTarget;
  profile: ModelProfile;
  request: string;
  executionLabel: string;
  unsavedFiles: readonly string[] | undefined;
  contextProblem?: string | null;
  onStart: (settings: VerifiedDeliverySettings) => void;
  onCancel: () => void;
}) {
  const [inspection, setInspection] = createSignal<HarnessInspection>();
  const [untracked, setUntracked] = createSignal<string[]>([]);
  const [ready, setReady] = createSignal(false);
  const [reviewed, setReviewed] = createSignal(false);
  const [values, setValues] = createSignal<Record<ProviderRateField, string>>({
    input: '',
    cachedInput: '',
    output: '',
  });
  const rates = () => parseHarnessRates(values().input, values().cachedInput, values().output);
  const problem = () =>
    props.contextProblem ??
    verificationInputProblem(props.unsavedFiles) ??
    (props.profile.noTools ? 'Select a model with tools enabled for Autopilot planning.' : null) ??
    harnessProviderProblem(props.profile);
  const setup = () =>
    inspection()?.setups.find((entry) => entry.id === inspection()?.configuration?.setupId);
  const canStart = () =>
    ready() &&
    reviewed() &&
    rates() &&
    inspection()?.host.supported &&
    inspection()?.configuration &&
    !problem();
  function inspected(value: HarnessInspection, selected: string[]) {
    if (
      inspection()?.snapshotDigest !== value.snapshotDigest ||
      inspection()?.configuration?.digest !== value.configuration?.digest ||
      JSON.stringify(selected) !== JSON.stringify(untracked())
    )
      setReviewed(false);
    setInspection(value);
    setUntracked([...selected]);
    setReady(true);
  }
  createEffect(() => {
    if (problem()) {
      setReviewed(false);
      setReady(false);
    }
  });
  return (
    <PolyglotReviewFrame
      title="Review this verified Autopilot task"
      label="Verified Autopilot task setup"
      closeLabel="Cancel task start"
      onClose={props.onCancel}
      actions={
        <button
          type="button"
          class="chat-action-button primary"
          disabled={!canStart()}
          onClick={() => {
            const configuration = inspection()?.configuration;
            const selectedRates = rates();
            if (!canStart() || !configuration || !selectedRates) return;
            props.onStart({
              setupId: configuration.setupId,
              configurationDigest: configuration.digest,
              untracked: [...untracked()],
              rates: { ...selectedRates },
            });
          }}
        >
          Start reviewed verified Autopilot task
        </button>
      }
    >
      <p>
        Execution: {props.executionLabel}. Model: {props.profile.label}.
      </p>
      <p class="settings-hint">
        Model identity: <code>{props.profile.model}</code>.
      </p>
      <Show when={!harnessProviderProblem(props.profile)}>
        <p class="settings-hint">
          Provider endpoint: <code>{props.profile.baseUrl}</code>.
        </p>
      </Show>
      <p class="settings-hint">
        The selected setup, provider identity and prices are frozen for this task. The exact saved
        plan, implementation input, oracle and candidate decisions still need review before the
        strict controller runs. Applying verified changes requires a separate exact delivery
        approval.
      </p>
      <details open>
        <summary>Task request</summary>
        <pre style={{ 'white-space': 'pre-wrap' }}>{props.request}</pre>
      </details>
      <Show when={problem()}>
        {(message) => (
          <p role="alert" class="settings-status warning">
            {message()}
          </p>
        )}
      </Show>
      <Show when={!props.contextProblem}>
        <PolyglotHarnessPanel
          root={props.target.root}
          dialogId={props.target.dialogId}
          executionLabel={props.executionLabel}
          unsavedFiles={props.unsavedFiles}
          setupOnly
          onInspection={inspected}
          onInputPending={() => {
            setReady(false);
            setReviewed(false);
          }}
        />
      </Show>
      <Show when={setup()}>
        {(selected) => (
          <div>
            <p>
              <strong>{selected().label}</strong>: {selected().contractSummary}
            </p>
            <For each={selected().invariants ?? []}>
              {(invariant) => (
                <p>
                  {invariant.statement} · {invariant.owner} · {invariant.case_ids.join(', ')}
                </p>
              )}
            </For>
          </div>
        )}
      </Show>
      <PolyglotProviderRates
        values={values()}
        onChange={(field, value) => {
          setValues({ ...values(), [field]: value });
          setReviewed(false);
        }}
      />
      <Show when={!rates()}>
        <p class="settings-hint">
          Enter all three rates, including zero explicitly, before starting.
        </p>
      </Show>
      <label>
        <input
          type="checkbox"
          checked={reviewed()}
          disabled={!ready() || !setup() || !rates() || !!problem()}
          onChange={(event) => setReviewed(event.currentTarget.checked)}
        />{' '}
        I reviewed this task's protected setup, execution host, model and prices.
      </label>
    </PolyglotReviewFrame>
  );
}
