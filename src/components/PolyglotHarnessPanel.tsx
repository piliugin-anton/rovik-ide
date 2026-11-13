import { createEffect, createMemo, createSignal, For, onCleanup, Show } from 'solid-js';
import { open } from '@tauri-apps/plugin-dialog';
import { onFileDeleted, onFileWritten } from '../lib/approvals';
import PolyglotApproval, { SubjectFields } from './PolyglotApproval';
import {
  displayedHarnessStatus,
  harnessRequest,
  verificationInputProblem,
  verifyChanges,
  type HarnessApproval,
  type HarnessInspection,
  type HarnessOperation,
  type HarnessRun,
} from '../lib/polyglotHarness';

const STATUS_LABEL = {
  running: 'Verification running',
  verified: 'Verified',
  applied: 'Verified changes applied to the named checkout',
  failed: 'Verification failed',
  blocked: 'Verification blocked',
  stale: 'Verification is stale',
  cancelled: 'Verification cancelled',
};

/** Project setup and exact-snapshot verification through the trusted controller. */
export default function PolyglotHarnessPanel(props: {
  root: string;
  dialogId?: string | null;
  executionLabel?: string;
  /** Undefined means the caller cannot establish what the editor has saved. */
  unsavedFiles?: readonly string[];
  /** Preparation reuses setup selection without starting a separate verification. */
  setupOnly?: boolean;
  onInspection?: (inspection: HarnessInspection, untracked: string[]) => void;
  onInputPending?: () => void;
}) {
  const [inspection, setInspection] = createSignal<HarnessInspection>();
  const [selectedFiles, setSelectedFiles] = createSignal<string[]>([]);
  const [setupId, setSetupId] = createSignal('');
  const [engineId, setEngineId] = createSignal('');
  const [hostPath, setHostPath] = createSignal('');
  const [run, setRun] = createSignal<HarnessRun>();
  const [reviewedApproval, setReviewedApproval] = createSignal<HarnessApproval>();
  const [busy, setBusy] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [pollRevision, setPollRevision] = createSignal(0);
  const [freshnessKnown, setFreshnessKnown] = createSignal(false);
  let generation = 0;
  let requestNumber = 0;
  let disposed = false;
  const target = () => ({ root: props.root, dialogId: props.dialogId });
  const inputProblem = () => verificationInputProblem(props.unsavedFiles);
  const selectedSetup = createMemo(() => inspection()?.setups.find((s) => s.id === setupId()));
  const engineKey = (engine: { kind: string; executable: string }) =>
    `${engine.kind}:${engine.executable}`;
  const selectedEngine = () => inspection()?.host.engines.find((e) => engineKey(e) === engineId());
  const shownStatus = (current: HarnessRun) =>
    current.status === 'verified' && !freshnessKnown()
      ? 'stale'
      : displayedHarnessStatus(current, props.unsavedFiles, inspection()?.snapshotDigest);

  function acceptInspection(value: HarnessInspection) {
    setInspection(value);
    setFreshnessKnown(true);
    props.onInspection?.(value, [...selectedFiles()]);
    if (!value.setups.some((s) => s.id === setupId()))
      setSetupId(value.configuration?.setupId ?? value.setups[0]?.id ?? '');
    if (!value.host.engines.some((e) => engineKey(e) === engineId())) {
      const engine = value.configuration?.engine ?? value.host.engines[0];
      setEngineId(engine ? engineKey(engine) : '');
    }
    const updated = value.runs.find((entry) => entry.runId === run()?.runId);
    if (updated) setRun(updated);
    else if (!run() && value.runs.length) setRun(value.runs[0]);
  }

  async function inspect(files = selectedFiles()) {
    props.onInputPending?.();
    const scope = generation;
    const number = ++requestNumber;
    setLoading(true);
    setError(undefined);
    try {
      const value = await harnessRequest<HarnessInspection>(target(), {
        action: 'inspect',
        untracked: files,
      });
      if (disposed || scope !== generation || number !== requestNumber) return;
      acceptInspection(value);
    } catch (e) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(e));
        setFreshnessKnown(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setLoading(false);
    }
  }

  async function action(request: HarnessOperation, kind: 'inspection' | 'run') {
    if (kind === 'inspection') props.onInputPending?.();
    const scope = generation;
    const number = ++requestNumber;
    setLoading(false);
    setBusy(true);
    setError(undefined);
    try {
      const response = await harnessRequest<HarnessInspection | HarnessRun>(target(), request);
      if (disposed || scope !== generation || number !== requestNumber) return;
      if (kind === 'inspection') acceptInspection(response as HarnessInspection);
      else {
        setRun(response as HarnessRun);
        setFreshnessKnown(true);
      }
      setReviewedApproval(undefined);
      return true;
    } catch (e) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(e));
        setFreshnessKnown(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setBusy(false);
    }
  }

  async function verify() {
    const value = inspection();
    if (!value || busy() || loading()) return;
    const scope = generation;
    const number = ++requestNumber;
    setBusy(true);
    setError(undefined);
    try {
      const result = await verifyChanges(target(), value, selectedFiles(), props.unsavedFiles);
      if (!disposed && scope === generation && number === requestNumber) {
        setRun(result);
        setFreshnessKnown(true);
      }
    } catch (e) {
      if (!disposed && scope === generation && number === requestNumber) {
        setError(String(e));
        setFreshnessKnown(false);
      }
    } finally {
      if (!disposed && scope === generation && number === requestNumber) setBusy(false);
    }
  }

  async function importSetup() {
    const scope = generation;
    try {
      const path = props.dialogId
        ? hostPath().trim()
        : await open({
            title: 'Choose the prepared verification setup',
            multiple: false,
            filters: [{ name: 'Prepared verification setup', extensions: ['json'] }],
          });
      if (disposed || scope !== generation || typeof path !== 'string' || !path) return;
      const imported = await action({ action: 'importSetup', path }, 'inspection');
      if (imported && !disposed && scope === generation) await inspect(selectedFiles());
    } catch (e) {
      if (!disposed && scope === generation) setError(String(e));
    }
  }

  createEffect(() => {
    props.root;
    props.dialogId;
    generation++;
    setInspection(undefined);
    setRun(undefined);
    setSelectedFiles([]);
    setReviewedApproval(undefined);
    setFreshnessKnown(false);
    setBusy(false);
    void inspect([]);
  });

  // A persisted run survives this panel. Polling only observes it and is cleaned
  // up on close; it never restarts work or grants approval implicitly.
  createEffect(() => {
    pollRevision();
    if (busy() || loading()) return;
    const current = run();
    if (!current || !['running', 'verified'].includes(current.status)) return;
    const scope = generation;
    const timer = window.setTimeout(
      async () => {
        if (busy() || loading()) return;
        const number = requestNumber;
        try {
          const result = await harnessRequest<HarnessRun>(target(), {
            action: 'status',
            runId: current.runId,
          });
          if (
            !disposed &&
            scope === generation &&
            number === requestNumber &&
            run()?.runId === current.runId
          ) {
            setRun(result);
            setError(undefined);
            setFreshnessKnown(true);
          }
        } catch (e) {
          if (!disposed && scope === generation && number === requestNumber) {
            setError(String(e));
            setFreshnessKnown(false);
          }
        } finally {
          if (!disposed && scope === generation) setPollRevision((revision) => revision + 1);
        }
      },
      current.status === 'running' ? 1500 : 5000
    );
    onCleanup(() => window.clearTimeout(timer));
  });

  const changed = (path: string) => {
    const root = props.root.replace(/\\/g, '/').replace(/\/$/, '');
    const file = path.replace(/\\/g, '/');
    if (file.startsWith(`${root}/`) && !busy()) void inspect();
  };
  onCleanup(onFileWritten(changed));
  onCleanup(onFileDeleted(changed));
  onCleanup(() => {
    disposed = true;
    generation++;
  });

  return (
    <div class="polyglot-harness">
      <p class="settings-hint">
        {props.setupOnly
          ? 'Configure the protected acceptance tests and architecture rules for the reviewed plan. Implementation uses the selected model; verification checks run without model calls.'
          : 'Verify an exact snapshot against reviewed acceptance tests and architecture rules. Verification uses the execution host’s protected environment and makes no model calls.'}
      </p>
      <p class="settings-hint">
        Execution:{' '}
        {props.executionLabel ?? (props.dialogId ? 'the selected chat’s host' : 'this computer')}.
      </p>
      <Show when={error()}>
        <p class="settings-status warning" role="alert">
          {error()}
        </p>
      </Show>
      <Show when={inputProblem()}>
        <p class="settings-status warning" role="status">
          {inputProblem()}
        </p>
      </Show>
      <button
        type="button"
        class="link-button"
        disabled={busy() || loading()}
        onClick={() => void inspect()}
      >
        {loading() ? 'Inspecting project…' : 'Refresh project and results'}
      </button>
      <Show when={inspection()}>
        {(info) => (
          <>
            <Show when={!info().host.supported}>
              <p class="settings-status warning" role="status">
                {info().host.reason ??
                  'Strict verification requires a supported Linux execution host with Docker or Podman.'}
              </p>
            </Show>
            <For each={info().inventory.units}>
              {(unit) => (
                <p class="settings-hint">
                  <strong>{unit.root || 'Project root'}</strong>: {unit.languages.join(', ')} —{' '}
                  {unit.manifest}
                </p>
              )}
            </For>
            <For each={[...info().issues, ...info().inventory.unresolved]}>
              {(issue) => <p class="settings-status warning">{issue}</p>}
            </For>

            <label class="setting-row">
              <span class="setting-row-label">Prepared verification setup</span>
              <select
                class="settings-select"
                aria-label="Prepared verification setup"
                value={setupId()}
                disabled={busy() || loading()}
                onChange={(e) => {
                  props.onInputPending?.();
                  setSetupId(e.currentTarget.value);
                }}
              >
                <option value="">Choose a setup</option>
                <For each={info().setups}>
                  {(setup) => <option value={setup.id}>{setup.label}</option>}
                </For>
              </select>
            </label>
            <Show when={selectedSetup()}>
              {(setup) => (
                <div>
                  <p class="settings-hint">{setup().contractSummary}</p>
                  <p class="settings-hint">Configurations: {setup().configurations.join(', ')}</p>
                  <Show when={setup().scope}>
                    <p class="settings-hint">
                      Writable scope: {setup().scope!.allow_write.join(', ')}
                    </p>
                    <p class="settings-hint">
                      Protected paths: {setup().scope!.protected.join(', ') || 'None declared'}
                    </p>
                  </Show>
                  <For each={setup().invariants ?? []}>
                    {(invariant) => (
                      <p class="settings-hint">
                        {invariant.id}: {invariant.statement} ({invariant.owner})
                      </p>
                    )}
                  </For>
                  <Show when={setup().negativeControlCases?.length}>
                    <p class="settings-hint">
                      Negative controls: {setup().negativeControlCases!.join(', ')}
                    </p>
                  </Show>
                  <Show when={setup().riskPolicy}>
                    <details>
                      <summary>Risk evidence requirements</summary>
                      <SubjectFields value={setup().riskPolicy} />
                    </details>
                  </Show>
                  <ul>
                    <For each={setup().checks}>
                      {(check) => (
                        <li>
                          {check.id}: {check.command} ({check.phase})
                          <Show when={check.argv?.length}>
                            <p>
                              <code>{check.argv!.join(' ')}</code>
                            </p>
                          </Show>
                          <Show when={check.caseIds?.length}>
                            <p>Required cases: {check.caseIds!.join(', ')}</p>
                          </Show>
                          <Show when={check.minimumTestCount !== undefined}>
                            <p>Minimum executed cases: {check.minimumTestCount}</p>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              )}
            </Show>
            <label class="setting-row">
              <span class="setting-row-label">Container engine</span>
              <select
                class="settings-select"
                aria-label="Container engine"
                value={engineId()}
                disabled={busy() || loading()}
                onChange={(e) => {
                  props.onInputPending?.();
                  setEngineId(e.currentTarget.value);
                }}
              >
                <option value="">Choose an installed engine</option>
                <For each={info().host.engines}>
                  {(engine) => (
                    <option value={engineKey(engine)}>
                      {engine.kind} — {engine.executable}
                    </option>
                  )}
                </For>
              </select>
            </label>
            <button
              type="button"
              class="chat-action-button"
              disabled={
                busy() ||
                loading() ||
                !info().host.supported ||
                !selectedSetup() ||
                !selectedEngine()
              }
              onClick={() =>
                void action(
                  {
                    action: 'configure',
                    setupId: setupId(),
                    engine: selectedEngine()!,
                    expectedSnapshotDigest: info().snapshotDigest,
                  },
                  'inspection'
                )
              }
            >
              Use this setup
            </button>
            <details>
              <summary>Import a prepared setup</summary>
              <p class="settings-hint">
                Choose a prepared setup with its accompanying artifacts directory. Importing it
                grants no approvals.
              </p>
              <Show when={props.dialogId}>
                <input
                  class="settings-input"
                  aria-label="Prepared setup path on execution host"
                  placeholder="Absolute setup path on the execution host"
                  value={hostPath()}
                  onInput={(e) => setHostPath(e.currentTarget.value)}
                />
              </Show>
              <button
                type="button"
                class="link-button"
                disabled={busy() || loading() || (!!props.dialogId && !hostPath().trim())}
                onClick={() => void importSetup()}
              >
                {props.dialogId ? 'Import from execution host' : 'Choose prepared setup…'}
              </button>
            </details>

            <Show when={info().untracked.length}>
              <p class="settings-hint">
                Tracked changes are included. Select any new files that belong in this verification:
              </p>
              <For each={info().untracked}>
                {(path) => (
                  <label class="setting-row">
                    <span>{path}</span>
                    <input
                      type="checkbox"
                      aria-label={`Include ${path}`}
                      checked={selectedFiles().includes(path)}
                      disabled={busy() || loading()}
                      onChange={(e) => {
                        const files = e.currentTarget.checked
                          ? [...selectedFiles(), path]
                          : selectedFiles().filter((p) => p !== path);
                        setSelectedFiles(files);
                        void inspect(files);
                      }}
                    />
                  </label>
                )}
              </For>
            </Show>
            <Show when={!props.setupOnly}>
              <button
                type="button"
                class="chat-action-button primary"
                disabled={
                  busy() ||
                  loading() ||
                  !!inputProblem() ||
                  !info().host.supported ||
                  !info().configuration ||
                  setupId() !== info().configuration?.setupId ||
                  engineId() !==
                    (info().configuration ? engineKey(info().configuration!.engine) : '')
                }
                onClick={() => void verify()}
              >
                Verify changes
              </button>
            </Show>

            <Show when={!props.setupOnly && info().runs.length > 1}>
              <label class="setting-row">
                <span>Run history</span>
                <select
                  class="settings-select"
                  aria-label="Verification run history"
                  value={run()?.runId ?? ''}
                  disabled={busy()}
                  onChange={(e) =>
                    void action({ action: 'status', runId: e.currentTarget.value }, 'run')
                  }
                >
                  <For each={info().runs}>
                    {(entry) => (
                      <option value={entry.runId}>
                        {entry.runId} — {entry.status}
                      </option>
                    )}
                  </For>
                </select>
              </label>
            </Show>
          </>
        )}
      </Show>

      <Show when={!props.setupOnly && run()}>
        {(current) => (
          <section aria-label="Verification result">
            <p
              class="settings-status"
              classList={{
                warning: !['running', 'verified'].includes(shownStatus(current())),
              }}
              role="status"
            >
              {STATUS_LABEL[shownStatus(current())]}
            </p>
            <p class="settings-hint">
              Run {current().runId} · {current().state}
            </p>
            <p class="settings-hint">
              Snapshot <code>{current().snapshotDigest}</code>
            </p>
            <Show when={current().reason}>
              <p class="settings-hint">{current().reason}</p>
            </Show>
            <For each={current().reasons?.filter((reason) => reason !== current().reason) ?? []}>
              {(reason) => <p class="settings-hint">{reason}</p>}
            </For>
            <For each={current().checks ?? []}>
              {(check) => (
                <details>
                  <summary>
                    {check.check_id}: {check.status} · {check.executed_test_count} cases executed
                  </summary>
                  <Show when={check.failed_case_ids.length}>
                    <p>Failed cases: {check.failed_case_ids.join(', ')}</p>
                  </Show>
                  <For each={check.diagnostics}>
                    {(diagnostic) => (
                      <p>
                        {diagnostic.rule_id}: {diagnostic.message}{' '}
                        {diagnostic.file
                          ? `(${diagnostic.file}${diagnostic.line ? `:${diagnostic.line}` : ''})`
                          : ''}
                      </p>
                    )}
                  </For>
                </details>
              )}
            </For>
            <Show when={shownStatus(current()) === 'stale'}>
              <p class="settings-hint">
                {freshnessKnown()
                  ? 'The current inputs differ from this verified snapshot. Save your files and run verification again.'
                  : 'Current inputs could not be confirmed. Refresh the result to check whether this verified snapshot is still current.'}
              </p>
            </Show>
            <For each={current().pendingApprovals}>
              {(approval) => (
                <button
                  type="button"
                  class="link-button"
                  disabled={busy()}
                  onClick={() => setReviewedApproval(approval)}
                >
                  Review {approval.approvalType}
                </button>
              )}
            </For>
            <Show when={reviewedApproval()}>
              {(approval) => (
                <div class="settings-section">
                  <p>{approval().summary}</p>
                  <PolyglotApproval approval={approval()} />
                  <p class="settings-hint">
                    Subject <code>{approval().subjectDigest}</code>
                  </p>
                  <p class="settings-hint">
                    Scope <code>{approval().scopeDigest}</code>
                  </p>
                  <button
                    type="button"
                    class="chat-action-button"
                    disabled={busy() || approval().subject == null}
                    onClick={() =>
                      void action(
                        { action: 'approve', runId: current().runId, request: approval().request },
                        'run'
                      )
                    }
                  >
                    Approve {approval().approvalType}
                  </button>
                  <button
                    type="button"
                    class="link-button"
                    onClick={() => setReviewedApproval(undefined)}
                  >
                    Close review
                  </button>
                </div>
              )}
            </Show>
            <Show when={current().canResume}>
              <button
                type="button"
                class="chat-action-button"
                disabled={busy() || !!inputProblem()}
                onClick={() => void action({ action: 'resume', runId: current().runId }, 'run')}
              >
                Resume verification
              </button>
            </Show>
            <Show when={current().canRecover}>
              <button
                type="button"
                class="chat-action-button"
                disabled={busy()}
                onClick={() => void action({ action: 'recover', runId: current().runId }, 'run')}
              >
                Recover interrupted verification
              </button>
            </Show>
            <Show when={current().canCancel}>
              <button
                type="button"
                class="link-button"
                disabled={busy()}
                onClick={() => void action({ action: 'cancel', runId: current().runId }, 'run')}
              >
                Cancel verification
              </button>
            </Show>
            <button
              type="button"
              class="link-button"
              disabled={busy()}
              onClick={() => void action({ action: 'status', runId: current().runId }, 'run')}
            >
              Refresh result
            </button>
          </section>
        )}
      </Show>
    </div>
  );
}
