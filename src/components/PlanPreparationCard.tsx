import { createEffect, createSignal, For, onCleanup, Show, untrack } from 'solid-js';
import type { ModelProfile } from '../types';
import { openAppSettings } from '../lib/appSettings';
import { savedHarnessRates } from '../lib/harnessRates';
import { harnessRequest, type HarnessRun, type HarnessTarget } from '../lib/polyglotHarness';
import {
  preparationProblem,
  preparationProviderProblem,
  startPreparedImplementation,
  type PlanPreparation,
} from '../lib/planPreparation';

/** Preparation runs in the conversation; this card holds only its status and the final decision. */
export default function PlanPreparationCard(props: {
  target: HarnessTarget;
  planPath: string;
  preparationId?: string;
  profile?: ModelProfile;
  unsavedFiles: readonly string[] | undefined;
  contextProblem?: string | null;
  executionLabel: string;
  agentRunning?: boolean;
  onPrepare: (preparationId: string) => Promise<boolean>;
  onStop: () => Promise<void>;
  onPreparation: (preparation: PlanPreparation) => void;
  onStart: (run: HarnessRun) => void;
  onClose: () => void;
}) {
  const [preparation, setPreparation] = createSignal<PlanPreparation>();
  const [busy, setBusy] = createSignal(false);
  const [launching, setLaunching] = createSignal(false);
  const [error, setError] = createSignal('');
  const [poll, setPoll] = createSignal(0);
  const [contextRevision, setContextRevision] = createSignal(0);
  let disposed = false;
  let generation = 0;
  let attempted = false;
  const problem = () =>
    props.contextProblem ?? preparationProblem(props.profile, props.unsavedFiles);
  const status = () => preparation()?.status;
  const providerProblem = () => preparationProviderProblem(preparation(), props.profile);
  const accept = (value: PlanPreparation) => {
    setPreparation(value);
    props.onPreparation(value);
  };
  const active = (scope: number) => !disposed && scope === generation && !props.contextProblem;

  async function launchPreparation(id: string, scope: number) {
    setLaunching(true);
    try {
      const started = await props.onPrepare(id);
      if (active(scope) && !started)
        setError('Return to this plan’s chat and selected model to continue preparation.');
    } catch (cause) {
      if (active(scope)) setError(String(cause));
    } finally {
      if (active(scope)) setLaunching(false);
    }
  }

  async function begin() {
    if (busy() || launching() || problem() || !props.profile || !props.target.dialogId) return;
    const scope = ++generation;
    setBusy(true);
    setError('');
    setPreparation(undefined);
    try {
      const value = await harnessRequest<PlanPreparation>(props.target, {
        action: 'beginPlanPreparation',
        planPath: props.planPath,
        dialogId: props.target.dialogId,
        provider: { profile: props.profile, rates: savedHarnessRates(props.profile)! },
        savedBuffers: true,
        untracked: [],
      });
      if (!active(scope)) return;
      accept(value);
      setBusy(false);
      if (value.status === 'preparing') await launchPreparation(value.preparationId, scope);
    } catch (cause) {
      if (active(scope)) setError(String(cause));
    } finally {
      if (active(scope)) setBusy(false);
    }
  }

  async function refresh() {
    const id = preparation()?.preparationId ?? props.preparationId;
    if (!id || props.contextProblem) return;
    const scope = generation;
    try {
      const value = await harnessRequest<PlanPreparation>(props.target, {
        action: 'preparationStatus',
        preparationId: id,
      });
      if (active(scope) && id === (preparation()?.preparationId ?? props.preparationId)) {
        accept(value);
        setError('');
        if (value.runId) {
          const run = await harnessRequest<HarnessRun>(props.target, {
            action: 'status',
            runId: value.runId,
          });
          if (active(scope)) props.onStart(run);
        }
      }
    } catch (cause) {
      if (active(scope)) setError(String(cause));
    }
  }

  async function continuePreparation() {
    const current = preparation();
    if (!current || busy() || launching() || problem() || providerProblem()) return;
    const scope = generation;
    setError('');
    await launchPreparation(current.preparationId, scope);
  }

  async function stop() {
    const current = preparation();
    if (!current || busy() || props.contextProblem) return;
    const scope = ++generation;
    setBusy(true);
    setLaunching(false);
    try {
      await props.onStop();
      const value = await harnessRequest<PlanPreparation>(props.target, {
        action: 'cancelPreparation',
        preparationId: current.preparationId,
      });
      if (active(scope)) accept(value);
    } catch (cause) {
      if (active(scope)) setError(String(cause));
    } finally {
      if (active(scope)) setBusy(false);
    }
  }

  async function implement() {
    const current = preparation();
    if (!current || !props.profile || busy() || launching() || problem() || props.agentRunning)
      return;
    const scope = ++generation;
    setBusy(true);
    setError('');
    try {
      const run = await startPreparedImplementation(
        props.target,
        current,
        props.profile,
        props.unsavedFiles
      );
      if (active(scope)) props.onStart(run);
    } catch (cause) {
      if (active(scope)) setError(String(cause));
    } finally {
      if (active(scope)) setBusy(false);
    }
  }

  createEffect(() => {
    props.target.root;
    props.target.dialogId;
    props.planPath;
    generation++;
    attempted = false;
    setPreparation(undefined);
    setBusy(false);
    setLaunching(false);
    setError('');
    setContextRevision((value) => value + 1);
  });
  createEffect(() => {
    if (props.contextProblem) {
      generation++;
      setBusy(false);
      setLaunching(false);
    }
  });
  createEffect(() => {
    contextRevision();
    if (props.preparationId && !attempted) {
      attempted = true;
      void untrack(refresh);
    } else if (!attempted && !problem()) {
      attempted = true;
      void untrack(begin);
    }
  });
  createEffect(() => {
    poll();
    props.agentRunning;
    const current = preparation();
    if (
      !current ||
      busy() ||
      props.contextProblem ||
      !['preparing', 'ready'].includes(current.status)
    )
      return;
    const timer = window.setTimeout(
      async () => {
        await refresh();
        if (!disposed) setPoll((value) => value + 1);
      },
      current.status === 'preparing' ? 1500 : 5000
    );
    onCleanup(() => window.clearTimeout(timer));
  });
  onCleanup(() => {
    disposed = true;
    generation++;
  });

  return (
    <section
      class={`chat-verify plan-preparation ${problem() || providerProblem() || error() || ['blocked', 'stale', 'cancelled'].includes(status() ?? '') ? 'needs-approval' : status() === 'ready' ? 'clean' : 'running'}`}
      aria-label="Plan preparation"
    >
      <div class="plan-preparation-heading">
        <strong>
          {status() === 'ready' && !problem() && !providerProblem() && !error()
            ? 'Ready to implement'
            : status() === 'cancelled'
              ? 'Preparation stopped'
              : status() === 'stale'
                ? 'Preparation needs updating'
                : problem() || providerProblem() || error() || status() === 'blocked'
                  ? 'Preparation needs attention'
                  : 'Preparing your plan'}
        </strong>
        <button
          type="button"
          class="link-button"
          disabled={busy()}
          onClick={props.onClose}
          aria-label="Close plan preparation"
        >
          Close
        </button>
      </div>
      <div
        class="plan-preparation-body"
        role="region"
        aria-label="Preparation details"
        tabIndex={0}
      >
        <p class="chat-verify-progress">
          {props.executionLabel} · {props.profile?.label ?? 'Choose a model'}
        </p>
        <Show when={problem() || providerProblem() || error()}>
          <p role="alert">{problem() || providerProblem() || error()}</p>
        </Show>
        <Show when={preparation()?.reason ?? preparation()?.error}>
          <p>{preparation()!.reason ?? preparation()!.error}</p>
        </Show>
        <Show when={!preparation()?.review && !problem()}>
          <p>
            {status() === 'cancelled'
              ? 'Your project is unchanged. Prepare again when you are ready.'
              : 'The agent inspects the project, prepares protected tests and sets up checks in a private workspace. Progress and questions appear in this conversation.'}
          </p>
        </Show>
        <Show when={preparation()?.review}>
          {(review) => (
            <>
              <p>{review().summary}</p>
              <ul class="plan-preparation-requirements">
                <For each={review().requirements}>{(requirement) => <li>{requirement}</li>}</For>
              </ul>
              <p class="chat-verify-progress">
                {review().checks.length} prepared checks · {review().scope.writable.length} allowed
                paths
              </p>
              <For each={review().warnings}>
                {(warning) => <p class="plan-preparation-warning">{warning}</p>}
              </For>
              <details>
                <summary>Checks and change scope</summary>
                <ul>
                  <For each={review().checks}>
                    {(check) => (
                      <li>
                        {check.label}
                        {check.configuration ? ` · ${check.configuration}` : ''}
                      </li>
                    )}
                  </For>
                </ul>
                <p>Allowed changes: {review().scope.writable.join(', ')}</p>
                <p>Protected: {review().scope.protected.join(', ')}</p>
                <Show when={review().budgets}>
                  {(budgets) => (
                    <p>
                      Implementation limits: {budgets().model_calls} model calls ·{' '}
                      {budgets().tokens?.toLocaleString()} tokens · $
                      {((budgets().cost_microunits ?? 0) / 1_000_000).toFixed(2)}. Preparation uses
                      this chat’s model and is billed separately.
                    </p>
                  )}
                </Show>
              </details>
              <p class="chat-verify-progress">
                Implementation runs separately and must pass these checks. You review the resulting
                diff before applying it.
              </p>
            </>
          )}
        </Show>
      </div>
      <div class="plan-preparation-actions">
        <Show when={props.profile && !savedHarnessRates(props.profile)}>
          <button
            type="button"
            class="chat-action-button"
            onClick={() => openAppSettings('providers')}
          >
            Set model prices
          </button>
        </Show>
        <Show when={status() === 'ready'}>
          <button
            type="button"
            class="chat-action-button primary"
            disabled={
              busy() ||
              launching() ||
              !!problem() ||
              !!providerProblem() ||
              !!error() ||
              !!props.agentRunning
            }
            onClick={() => void implement()}
          >
            {busy() ? 'Starting implementation…' : 'Implement plan'}
          </button>
        </Show>
        <Show when={status() === 'preparing'}>
          <Show
            when={props.agentRunning || launching()}
            fallback={
              <button
                type="button"
                class="chat-action-button"
                disabled={busy() || !!problem() || !!providerProblem()}
                onClick={() => void continuePreparation()}
              >
                Continue preparation
              </button>
            }
          >
            <button
              type="button"
              class="chat-action-button"
              disabled={busy() || !!props.contextProblem}
              onClick={() => void stop()}
            >
              Stop preparation
            </button>
          </Show>
        </Show>
        <Show
          when={
            !preparation() ||
            providerProblem() ||
            ['blocked', 'cancelled', 'stale'].includes(status() ?? '')
          }
        >
          <button
            type="button"
            class="chat-action-button"
            disabled={busy() || launching() || !!problem() || !!props.agentRunning}
            onClick={() => void begin()}
          >
            {busy() ? 'Preparing…' : preparation() ? 'Prepare again' : 'Prepare plan'}
          </button>
        </Show>
        <Show when={preparation()}>
          <button
            type="button"
            class="link-button"
            disabled={busy() || !!props.contextProblem}
            onClick={() => void refresh()}
          >
            Refresh
          </button>
        </Show>
      </div>
    </section>
  );
}
