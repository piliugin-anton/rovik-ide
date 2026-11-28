import {
  createMemo,
  createEffect,
  createSignal,
  For,
  on,
  onCleanup,
  Show,
  untrack,
} from 'solid-js';
import VerifiedPlanStatusRow, { type PlanStatus } from './VerifiedPlanStatusRow';
import type { ModelProfile } from '../types';
import { openAppSettings } from '../lib/appSettings';
import { summarizeChatError } from '../lib/chatErrors';
import type { preparationActivity } from '../lib/verifiedPlanProgress';
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
  background?: boolean;
  automatic?: boolean;
  restoreOnly?: boolean;
  target: HarnessTarget;
  planPath: string;
  preparationId?: string;
  /** Retry resumes active drafts, replaces cancelled/stale sessions, or opens their review. */
  retry?: boolean;
  profile?: ModelProfile;
  unsavedFiles: readonly string[] | undefined;
  contextProblem?: string | null;
  executionLabel: string;
  agentRunning?: boolean;
  /** A known request identifies its preparation, or null for an ordinary turn. */
  agentPreparation?: string | null;
  onActiveChange?: (active: boolean) => void;
  preparationActivity?: (id: string) => ReturnType<typeof preparationActivity>;
  onPrepare: (preparationId: string) => Promise<boolean>;
  onStop: () => Promise<void>;
  onPreparation: (preparation: PlanPreparation) => void;
  onStart: (run: HarnessRun) => void;
  onClose: () => void;
}) {
  const [preparation, setPreparation] = createSignal<PlanPreparation>();
  const [authorized, setAuthorized] = createSignal(!props.restoreOnly);
  const [busy, setBusy] = createSignal(false);
  const [launching, setLaunching] = createSignal(false);
  const [actionError, setError] = createSignal('');
  const [readError, setReadError] = createSignal('');
  const error = () => actionError() || readError();
  const [poll, setPoll] = createSignal(0);
  const [contextRevision, setContextRevision] = createSignal(0);
  let disposed = false;
  let generation = 0;
  let statusRequest = 0;
  let attempted = false;
  const problem = () =>
    props.contextProblem ?? preparationProblem(props.profile, props.unsavedFiles);
  const status = () => preparation()?.status;
  const providerProblem = () => preparationProviderProblem(preparation(), props.profile);
  const accept = (value: PlanPreparation) => {
    props.onPreparation(value);
    setPreparation(value);
  };
  const active = (scope: number) => !disposed && scope === generation && !props.contextProblem;

  const [deferredLaunch, setDeferredLaunch] = createSignal<{ id: string; scope: number }>();
  createEffect(() => {
    const pending = deferredLaunch();
    if (props.background || !pending) return;
    setDeferredLaunch(undefined);
    const session = preparation();
    if (
      active(pending.scope) &&
      session?.preparationId === pending.id &&
      !session.runId &&
      ['preparing', 'blocked'].includes(session.status) &&
      !props.agentRunning &&
      !problem() &&
      !providerProblem()
    )
      void untrack(() => launchPreparation(pending.id, pending.scope));
  });

  async function launchPreparation(id: string, scope: number) {
    if (props.background) {
      setDeferredLaunch({ id, scope });
      return;
    }
    setAuthorized(true);
    setLaunching(true);
    try {
      const started = await props.onPrepare(id);
      if (active(scope) && !started)
        setError(
          props.preparationActivity?.(id)?.failure ||
            'Return to this plan’s chat and selected model to continue preparation.'
        );
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
    setReadError('');
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
    const request = ++statusRequest;
    const current = () => active(scope) && request === statusRequest;
    try {
      const value = await harnessRequest<PlanPreparation>(props.target, {
        action: 'preparationStatus',
        preparationId: id,
      });
      if (current() && id === (preparation()?.preparationId ?? props.preparationId)) {
        accept(value);
        setReadError('');
        if (value.runId) {
          const run = await harnessRequest<HarnessRun>(props.target, {
            action: 'status',
            runId: value.runId,
          });
          if (current()) props.onStart(run);
        }
        return value;
      }
    } catch (cause) {
      if (current()) setReadError(String(cause));
    }
  }

  async function openPreparation(retry = false) {
    if (busy() || props.contextProblem) return;
    const scope = generation;
    setBusy(true);
    setReadError('');
    const value = await refresh();
    if (!active(scope)) return;
    setBusy(false);
    if (!retry || !value || value.runId || props.agentRunning || problem()) return;
    const resumable = ['preparing', 'blocked'].includes(value.status);
    if (['cancelled', 'stale'].includes(value.status) || (resumable && providerProblem()))
      await begin();
    else if (resumable) await launchPreparation(value.preparationId, scope);
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
    setReadError('');
    setPreparation(undefined);
    setBusy(false);
    setLaunching(false);
    setError('');
    setContextRevision((value) => value + 1);
  });
  createEffect(
    on(
      () => props.contextProblem,
      (issue, previous) => {
        if (issue) {
          generation++;
          setBusy(false);
          setLaunching(false);
          if (attempted) setAuthorized(false);
        } else if (previous && attempted && (preparation() || props.preparationId)) {
          // A request may have reached its host before the project or host changed.
          void openPreparation();
        }
      }
    )
  );
  createEffect(() => {
    contextRevision();
    if (props.contextProblem) return;
    if (props.preparationId && !attempted) {
      attempted = true;
      void untrack(() => openPreparation(props.retry));
    } else if (!attempted && !problem()) {
      attempted = true;
      void untrack(begin);
    }
  });
  createEffect(
    on(
      () => props.agentRunning,
      (running, previous) => {
        // Read the final host result even if the last poll saw a blocked preparation.
        if (previous && !running && preparation() && !busy() && !launching())
          void openPreparation();
      }
    )
  );
  createEffect(() => {
    poll();
    const current = preparation();
    if (
      !current ||
      busy() ||
      props.contextProblem ||
      !(
        ['preparing', 'ready'].includes(current.status) ||
        (current.status === 'blocked' && props.agentRunning)
      )
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

  let promoted: string | undefined;
  createEffect(() => {
    const current = preparation();
    if (
      !props.automatic ||
      !authorized() ||
      current?.status !== 'ready' ||
      current.runId ||
      !current.review ||
      busy() ||
      launching() ||
      props.agentRunning ||
      problem() ||
      providerProblem() ||
      error()
    )
      return;
    const key = current.preparationId + current.review.digest;
    if (promoted === key) return;
    promoted = key;
    void untrack(implement);
  });

  const row = createMemo<PlanStatus>(() => {
    const issue = problem() || providerProblem() || error();
    const current = preparation();
    const progress = current && props.preparationActivity?.(current.preparationId);
    const failure = issue || progress?.failure || current?.error || current?.reason;
    const summary = failure && summarizeChatError(failure);
    const statusReason =
      progress?.shownInChat && progress.failure === failure && summary !== failure
        ? 'See error in chat.'
        : summary;
    const actions = [];
    const otherTurn =
      props.agentRunning &&
      props.agentPreparation !== undefined &&
      props.agentPreparation !== current?.preparationId;
    const agentWorking = launching() || (props.agentRunning && !otherTurn);
    if (
      issue ||
      ['stale', 'cancelled'].includes(status() ?? '') ||
      (status() === 'blocked' && !agentWorking)
    ) {
      if (props.profile && !savedHarnessRates(props.profile))
        actions.push({
          label: 'Set model prices',
          run: () => openAppSettings('providers'),
        });
      else if (!problem() && !busy() && !launching() && !props.agentRunning)
        actions.push({
          label: 'Retry',
          run: () => {
            if (!current && props.preparationId) void openPreparation(props.retry);
            else if (current?.runId || readError()) void openPreparation();
            else if (status() === 'ready' && !providerProblem()) void implement();
            else if (
              current &&
              ['preparing', 'blocked'].includes(current.status) &&
              !providerProblem()
            )
              void continuePreparation();
            else void begin();
          },
        });

      return {
        title: 'Plan preparation needs attention',
        attention: true,

        onDismiss: props.onClose,
        message: statusReason || 'Prepare the plan again to continue.',
        actions,
      };
    }
    if (otherTurn && !busy() && !launching())
      return {
        title: 'Preparation paused',
        message: 'Another request is running in this chat.',
        onDismiss: props.onClose,
      };
    if (status() === 'ready' && !current?.runId && !busy() && !agentWorking)
      return {
        title: 'Ready to implement',
        message: current?.review?.summary,
        onDismiss: props.onClose,
        actions: [
          {
            label: 'Implement plan',
            primary: true,
            run: () => void implement(),
          },
        ],
      };
    if (
      !busy() &&
      !launching() &&
      (!current || (current.status === 'preparing' && !props.agentRunning))
    )
      return {
        title: 'Preparation paused',
        attention: true,
        onDismiss: props.onClose,
        message:
          statusReason ||
          (current
            ? 'The agent stopped before the checks were ready.'
            : 'Preparation stopped before the project snapshot was available.'),
        actions: [
          {
            label: 'Continue',
            run: () => {
              if (current) void continuePreparation();
              else if (props.preparationId) void openPreparation(true);
              else void begin();
            },
          },
        ],
      };
    return {
      title: status() === 'ready' ? 'Starting implementation' : 'Preparing checks',

      message:
        status() === 'ready'
          ? 'Starting the verified run'
          : current && busy() && !agentWorking
            ? 'Checking the preparation result'
            : launching() && current?.status === 'blocked'
              ? 'Retrying preparation'
              : (progress?.failure ? statusReason : progress?.activity || statusReason) ||
                (!current
                  ? props.preparationId
                    ? 'Opening the saved preparation'
                    : 'Taking a snapshot of the project'
                  : 'Waiting for the preparation agent'),
      attention: !!progress?.failure && !launching(),
      busy: true,
      actions:
        current && !busy() && status() === 'preparing'
          ? [{ label: 'Stop', run: () => void stop() }]
          : [],
    };
  });

  // The editor follows the same transition as the chat, including while the row is hidden.
  createEffect(() => props.onActiveChange?.(!!row().busy));
  if (props.automatic)
    return <VerifiedPlanStatusRow status={props.background ? undefined : row()} />;
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
                  : status() === 'preparing' && !busy() && !launching() && !props.agentRunning
                    ? 'Preparation paused'
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
          <Show when={props.profile && savedHarnessRates(props.profile)}>
            {' · '}
            <button type="button" class="link-button" onClick={() => openAppSettings('providers')}>
              Review model prices
            </button>
          </Show>
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
