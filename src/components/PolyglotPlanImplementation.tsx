import { createEffect, createSignal, For, onCleanup, Show, untrack } from 'solid-js';
import type { ModelProfile } from '../types';
import {
  displayedHarnessStatus,
  harnessRequest,
  verificationInputProblem,
  type HarnessApproval,
  type HarnessOperation,
  type HarnessRun,
  type HarnessTarget,
  type HarnessProviderBinding,
} from '../lib/polyglotHarness';
import {
  harnessProviderProblem,
  boundHarnessProviderProblem,
  type HarnessChangeReview,
  type HarnessPlan,
  type HarnessPlanOperation,
} from '../lib/polyglotPlan';
import PolyglotApproval, { SubjectFields } from './PolyglotApproval';
import PolyglotReviewFrame from './PolyglotReviewFrame';
import PlanPreparationCard from './PlanPreparationCard';
import { savedHarnessRates } from '../lib/harnessRates';
import type { PlanPreparation } from '../lib/planPreparation';

/** Reviewed saved-plan implementation, independent of the ordinary agent's repair loop. */
export default function PolyglotPlanImplementation(props: {
  target: HarnessTarget;
  planPath?: string;
  /** Reopen the existing controller run, including an Autopilot-owned run. */
  runId?: string;
  profile?: ModelProfile;
  preparationId?: string;
  agentRunning?: boolean;
  onPrepare?: (preparationId: string) => Promise<boolean>;
  onStopPreparation?: () => Promise<void>;
  onPreparationChange?: (preparation: PlanPreparation) => void;
  /** Resolve current credentials for the persisted identity without persisting secrets. */
  resolveProvider?: (binding: HarnessProviderBinding) => ModelProfile | undefined;
  unsavedFiles: readonly string[] | undefined;
  executionLabel: string;
  contextProblem?: string | null;
  onClose: () => void;
  onActiveChange?: (active: boolean) => void;
  onRunChange?: (run: HarnessRun) => void;
}) {
  const [plan, setPlan] = createSignal<HarnessPlan>();
  const [preparedSnapshot, setPreparedSnapshot] = createSignal<string>();
  const [restart, setRestart] = createSignal(false);
  const [run, setRun] = createSignal<HarnessRun>();
  const [approval, setApproval] = createSignal<HarnessApproval>();
  const [review, setReview] = createSignal<HarnessChangeReview>();
  const [exported, setExported] = createSignal<unknown>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [fresh, setFresh] = createSignal(false);
  const [releaseReviewed, setReleaseReviewed] = createSignal<string>();
  const [pollRevision, setPollRevision] = createSignal(0);
  let generation = 0;
  let requestNumber = 0;
  let disposed = false;
  const rates = () => run()?.providerBinding?.rates ?? savedHarnessRates(currentProfile());
  const currentProfile = () => {
    const binding = run()?.providerBinding;
    return binding && props.resolveProvider ? props.resolveProvider(binding) : props.profile;
  };
  const provider = () => ({ profile: currentProfile()!, rates: rates()! });
  const inputProblem = () => props.contextProblem ?? verificationInputProblem(props.unsavedFiles);
  const unavailable = () => busy() || !!props.contextProblem;
  const providerProblem = () => {
    const binding = run()?.providerBinding;
    return binding
      ? boundHarnessProviderProblem(binding, currentProfile())
      : props.profile
        ? harnessProviderProblem(props.profile)
        : 'Choose the configured provider to make model calls.';
  };
  const shownStatus = () => {
    const current = run();
    if (!current) return '';
    return ['verified', 'applied'].includes(current.status) && (!fresh() || !!props.contextProblem)
      ? 'stale'
      : displayedHarnessStatus(current, props.unsavedFiles, preparedSnapshot());
  };

  async function request<T>(
    operation: HarnessOperation | HarnessPlanOperation,
    accept: (value: T) => void
  ) {
    if (unavailable()) return;
    const scope = generation;
    const number = ++requestNumber;
    setBusy(true);
    setError('');
    try {
      const value = await harnessRequest<T>(props.target, operation);
      if (!disposed && scope === generation && number === requestNumber) accept(value);
    } catch (cause) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(cause));
        setFresh(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setBusy(false);
    }
  }
  const acceptRun = (value: HarnessRun) => {
    setRun(value);
    setApproval(undefined);
    setFresh(true);
    setReview(undefined);
    setReleaseReviewed(undefined);
    props.onRunChange?.(value);
  };
  const runAction = (operation: HarnessOperation | HarnessPlanOperation) =>
    request<HarnessRun>(operation, acceptRun);
  createEffect(() => {
    props.target.root;
    props.target.dialogId;
    props.planPath;
    props.runId;
    generation++;
    requestNumber++;
    setRun(undefined);
    setPreparedSnapshot(undefined);
    setApproval(undefined);
    setReview(undefined);
    setFresh(false);
    setBusy(false);
    if (props.runId) void untrack(() => runAction({ action: 'status', runId: props.runId! }));
  });
  createEffect(() => {
    if (props.contextProblem) {
      generation++;
      requestNumber++;
      setBusy(false);
      setFresh(false);
    }
  });
  createEffect(() => props.onActiveChange?.(busy() || run()?.status === 'running'));
  createEffect(() => {
    pollRevision();
    const current = run();
    if (unavailable() || !current || !['running', 'verified', 'applied'].includes(current.status))
      return;
    const scope = generation;
    const number = requestNumber;
    const timer = window.setTimeout(
      async () => {
        try {
          const value = await harnessRequest<HarnessRun>(props.target, {
            action: 'status',
            runId: current.runId,
          });
          if (!disposed && scope === generation && number === requestNumber) {
            setRun(value);
            setFresh(true);
            setError('');
            props.onRunChange?.(value);
          }
        } catch (cause) {
          if (!disposed && scope === generation && number === requestNumber) {
            setError(String(cause));
            setFresh(false);
          }
        } finally {
          if (!disposed && scope === generation) setPollRevision((revision) => revision + 1);
        }
      },
      current.status === 'running' ? 1500 : 5000
    );
    onCleanup(() => window.clearTimeout(timer));
  });
  onCleanup(() => {
    disposed = true;
    generation++;
    requestNumber++;
  });

  return (
    <Show
      when={run() || props.runId}
      fallback={
        <PlanPreparationCard
          target={props.target}
          planPath={props.planPath ?? ''}
          preparationId={restart() ? undefined : props.preparationId}
          profile={props.profile}
          unsavedFiles={props.unsavedFiles}
          contextProblem={props.contextProblem}
          executionLabel={props.executionLabel}
          agentRunning={props.agentRunning}
          onPrepare={props.onPrepare ?? (() => Promise.resolve(false))}
          onStop={props.onStopPreparation ?? (() => Promise.resolve())}
          onPreparation={(value) => {
            setPlan(value.plan);
            setPreparedSnapshot(value.snapshotDigest);
            props.onPreparationChange?.(value);
          }}
          onStart={acceptRun}
          onClose={props.onClose}
        />
      }
    >
      <PolyglotReviewFrame
        title={props.runId ? 'Verified Autopilot task' : 'Plan implementation'}
        label="Verified plan implementation"
        closeLabel="Close verified implementation"
        closeDisabled={!props.contextProblem && (busy() || run()?.status === 'running')}
        onClose={props.onClose}
        actions={
          <>
            <Show when={run()}>
              {(current) => (
                <Show when={current().canCancel}>
                  <button
                    type="button"
                    class="link-button"
                    disabled={unavailable()}
                    onClick={() => void runAction({ action: 'cancel', runId: current().runId })}
                  >
                    Cancel implementation
                  </button>
                </Show>
              )}
            </Show>
          </>
        }
      >
        <p class="settings-hint">
          Execution: {props.executionLabel}. Model:{' '}
          {currentProfile()?.label ?? run()?.providerBinding?.model ?? 'Unavailable'}. The
          controller generates a separate candidate and only accepts it after the reviewed checks
          pass. Applying changes to your project is a separate review.
        </p>
        <Show when={error()}>
          <p role="alert" class="settings-status warning">
            {error()}
          </p>
        </Show>
        <Show when={providerProblem()}>
          <p role="status" class="settings-status warning">
            {providerProblem()}
          </p>
        </Show>
        <Show when={inputProblem()}>
          <p role="status" class="settings-status warning">
            {inputProblem()}
          </p>
        </Show>
        <Show when={plan()}>
          {(saved) => (
            <details>
              <summary>Exact saved plan: {saved().path}</summary>
              <pre style={{ 'white-space': 'pre-wrap', 'overflow-wrap': 'anywhere' }}>
                {saved().content}
              </pre>
              <p class="settings-hint">
                Plan identity: <code>{saved().digest}</code>
              </p>
            </details>
          )}
        </Show>
        <Show when={run()}>
          {(current) => (
            <section aria-label="Verified implementation result">
              <p role="status" class="settings-status">
                {shownStatus() === 'applied'
                  ? current().applicationReceipt?.deliveredLocally
                    ? 'Applied to local checkout'
                    : 'Applied on execution host'
                  : shownStatus() === 'verified'
                    ? 'Accepted — verified candidate'
                    : `Implementation ${shownStatus()}`}
              </p>
              <p class="settings-hint">
                Run {current().runId} · {current().state}
              </p>
              <Show when={shownStatus() === 'stale'}>
                <p class="settings-status warning">
                  Current inputs are changed or could not be confirmed. This result does not verify
                  the current project.
                </p>
              </Show>
              <Show when={current().reason}>
                <p>{current().reason}</p>
              </Show>
              <For each={current().reasons?.filter((reason) => reason !== current().reason) ?? []}>
                {(reason) => <p>{reason}</p>}
              </For>
              <For each={current().checks ?? []}>
                {(check) => (
                  <details>
                    <summary>
                      {check.check_id}: {check.status} · {check.executed_test_count} cases
                    </summary>
                    <p>{check.failed_case_ids.join(', ')}</p>
                    <For each={check.diagnostics}>
                      {(diagnostic) => (
                        <p>
                          {diagnostic.rule_id}: {diagnostic.message} {diagnostic.file}{' '}
                          {diagnostic.line}
                        </p>
                      )}
                    </For>
                  </details>
                )}
              </For>
              <Show when={current().providerBinding}>
                <details>
                  <summary>Bound provider identity</summary>
                  <SubjectFields value={current().providerBinding} />
                </details>
              </Show>
              <Show when={current().applicationReceipt}>
                {(receipt) => (
                  <div>
                    <p>
                      Application target: {receipt().location} · {receipt().root}
                    </p>
                    <Show when={!receipt().deliveredLocally}>
                      <p>Desktop delivery remains a separate step.</p>
                    </Show>
                    <Show when={!receipt().current}>
                      <p class="settings-status warning">
                        This receipt is historical; the current checkout no longer matches it.
                      </p>
                    </Show>
                    <details>
                      <summary>Application receipt</summary>
                      <SubjectFields value={receipt()} />
                    </details>
                    <Show
                      when={receipt().location === 'host' && !receipt().synchronizationReleased}
                    >
                      <label>
                        <input
                          type="checkbox"
                          checked={releaseReviewed() === receipt().subjectDigest}
                          onChange={(event) =>
                            setReleaseReviewed(
                              event.currentTarget.checked ? receipt().subjectDigest : undefined
                            )
                          }
                        />{' '}
                        I have preserved or manually integrated these host changes.
                      </label>
                      <button
                        type="button"
                        class="link-button"
                        disabled={unavailable() || releaseReviewed() !== receipt().subjectDigest}
                        onClick={() =>
                          void runAction({
                            action: 'releaseApplication',
                            runId: current().runId,
                            subjectDigest: receipt().subjectDigest,
                            acknowledged: true,
                          })
                        }
                      >
                        Allow synchronization
                      </button>
                    </Show>
                  </div>
                )}
              </Show>
              <For each={current().pendingApprovals}>
                {(item) => (
                  <button
                    type="button"
                    class="link-button"
                    disabled={unavailable()}
                    onClick={() => setApproval(item)}
                  >
                    Review {item.approvalType}
                  </button>
                )}
              </For>
              <Show when={approval()}>
                {(item) => (
                  <section aria-label="Implementation approval">
                    <p>{item().summary}</p>
                    <PolyglotApproval approval={item()} />
                    <p class="settings-hint">
                      Subject <code>{item().subjectDigest}</code> · Scope{' '}
                      <code>{item().scopeDigest}</code>
                    </p>
                    <button
                      type="button"
                      class="chat-action-button"
                      disabled={unavailable() || item().subject == null}
                      onClick={() =>
                        void runAction({
                          action: 'approve',
                          runId: current().runId,
                          request: item().request,
                        })
                      }
                    >
                      Approve {item().approvalType}
                    </button>
                    <button
                      type="button"
                      class="link-button"
                      onClick={() => setApproval(undefined)}
                    >
                      Close approval review
                    </button>
                  </section>
                )}
              </Show>
              <Show when={current().canResume}>
                <button
                  type="button"
                  class="chat-action-button"
                  disabled={unavailable() || !rates() || !!providerProblem() || !!inputProblem()}
                  onClick={() =>
                    void runAction({
                      action: 'resumeImplementation',
                      runId: current().runId,
                      provider: provider(),
                    })
                  }
                >
                  Resume verified implementation
                </button>
              </Show>
              <Show when={current().canRecover}>
                <button
                  type="button"
                  class="link-button"
                  disabled={unavailable()}
                  onClick={() => void runAction({ action: 'recover', runId: current().runId })}
                >
                  Recover interrupted implementation
                </button>
              </Show>
              <button
                type="button"
                class="link-button"
                disabled={unavailable()}
                onClick={() => void runAction({ action: 'status', runId: current().runId })}
              >
                Refresh implementation result
              </button>
              <button
                type="button"
                class="chat-action-button"
                disabled={unavailable() || !current().candidateDigest}
                onClick={() =>
                  void request<HarnessChangeReview>(
                    { action: 'reviewChanges', runId: current().runId },
                    setReview
                  )
                }
              >
                {shownStatus() === 'verified'
                  ? 'Review verified changes'
                  : 'Review candidate changes'}
              </button>
              <button
                type="button"
                class="link-button"
                disabled={unavailable() || !current().candidateDigest}
                onClick={() =>
                  void request<unknown>(
                    { action: 'exportReview', runId: current().runId },
                    setExported
                  )
                }
              >
                Export review bundle
              </button>
              <button
                type="button"
                class="chat-action-button"
                disabled={
                  unavailable() ||
                  shownStatus() !== 'verified' ||
                  !!inputProblem() ||
                  review()?.runId !== current().runId
                }
                onClick={() =>
                  void request<HarnessRun>(
                    { action: 'prepareApplication', runId: current().runId },
                    (value) => {
                      setRun(value);
                      setFresh(true);
                      setApproval(undefined);
                      props.onRunChange?.(value);
                    }
                  )
                }
              >
                Prepare application review
              </button>
              <Show when={current().application}>
                {(application) => (
                  <section aria-label="Reviewed application">
                    <SubjectFields value={application().subject} />
                    <p>
                      Approve the exact delivery subject above, then apply it to the named checkout.
                    </p>
                    <button
                      type="button"
                      class="chat-action-button primary"
                      disabled={
                        unavailable() ||
                        !!inputProblem() ||
                        shownStatus() !== 'verified' ||
                        current().pendingApprovals.some((item) => item.approvalType === 'delivery')
                      }
                      onClick={() =>
                        void runAction({
                          action: 'applyApplication',
                          runId: current().runId,
                          subjectDigest: application().subjectDigest,
                          savedBuffers: true,
                        })
                      }
                    >
                      Apply reviewed verified changes
                    </button>
                    <Show when={['blocked', 'stale'].includes(current().status)}>
                      <button
                        type="button"
                        class="link-button"
                        disabled={unavailable() || !!inputProblem()}
                        onClick={() =>
                          void runAction({
                            action: 'recoverApplication',
                            runId: current().runId,
                            subjectDigest: application().subjectDigest,
                            savedBuffers: true,
                          })
                        }
                      >
                        Recover interrupted application
                      </button>
                    </Show>
                  </section>
                )}
              </Show>
              <Show when={!props.runId && current().status !== 'running'}>
                <button
                  type="button"
                  class="link-button"
                  disabled={unavailable()}
                  onClick={() => {
                    setRun(undefined);
                    setRestart(true);
                  }}
                >
                  Prepare a new implementation
                </button>
              </Show>
            </section>
          )}
        </Show>
        <Show when={review()}>
          {(value) => (
            <details open>
              <summary>Candidate change review</summary>
              <SubjectFields value={value()} />
            </details>
          )}
        </Show>
        <Show when={exported()}>
          {(value) => (
            <div role="status">
              <p>Review bundle saved on the execution host.</p>
              <SubjectFields value={value()} />
            </div>
          )}
        </Show>
      </PolyglotReviewFrame>
    </Show>
  );
}
