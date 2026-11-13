import { createEffect, createSignal, For, onCleanup, Show, untrack } from 'solid-js';
import type { ModelProfile } from '../types';
import {
  displayedHarnessStatus,
  harnessRequest,
  verificationInputProblem,
  type HarnessApproval,
  type HarnessInspection,
  type HarnessOperation,
  type HarnessRun,
  type HarnessTarget,
  type HarnessProviderBinding,
} from '../lib/polyglotHarness';
import {
  harnessProviderProblem,
  boundHarnessProviderProblem,
  implementVerifiedPlan,
  parseHarnessRates,
  type HarnessChangeReview,
  type HarnessPlan,
  type HarnessPlanOperation,
} from '../lib/polyglotPlan';
import type { WorkspaceSetupInput } from '../lib/polyglotPreparation';
import PolyglotHarnessPanel from './PolyglotHarnessPanel';
import PolyglotApproval, { SubjectFields } from './PolyglotApproval';
import PolyglotPreparation from './PolyglotPreparation';
import PolyglotProviderRates from './PolyglotProviderRates';

/** Reviewed saved-plan implementation, independent of the ordinary agent's repair loop. */
export default function PolyglotPlanImplementation(props: {
  target: HarnessTarget;
  planPath?: string;
  /** Reopen the existing controller run, including an Autopilot-owned run. */
  runId?: string;
  profile?: ModelProfile;
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
  const [inspection, setInspection] = createSignal<HarnessInspection>();
  const [inputsReady, setInputsReady] = createSignal(false);
  const [untracked, setUntracked] = createSignal<string[]>([]);
  const [criteriaReviewed, setCriteriaReviewed] = createSignal(false);
  const [inputRate, setInputRate] = createSignal('');
  const [cachedRate, setCachedRate] = createSignal('');
  const [outputRate, setOutputRate] = createSignal('');
  const [run, setRun] = createSignal<HarnessRun>();
  const [approval, setApproval] = createSignal<HarnessApproval>();
  const [review, setReview] = createSignal<HarnessChangeReview>();
  const [exported, setExported] = createSignal<unknown>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [fresh, setFresh] = createSignal(false);
  const [releaseReviewed, setReleaseReviewed] = createSignal<string>();
  const [setupRevision, setSetupRevision] = createSignal(0);
  const [pollRevision, setPollRevision] = createSignal(0);
  let generation = 0;
  let requestNumber = 0;
  let disposed = false;
  const rates = () =>
    run()?.providerBinding?.rates ?? parseHarnessRates(inputRate(), cachedRate(), outputRate());
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
  const selectedSetup = () =>
    inspection()?.setups.find((setup) => setup.id === inspection()?.configuration?.setupId);
  const shownStatus = () => {
    const current = run();
    if (!current) return '';
    return ['verified', 'applied'].includes(current.status) && (!fresh() || !!props.contextProblem)
      ? 'stale'
      : displayedHarnessStatus(current, props.unsavedFiles, inspection()?.snapshotDigest);
  };

  async function readPlan() {
    if (props.contextProblem || !props.planPath) return;
    const scope = generation;
    const number = ++requestNumber;
    setBusy(true);
    setError('');
    setCriteriaReviewed(false);
    setPlan(undefined);
    try {
      const value = await harnessRequest<HarnessPlan>(props.target, {
        action: 'inspectPlan',
        planPath: props.planPath,
      });
      if (!disposed && scope === generation && number === requestNumber) setPlan(value);
    } catch (cause) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(cause));
        setFresh(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setBusy(false);
    }
  }

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
  function inspected(value: HarnessInspection, files: string[]) {
    if (
      value.snapshotDigest !== inspection()?.snapshotDigest ||
      value.configuration?.digest !== inspection()?.configuration?.digest
    )
      setCriteriaReviewed(false);
    setInspection(value);
    setUntracked(files);
    setInputsReady(true);
  }
  async function start() {
    if (unavailable() || !inputsReady() || !plan() || !inspection() || !rates()) return;
    const scope = generation;
    const number = ++requestNumber;
    setBusy(true);
    setError('');
    props.onActiveChange?.(true);
    try {
      const value = await implementVerifiedPlan({
        target: props.target,
        plan: plan()!,
        inspection: inspection()!,
        untracked: untracked(),
        unsavedFiles: props.unsavedFiles,
        provider: provider(),
        reviewed: criteriaReviewed(),
      });
      if (!disposed && scope === generation && number === requestNumber) acceptRun(value);
    } catch (cause) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(cause));
        setFresh(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setBusy(false);
    }
  }
  async function prepare(input: WorkspaceSetupInput) {
    setCriteriaReviewed(false);
    await request<HarnessInspection>({ action: 'prepareWorkspace', input }, (value) => {
      inspected(value, untracked());
      setSetupRevision((revision) => revision + 1);
    });
  }
  createEffect(() => {
    props.target.root;
    props.target.dialogId;
    props.planPath;
    props.runId;
    generation++;
    requestNumber++;
    setRun(undefined);
    setInspection(undefined);
    setInputsReady(false);
    setApproval(undefined);
    setReview(undefined);
    setFresh(false);
    setBusy(false);
    if (props.runId) void untrack(() => runAction({ action: 'status', runId: props.runId! }));
    else void untrack(readPlan);
  });
  createEffect(() => {
    if (props.contextProblem) {
      generation++;
      requestNumber++;
      setBusy(false);
      setFresh(false);
      setInputsReady(false);
      setCriteriaReviewed(false);
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
    <section class="settings-section polyglot-plan" aria-label="Verified plan implementation">
      <h3>{props.runId ? 'Verified Autopilot task' : 'Verified plan implementation'}</h3>
      <p class="settings-hint">
        Execution: {props.executionLabel}. Model:{' '}
        {currentProfile()?.label ?? run()?.providerBinding?.model ?? 'Unavailable'}. The controller
        generates a separate candidate and only accepts it after the reviewed checks pass. Applying
        changes to your project is a separate review.
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
          <details open>
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
      <Show when={!props.runId}>
        <button
          type="button"
          class="link-button"
          disabled={unavailable() || run()?.status === 'running'}
          onClick={() => void readPlan()}
        >
          Reload saved plan
        </button>
      </Show>
      <PolyglotProviderRates
        values={
          run()?.providerBinding
            ? {
                input: String(rates()!.input),
                cachedInput: String(rates()!.cachedInput),
                output: String(rates()!.output),
              }
            : { input: inputRate(), cachedInput: cachedRate(), output: outputRate() }
        }
        disabled={!!run()?.providerBinding}
        onChange={(field, value) => {
          if (field === 'input') setInputRate(value);
          else if (field === 'cachedInput') setCachedRate(value);
          else setOutputRate(value);
        }}
      />
      <Show when={!rates()}>
        <p class="settings-hint">Enter all three provider rates before any model calls.</p>
      </Show>
      <Show when={!run() && !props.runId && !props.contextProblem}>
        <Show when={setupRevision() + 1} keyed>
          {(_revision) => (
            <PolyglotHarnessPanel
              root={props.target.root}
              dialogId={props.target.dialogId}
              executionLabel={props.executionLabel}
              unsavedFiles={props.unsavedFiles}
              setupOnly
              onInspection={inspected}
              onInputPending={() => {
                setInputsReady(false);
                setCriteriaReviewed(false);
              }}
            />
          )}
        </Show>
        <Show when={inspection()}>
          {(info) => (
            <details>
              <summary>Create a verification setup from this project</summary>
              <PolyglotPreparation
                model={props.profile?.model ?? ''}
                manifests={info().inventory.units}
                busy={busy()}
                onPrepare={prepare}
              />
            </details>
          )}
        </Show>
        <Show when={selectedSetup()}>
          {(setup) => (
            <div>
              <p>
                <strong>Acceptance criteria: {setup().label}</strong>
              </p>
              <p>{setup().contractSummary}</p>
              <For each={setup().invariants ?? []}>
                {(invariant) => (
                  <p>
                    {invariant.statement} · {invariant.owner} · cases{' '}
                    {invariant.case_ids.join(', ')}
                  </p>
                )}
              </For>
            </div>
          )}
        </Show>
        <label>
          <input
            type="checkbox"
            checked={criteriaReviewed()}
            disabled={!inputsReady() || !plan() || !selectedSetup()}
            onChange={(event) => setCriteriaReviewed(event.currentTarget.checked)}
          />{' '}
          I reviewed these exact saved plan bytes and confirm the configured acceptance criteria
          cover the plan.
        </label>
        <button
          type="button"
          class="chat-action-button primary"
          disabled={
            unavailable() ||
            !inputsReady() ||
            !plan() ||
            !inspection()?.configuration ||
            !inspection()?.host.supported ||
            !rates() ||
            !criteriaReviewed() ||
            !!inputProblem() ||
            !!providerProblem()
          }
          onClick={() => void start()}
        >
          Start verified implementation
        </button>
        <For
          each={
            inspection()?.runs.filter(
              (entry) => entry.kind === 'implementation' && entry.planDigest === plan()?.digest
            ) ?? []
          }
        >
          {(entry) => (
            <button
              type="button"
              class="link-button"
              disabled={unavailable()}
              onClick={() => void runAction({ action: 'status', runId: entry.runId })}
            >
              Open implementation {entry.runId} ({entry.status})
            </button>
          )}
        </For>
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
                  <Show when={receipt().location === 'host' && !receipt().synchronizationReleased}>
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
                  <button type="button" class="link-button" onClick={() => setApproval(undefined)}>
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
            <details>
              <summary>Preparation role drafts</summary>
              <p class="settings-hint">
                Drafts use this run's model budget. They are advisory and grant no approvals. Review
                the proposals, then edit the guided setup and start a new run if the criteria or
                design must change.
              </p>
              <For each={['domain', 'architect', 'oracle'] as const}>
                {(role) => (
                  <button
                    type="button"
                    class="link-button"
                    disabled={
                      unavailable() ||
                      current().status === 'running' ||
                      !rates() ||
                      !!providerProblem()
                    }
                    onClick={() =>
                      void runAction({
                        action: 'draftPlan',
                        runId: current().runId,
                        role,
                        provider: provider(),
                      })
                    }
                  >
                    Draft {role} preparation
                  </button>
                )}
              </For>
              <For each={current().drafts ?? []}>
                {(value) => (
                  <details open>
                    <summary>{value.role} — advisory draft; review required</summary>
                    <SubjectFields value={value.draft} />
                  </details>
                )}
              </For>
            </details>
            <Show when={!props.runId && current().status !== 'running'}>
              <button
                type="button"
                class="link-button"
                disabled={unavailable()}
                onClick={() => {
                  setRun(undefined);
                  setCriteriaReviewed(false);
                  setSetupRevision((revision) => revision + 1);
                }}
              >
                Return to setup and plan review
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
      <button
        type="button"
        class="link-button"
        disabled={!props.contextProblem && (busy() || run()?.status === 'running')}
        onClick={props.onClose}
      >
        Close verified implementation
      </button>
    </section>
  );
}
