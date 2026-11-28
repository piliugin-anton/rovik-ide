import VerificationSummary, { approvalDecision, verificationMessage } from './VerificationSummary';
import { implementationProgress, type preparationActivity } from '../lib/verifiedPlanProgress';
import { createMemo, createEffect, createSignal, For, onCleanup, Show, untrack } from 'solid-js';
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
import VerifiedChangesDialog from './VerifiedChangesDialog';
import VerifiedPlanStatusRow, { type PlanStatus } from './VerifiedPlanStatusRow';
import PolyglotReviewFrame from './PolyglotReviewFrame';
import PlanPreparationCard from './PlanPreparationCard';
import { savedHarnessRates } from '../lib/harnessRates';
import type { PlanPreparation } from '../lib/planPreparation';

/** Reviewed saved-plan implementation, independent of the ordinary agent's repair loop. */
export default function PolyglotPlanImplementation(props: {
  background?: boolean;
  automatic?: boolean;
  restoreOnly?: boolean;
  target: HarnessTarget;
  planPath?: string;
  /** Reopen the existing controller run, including an Autopilot-owned run. */
  runId?: string;
  profile?: ModelProfile;
  preparationId?: string;
  /** An explicit chat Retry checks the saved session before resuming preparation. */
  retryPreparation?: boolean;
  agentRunning?: boolean;
  agentPreparation?: string | null;
  onPreparationActive?: (active: boolean) => void;
  preparationActivity?: (id: string) => ReturnType<typeof preparationActivity>;
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
  const preparationId = () => (restart() ? undefined : props.preparationId);
  const [run, setRun] = createSignal<HarnessRun>();
  const [approval, setApproval] = createSignal<HarnessApproval>();
  const [review, setReview] = createSignal<HarnessChangeReview>();
  const [exported, setExported] = createSignal<{ path?: string }>();
  const [busy, setBusy] = createSignal(false);
  const [actionError, setError] = createSignal('');
  const [readError, setReadError] = createSignal('');
  const error = () => actionError() || readError();
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
    setReadError('');
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
    setReadError('');
    setError('');
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
            setReadError('');
            props.onRunChange?.(value);
          }
        } catch (cause) {
          if (!disposed && scope === generation && number === requestNumber) {
            setReadError(String(cause));
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

  const [details, setDetails] = createSignal(false);
  createEffect(() => {
    if (props.automatic && (run()?.status === 'running' || run()?.status === 'verified'))
      setDetails(false);
  });
  const [reviewTarget, setReviewTarget] = createSignal('');
  const [reviewSubject, setReviewSubject] = createSignal<string>();
  const matchingApplication = (value: HarnessRun | undefined, candidate?: string) => {
    const subject = value?.application?.subject as
      { candidateDigest?: string; root?: string } | undefined;
    return candidate &&
      value?.candidateDigest === candidate &&
      subject?.candidateDigest === candidate
      ? subject.root
      : undefined;
  };

  const [previewDesign, setPreviewDesign] = createSignal<HarnessApproval>();
  function previewChanges(item?: HarnessApproval) {
    const current = run();
    if (!current?.candidateDigest || (item && current.candidateDigest !== item.subjectDigest)) {
      setError('The proposed changes have changed. Refresh the run before approving the design.');
      return;
    }
    void request<HarnessChangeReview>(
      { action: 'reviewChanges', runId: current.runId },
      (changes) => {
        if (
          changes.runId !== current.runId ||
          changes.candidateDigest !== current.candidateDigest ||
          !Array.isArray(changes.files)
        )
          throw new Error('The proposed changes could not be confirmed. Open the review again.');
        setReviewSubject(undefined);
        setPreviewDesign(item);
        setExported(undefined);
        setReview(changes);
      }
    );
  }
  const designApproval = () => {
    const item = previewDesign();
    return (
      item &&
      shownStatus() !== 'stale' &&
      run()?.pendingApprovals.find(
        (pending) =>
          pending.approvalType === 'design-review' &&
          pending.subjectDigest === item.subjectDigest &&
          pending.subjectDigest === review()?.candidateDigest &&
          pending.subjectDigest === run()?.candidateDigest &&
          pending.subject != null
      )
    );
  };
  async function openChanges() {
    const current = run();
    if (!current || unavailable() || inputProblem() || shownStatus() !== 'verified') return;
    const scope = generation;
    const candidate = current.candidateDigest;
    const target = props.target;
    requestNumber++;
    setBusy(true);
    setError('');
    try {
      const changes = await harnessRequest<HarnessChangeReview>(target, {
        action: 'reviewChanges',
        runId: current.runId,
      });
      if (scope !== generation || disposed || inputProblem()) return;
      if (!Array.isArray(changes.files))
        throw new Error('The file review is unavailable. Open the review again.');
      if (!candidate || changes.candidateDigest !== candidate || changes.runId !== current.runId)
        throw new Error('The verified changes changed. Open the review again.');
      const prepared = await harnessRequest<HarnessRun>(target, {
        action: 'prepareApplication',
        runId: current.runId,
      });
      if (scope !== generation || disposed || inputProblem()) return;
      const destination = matchingApplication(prepared, candidate);
      if (!destination || prepared.runId !== current.runId || prepared.status !== 'verified')
        throw new Error('The checkout changed. Refresh the verified result.');
      acceptRun(prepared);
      setExported(undefined);
      setReviewTarget(`${props.executionLabel} · ${destination}`);
      setReviewSubject(prepared.application?.subjectDigest);
      setReview(changes);
    } catch (cause) {
      if (scope === generation && !disposed) setError(String(cause));
    } finally {
      if (scope === generation && !disposed) setBusy(false);
    }
  }

  async function applyChanges() {
    const current = run();
    const changes = review();
    const subject = current?.application?.subjectDigest;
    if (
      !current ||
      !subject ||
      subject !== reviewSubject() ||
      !changes ||
      changes.runId !== current.runId ||
      !matchingApplication(current, changes.candidateDigest) ||
      unavailable() ||
      inputProblem() ||
      shownStatus() !== 'verified'
    )
      return;
    const scope = generation;
    requestNumber++;
    setBusy(true);
    setError('');
    try {
      const delivery = current.pendingApprovals.find(
        (item) => item.approvalType === 'delivery' && item.subjectDigest === subject
      );
      if (delivery) {
        const approved = await harnessRequest<HarnessRun>(props.target, {
          action: 'approve',
          runId: current.runId,
          request: delivery.request,
        });
        if (disposed || scope !== generation || inputProblem()) return;
        if (
          approved.runId !== current.runId ||
          approved.application?.subjectDigest !== subject ||
          approved.status !== 'verified' ||
          approved.pendingApprovals.length
        )
          throw new Error('The approval changed. Open the review again.');
      }
      const result = await harnessRequest<HarnessRun>(props.target, {
        action: 'applyApplication',
        runId: current.runId,
        subjectDigest: subject,
        savedBuffers: true,
      });
      if (!disposed && scope === generation) acceptRun(result);
    } catch (cause) {
      if (!disposed && scope === generation) {
        setReview(undefined);
        setError(String(cause));
      }
    } finally {
      if (!disposed && scope === generation) setBusy(false);
    }
  }

  function restartPlan() {
    setReadError('');
    setError('');
    setDetails(false);
    setRun(undefined);
    setRestart(true);
  }
  const restartInFooter = () =>
    props.automatic && !props.runId && ['stale', 'failed'].includes(shownStatus());

  const row = createMemo<PlanStatus | undefined>(() => {
    const current = run();
    if (props.background || !props.automatic || details() || review()) return;
    const problem = inputProblem() || error();
    const id = current?.runId ?? props.runId;
    if (problem)
      return {
        title: 'Implementation needs attention',
        attention: true,
        message: problem,

        onDismiss: props.onClose,
        actions:
          !props.contextProblem && !busy() && id
            ? [
                {
                  label: 'Refresh',
                  run: () => void runAction({ action: 'status', runId: id }),
                },
              ]
            : [],
      };
    if (!current) return { title: 'Loading implementation status', busy: true };
    if (shownStatus() === 'verified')
      return {
        title: 'Verified changes are ready',

        onDismiss: props.onClose,
        actions: busy()
          ? []
          : [
              {
                label: 'Review changes',
                primary: true,
                run: () => void openChanges(),
              },
            ],
      };
    if (shownStatus() === 'running')
      return {
        title: 'Implementing',
        message: implementationProgress(current),

        busy: true,
        actions:
          current.canCancel && !busy()
            ? [
                {
                  label: 'Stop',
                  run: () => void runAction({ action: 'cancel', runId: current.runId }),
                },
              ]
            : [],
      };
    if (
      current.canResume &&
      !current.canRecover &&
      !current.pendingApprovals.length &&
      !providerProblem() &&
      rates()
    )
      return {
        title: 'Implementation paused',
        attention: true,
        message: implementationProgress(current),
        onDismiss: props.onClose,
        actions: busy()
          ? []
          : [
              {
                label: 'Resume',
                primary: true,

                run: () =>
                  void runAction({
                    action: 'resumeImplementation',
                    runId: current.runId,
                    provider: provider(),
                  }),
              },
            ],
      };
    if (shownStatus() === 'applied' && current.applicationReceipt?.deliveredLocally)
      return {
        title: 'Verified changes applied',
        message: current.applicationReceipt.root,

        onDismiss: props.onClose,
      };
    if (shownStatus() === 'cancelled')
      return {
        title: 'Implementation stopped',

        onDismiss: props.onClose,
      };
    const decision =
      current.pendingApprovals.length === 1 ? current.pendingApprovals[0] : undefined;
    const design =
      decision?.approvalType === 'design-review' &&
      decision.subjectDigest === current.candidateDigest;
    return {
      attention: shownStatus() !== 'applied',
      title:
        shownStatus() === 'applied'
          ? 'Changes applied on the host'
          : shownStatus() === 'failed'
            ? 'Verification failed'
            : 'Implementation needs attention',
      message:
        current.pendingApprovals.length || !implementationProgress(current)
          ? verificationMessage(current, shownStatus())
          : implementationProgress(current).split('\n')[0].slice(0, 300),
      onDismiss: props.onClose,
      actions: [
        {
          label: design
            ? 'Review proposed design'
            : current.pendingApprovals.length
              ? 'Review decision'
              : 'View details',
          run: () => {
            setDetails(true);
            if (design) previewChanges(decision);
            else if (decision) setApproval(decision);
          },
        },
      ],
    };
  });

  return (
    <Show
      when={run() || props.runId}
      fallback={
        <PlanPreparationCard
          background={props.background}
          automatic={props.automatic}
          restoreOnly={props.restoreOnly}
          target={props.target}
          planPath={props.planPath ?? ''}
          preparationId={preparationId()}
          retry={props.retryPreparation}
          profile={props.profile}
          unsavedFiles={props.unsavedFiles}
          contextProblem={props.contextProblem}
          executionLabel={props.executionLabel}
          agentRunning={props.agentRunning}
          agentPreparation={props.agentPreparation}
          preparationActivity={props.preparationActivity}
          onActiveChange={props.onPreparationActive}
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
      <Show
        when={!props.background && (!props.automatic || (details() && !review()))}
        fallback={
          <Show
            when={!props.background && review()}
            fallback={<VerifiedPlanStatusRow status={row()} />}
          >
            {(changes) => (
              <VerifiedChangesDialog
                preview={!reviewSubject()}
                onApproveDesign={
                  !reviewSubject() && designApproval()
                    ? () => {
                        const item = designApproval();
                        if (item && !inputProblem())
                          void runAction({
                            action: 'approve',
                            runId: changes().runId,
                            request: item.request,
                          });
                      }
                    : undefined
                }
                review={changes()}
                destination={
                  !reviewSubject()
                    ? previewDesign()
                      ? 'Check whether these changes implement the saved plan before approving the design.'
                      : 'These changes are available for inspection.'
                    : reviewTarget()
                }
                exportPath={exported()?.path}
                onExport={() =>
                  void request<{ path?: string }>(
                    { action: 'exportReview', runId: changes().runId },
                    setExported
                  )
                }
                busy={busy()}
                problem={
                  inputProblem() ||
                  error() ||
                  (!reviewSubject()
                    ? previewDesign() && !designApproval()
                      ? 'The approval changed. Reopen the review.'
                      : undefined
                    : shownStatus() !== 'verified' ||
                        run()?.application?.subjectDigest !== reviewSubject() ||
                        !matchingApplication(run(), changes().candidateDigest)
                      ? 'The checkout or candidate changed. Reopen the review.'
                      : undefined)
                }
                onApply={() => void applyChanges()}
                onClose={() => setReview(undefined)}
              />
            )}
          </Show>
        }
      >
        <PolyglotReviewFrame
          compact={props.automatic}
          title={
            props.automatic
              ? shownStatus() === 'failed'
                ? 'Verification failed'
                : run()?.pendingApprovals.length
                  ? 'Approval needed'
                  : 'Verification details'
              : props.runId
                ? 'Verified Autopilot task'
                : 'Plan implementation'
          }
          label="Verified plan implementation"
          closeLabel={props.automatic ? 'Close details' : 'Close verified implementation'}
          closeDisabled={!props.contextProblem && (busy() || run()?.status === 'running')}
          onClose={props.automatic ? () => setDetails(false) : props.onClose}
          actions={
            <>
              <Show when={restartInFooter()}>
                <button
                  class="chat-action-button"
                  type="button"
                  disabled={unavailable()}
                  onClick={restartPlan}
                >
                  Prepare again
                </button>
              </Show>
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
          <Show when={!props.automatic}>
            <p class="settings-hint">
              {props.executionLabel} ·{' '}
              {currentProfile()?.label ?? run()?.providerBinding?.model ?? 'Unavailable'}
            </p>
          </Show>
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
          <Show when={!props.automatic && plan()}>
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
                <Show when={props.automatic}>
                  <VerificationSummary run={current()} status={shownStatus()} />
                </Show>
                <Show when={!props.automatic}>
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
                      Current inputs are changed or could not be confirmed. This result does not
                      verify the current project.
                    </p>
                  </Show>
                  <Show when={current().reason}>
                    <p>{current().reason}</p>
                  </Show>
                  <For
                    each={current().reasons?.filter((reason) => reason !== current().reason) ?? []}
                  >
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
                </Show>
                <Show when={!props.automatic && current().providerBinding}>
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
                      <Show when={!props.automatic}>
                        <details>
                          <summary>Application receipt</summary>
                          <SubjectFields value={receipt()} />
                        </details>
                      </Show>
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
                      onClick={() =>
                        props.automatic && item.approvalType === 'design-review'
                          ? previewChanges(item)
                          : setApproval(item)
                      }
                    >
                      Review{' '}
                      {props.automatic
                        ? approvalDecision(item.approvalType).name
                        : item.approvalType}
                    </button>
                  )}
                </For>
                <Show when={approval()}>
                  {(item) => (
                    <section aria-label="Implementation approval">
                      <p>
                        {props.automatic
                          ? approvalDecision(item().approvalType).prompt
                          : item().summary}
                      </p>
                      <Show
                        when={props.automatic}
                        fallback={<PolyglotApproval approval={item()} />}
                      >
                        <details>
                          <summary>Inspect approval evidence</summary>
                          <PolyglotApproval approval={item()} />
                        </details>
                      </Show>
                      <Show when={!props.automatic}>
                        <p class="settings-hint">
                          Subject <code>{item().subjectDigest}</code> · Scope{' '}
                          <code>{item().scopeDigest}</code>
                        </p>
                      </Show>
                      <button
                        type="button"
                        class="chat-action-button"
                        disabled={
                          unavailable() ||
                          item().subject == null ||
                          (props.automatic &&
                            (shownStatus() === 'stale' || item().approvalType === 'design-review'))
                        }
                        onClick={() =>
                          void runAction({
                            action: 'approve',
                            runId: current().runId,
                            request: item().request,
                          })
                        }
                      >
                        Approve{' '}
                        {props.automatic
                          ? approvalDecision(item().approvalType).name
                          : item().approvalType}
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
                <Show when={!props.automatic}>
                  <button
                    type="button"
                    class="link-button"
                    disabled={unavailable()}
                    onClick={() => void runAction({ action: 'status', runId: current().runId })}
                  >
                    Refresh implementation result
                  </button>
                </Show>
                <Show
                  when={
                    !props.automatic ||
                    (current().candidateDigest &&
                      !current().pendingApprovals.some(
                        (item) => item.approvalType === 'design-review'
                      ))
                  }
                >
                  <button
                    type="button"
                    class="chat-action-button"
                    disabled={unavailable() || !current().candidateDigest}
                    onClick={() =>
                      props.automatic
                        ? previewChanges()
                        : void request<HarnessChangeReview>(
                            { action: 'reviewChanges', runId: current().runId },
                            setReview
                          )
                    }
                  >
                    {shownStatus() === 'verified'
                      ? 'Review verified changes'
                      : 'Review candidate changes'}
                  </button>
                </Show>
                <button
                  type="button"
                  class="link-button"
                  disabled={unavailable() || !current().candidateDigest}
                  onClick={() =>
                    void request<{ path?: string }>(
                      { action: 'exportReview', runId: current().runId },
                      setExported
                    )
                  }
                >
                  Export review bundle
                </button>
                <Show when={!props.automatic}>
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
                </Show>
                <Show when={current().application}>
                  {(application) => (
                    <section aria-label="Reviewed application">
                      <Show
                        when={!props.automatic}
                        fallback={
                          <p>
                            Destination:{' '}
                            {matchingApplication(
                              current(),
                              current().candidateDigest ?? undefined
                            ) || props.executionLabel}
                          </p>
                        }
                      >
                        <SubjectFields value={application().subject} />
                      </Show>
                      <p>
                        Approve the exact delivery subject above, then apply it to the named
                        checkout.
                      </p>
                      <button
                        type="button"
                        class="chat-action-button primary"
                        disabled={
                          unavailable() ||
                          !!inputProblem() ||
                          shownStatus() !== 'verified' ||
                          current().pendingApprovals.some(
                            (item) => item.approvalType === 'delivery'
                          )
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
                <Show when={!restartInFooter() && !props.runId && current().status !== 'running'}>
                  <button
                    type="button"
                    class="link-button"
                    disabled={unavailable()}
                    onClick={restartPlan}
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
                <Show when={value().path}>
                  <p>{value().path}</p>
                </Show>
              </div>
            )}
          </Show>
        </PolyglotReviewFrame>
      </Show>
    </Show>
  );
}
