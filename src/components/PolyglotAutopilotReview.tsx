import { createEffect, createSignal, For, on, Show } from 'solid-js';
import type { ModelProfile } from '../types';
import type { VerifiedDeliverySettings } from '../lib/autopilotSettings';
import {
  verificationInputProblem,
  type HarnessInspection,
  type HarnessTarget,
} from '../lib/polyglotHarness';
import { harnessProviderProblem } from '../lib/polyglotPlan';
import { formatHarnessRate, savedHarnessRates } from '../lib/harnessRates';
import { openAppSettings } from '../lib/appSettings';
import PolyglotHarnessPanel from './PolyglotHarnessPanel';
import PolyglotReviewFrame from './PolyglotReviewFrame';

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
  const rates = () => savedHarnessRates(props.profile);
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
  createEffect(
    on(
      () =>
        JSON.stringify({
          task: props.request,
          profile: props.profile.id,
          model: props.profile.model,
          endpoint: props.profile.baseUrl,
          rates: rates(),
        }),
      () => setReviewed(false),
      { defer: true }
    )
  );
  return (
    <PolyglotReviewFrame
      title="Review Autopilot task"
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
          Start verified task
        </button>
      }
    >
      <p>
        Execution: {props.executionLabel}. Model: {props.profile.label}.
      </p>
      <p class="settings-hint">
        Uses the project's protected checks and this model's saved prices. You'll review the
        implementation plan before it runs and the changes before applying them.
      </p>
      <details>
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
        <details open={!setup()}>
          <summary>{setup() ? 'Project checks and files' : 'Choose project checks'}</summary>
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
        </details>
      </Show>
      <Show when={setup()}>
        {(selected) => (
          <div class="chat-verify">
            <div>
              <strong>{selected().label}</strong>: {selected().contractSummary}
            </div>
            <Show when={selected().invariants?.length}>
              <ul class="chat-acceptance-criteria">
                <For each={selected().invariants ?? []}>
                  {(invariant) => <li>{invariant.statement}</li>}
                </For>
              </ul>
            </Show>
          </div>
        )}
      </Show>
      <Show when={!rates()}>
        <p role="status" class="settings-status warning">
          Save verification prices for {props.profile.label} in{' '}
          <button type="button" class="link-button" onClick={() => openAppSettings('providers')}>
            Model providers
          </button>{' '}
          before starting. Explicit zero prices are supported for free models.
        </p>
      </Show>
      <Show when={!harnessProviderProblem(props.profile) && rates()}>
        {(saved) => (
          <details>
            <summary>Model and saved prices</summary>
            <p class="settings-hint">
              {props.profile.model} · {props.profile.baseUrl}
            </p>
            <p class="settings-hint">
              Input {formatHarnessRate(saved().input)}, cached input{' '}
              {formatHarnessRate(saved().cachedInput)}, output {formatHarnessRate(saved().output)}{' '}
              in the provider’s billing currency per million tokens. These prices stay fixed for
              this task.
            </p>
          </details>
        )}
      </Show>
      <label>
        <input
          type="checkbox"
          checked={reviewed()}
          disabled={!ready() || !setup() || !rates() || !!problem()}
          onChange={(event) => setReviewed(event.currentTarget.checked)}
        />{' '}
        I reviewed this task and its protected checks.
      </label>
    </PolyglotReviewFrame>
  );
}
