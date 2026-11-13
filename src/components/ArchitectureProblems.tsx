import { createEffect, For, onCleanup, Show } from 'solid-js';
import {
  architectureFeedback,
  architectureFilePath,
  architectureFindings,
  architectureMarkersCurrent,
  type createArchitectureFeedbackController,
} from '../lib/architectureFeedback';
import { openInEditor } from '../lib/openInEditor';
import { SubjectFields } from './PolyglotApproval';

const precisionLabel: Record<string, string> = {
  cargo_resolved: 'Cargo resolved dependencies',
  configured_build: 'Configured build graph',
  clang_semantic: 'Clang semantic dependencies',
  project_imports: 'Project import graph',
};

/** Saved-source architecture evidence, visibly separate from strict verification status. */
export default function ArchitectureProblems(props: {
  controller?: ReturnType<typeof createArchitectureFeedbackController>;
}) {
  const controller = () => props.controller ?? architectureFeedback;
  const view = () => controller().view();
  createEffect(() => {
    const current = view();
    if (!current || current.state !== 'current') return;
    const timer = window.setTimeout(() => void controller().refresh(), 5000);
    onCleanup(() => window.clearTimeout(timer));
  });
  return (
    <Show when={view()}>
      {(current) => (
        <section class="architecture-problems" aria-label="Architecture Problems">
          <div class="settings-section-title">Architecture Problems · advisory</div>
          <p class="settings-hint">
            {current().selection.label} · Run {current().selection.runId}. Findings describe saved
            source and do not grant verification approval.
          </p>
          <p class="settings-status" role="status">
            {current().state === 'loading'
              ? 'Reading architecture evidence…'
              : architectureMarkersCurrent(current())
                ? 'Architecture evidence matches the saved source.'
                : (current().reason ?? `Architecture evidence is ${current().state}.`)}
          </p>
          <button
            type="button"
            class="link-button"
            disabled={!controller().canRefresh()}
            onClick={() => void controller().refresh()}
          >
            Refresh architecture feedback
          </button>
          <Show when={current().state === 'loading'}>
            <button
              type="button"
              class="link-button"
              onClick={() =>
                controller().invalidate(
                  'Architecture feedback read cancelled. Refresh to try again.'
                )
              }
            >
              Cancel architecture read
            </button>
          </Show>
          <button type="button" class="link-button" onClick={() => controller().dismiss()}>
            Dismiss architecture feedback
          </button>
          <Show when={current().feedback}>
            {(feedback) => (
              <>
                <For each={feedback().diagnostics}>
                  {(diagnostic) => <p class="settings-status warning">{diagnostic}</p>}
                </For>
                <Show when={!feedback().policyApproved}>
                  <p class="settings-status warning">
                    The architecture policy has not been approved for this evidence. Editor markers
                    are unavailable.
                  </p>
                </Show>
                <Show when={!architectureFindings(feedback()).length}>
                  <p class="settings-hint">
                    No findings were reported by the available architecture observations. This does
                    not establish complete coverage or strict acceptance.
                  </p>
                </Show>
                <ul>
                  <For each={architectureFindings(feedback())}>
                    {(finding) => (
                      <li>
                        <strong>{finding.ruleId}</strong>: {finding.message}{' '}
                        <Show
                          when={finding.path}
                          fallback={<span>(graph-level; source file unavailable)</span>}
                        >
                          {(path) => (
                            <button
                              type="button"
                              class="link-button"
                              disabled={
                                !architectureMarkersCurrent(current()) ||
                                !architectureFilePath(current().selection.target.root, path())
                              }
                              onClick={() => {
                                const file = architectureFilePath(
                                  current().selection.target.root,
                                  path()
                                );
                                if (file)
                                  void openInEditor({ path: file, focus: true, marks: 'none' });
                              }}
                            >
                              {path()} (file-level)
                            </button>
                          )}
                        </Show>
                        <p class="settings-hint">
                          {finding.checkId} ·{' '}
                          {precisionLabel[finding.precision] ?? finding.precision}. Precise source
                          location unavailable.
                        </p>
                        <Show when={finding.chain.length}>
                          <p class="settings-hint">Dependency path: {finding.chain.join(' → ')}</p>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
                <details>
                  <summary>Observed checks and reviewed policies</summary>
                  <For each={feedback().observations}>
                    {(observation) => (
                      <p>
                        {observation.checkId}: {observation.status} ·{' '}
                        {precisionLabel[observation.precision] ?? observation.precision} · graph{' '}
                        {observation.graphDigest}
                      </p>
                    )}
                  </For>
                  <For each={feedback().policies}>
                    {(policy) => (
                      <details>
                        <summary>
                          {policy.profileId} · {policy.language} · {policy.checkId}
                        </summary>
                        <p>Configuration {policy.configurationDigest}</p>
                        <SubjectFields value={policy.architecture} />
                        <For each={policy.forbiddenEdges}>
                          {(edge) => (
                            <p>
                              Forbidden dependency: {edge.source} → {edge.target}
                            </p>
                          )}
                        </For>
                      </details>
                    )}
                  </For>
                </details>
              </>
            )}
          </Show>
        </section>
      )}
    </Show>
  );
}
