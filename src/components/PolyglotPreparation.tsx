import { createSignal, For, Index, Show } from 'solid-js';
import {
  PROFILE_FACTS,
  lines,
  suggestedProfile,
  suggestedWorkspaceSetup,
  workspaceSetupProblem,
  type HarnessLanguage,
  type WorkspaceProfileInput,
  type WorkspaceSetupInput,
} from '../lib/polyglotPreparation';

function TextField(props: {
  label: string;
  value: string;
  change: (value: string) => void;
  multiline?: boolean;
}) {
  return (
    <label class="setting-row">
      <span class="setting-row-label">{props.label}</span>
      <Show
        when={props.multiline}
        fallback={
          <input
            class="settings-input"
            aria-label={props.label}
            value={props.value}
            onInput={(event) => props.change(event.currentTarget.value)}
          />
        }
      >
        <textarea
          class="settings-input"
          aria-label={props.label}
          rows={3}
          value={props.value}
          onInput={(event) => props.change(event.currentTarget.value)}
        />
      </Show>
    </label>
  );
}
function ListField(props: { label: string; value: string[]; change: (value: string[]) => void }) {
  return (
    <label class="setting-row">
      <span class="setting-row-label">{props.label} (one per line)</span>
      <textarea
        class="settings-input"
        aria-label={`${props.label} (one per line)`}
        rows={3}
        value={props.value.join('\n')}
        onChange={(event) => props.change(lines(event.currentTarget.value))}
      />
    </label>
  );
}
const title = (key: string) => key.replace(/_/g, ' ').replace(/([a-z])([A-Z])/g, '$1 $2');

/** Typed setup construction; all executable trust still comes from host-side validation and approval. */
export default function PolyglotPreparation(props: {
  model: string;
  manifests: { manifest: string; languages: string[] }[];
  busy: boolean;
  onPrepare: (input: WorkspaceSetupInput) => Promise<void>;
}) {
  const initial = suggestedWorkspaceSetup(props.model);
  if (props.manifests.length)
    initial.profiles = props.manifests.flatMap((unit, index) =>
      unit.languages
        .filter((language): language is HarnessLanguage => language in PROFILE_FACTS)
        .map((language) =>
          suggestedProfile(language, `profile-${index + 1}-${language}`, unit.manifest)
        )
    );
  const [form, setForm] = createSignal(initial);
  const [reviewed, setReviewed] = createSignal(false);
  const [componentName, setComponentName] = createSignal('');
  const [componentPurpose, setComponentPurpose] = createSignal('');
  const [existingFile, setExistingFile] = createSignal('');
  const [existingSymbol, setExistingSymbol] = createSignal('');
  const [existingRationale, setExistingRationale] = createSignal('');
  const update = (patch: Partial<WorkspaceSetupInput>) => {
    setReviewed(false);
    setForm((value) => ({ ...value, ...patch }));
  };
  const profile = (index: number, patch: Partial<WorkspaceProfileInput>) =>
    update({
      profiles: form().profiles.map((value, i) => (i === index ? { ...value, ...patch } : value)),
    });
  const design = (patch: Partial<WorkspaceSetupInput['design']>) =>
    update({ design: { ...form().design, ...patch } });
  const problem = () => workspaceSetupProblem(form());
  return (
    <section aria-label="Guided verification setup" class="settings-section">
      <p class="settings-hint">
        Build a setup from this project's manifests and independently protected acceptance tests.
        Values below are suggestions until you review them. Test IDs must name real executed cases;
        a known failing revision must demonstrate their sensitivity.
      </p>
      <TextField label="Setup name" value={form().name} change={(name) => update({ name })} />
      <TextField
        label="Acceptance intent"
        value={form().intent}
        change={(intent) => update({ intent })}
        multiline
      />
      <TextField
        label="Bounded context"
        value={form().boundedContext}
        change={(boundedContext) => update({ boundedContext })}
      />
      <ListField
        label="Writable paths"
        value={form().writePaths}
        change={(writePaths) => update({ writePaths })}
      />
      <ListField
        label="Protected paths"
        value={form().protectedPaths}
        change={(protectedPaths) => update({ protectedPaths })}
      />
      <Index each={form().profiles}>
        {(entry, index) => (
          <details open>
            <summary>
              Language profile {index + 1}: {entry().id}
            </summary>
            <TextField
              label={`Profile ${index + 1} identifier`}
              value={entry().id}
              change={(id) => profile(index, { id })}
            />
            <label class="setting-row">
              Language
              <select
                class="settings-select"
                aria-label={`Profile ${index + 1} language`}
                value={entry().language}
                onChange={(event) =>
                  profile(
                    index,
                    suggestedProfile(
                      event.currentTarget.value as HarnessLanguage,
                      entry().id,
                      entry().manifest
                    )
                  )
                }
              >
                <For each={Object.keys(PROFILE_FACTS)}>
                  {(language) => <option value={language}>{language}</option>}
                </For>
              </select>
            </label>
            <TextField
              label={`Profile ${index + 1} manifest`}
              value={entry().manifest}
              change={(manifest) => profile(index, { manifest })}
            />
            <For each={PROFILE_FACTS[entry().language]}>
              {(key) => (
                <TextField
                  label={`${entry().id}: ${title(key)}`}
                  value={entry().facts[key] ?? ''}
                  change={(value) => profile(index, { facts: { ...entry().facts, [key]: value } })}
                />
              )}
            </For>
            <ListField
              label={`${entry().id}: protected test paths`}
              value={entry().protectedTestPaths}
              change={(protectedTestPaths) => profile(index, { protectedTestPaths })}
            />
            <Show when={entry().language === 'typescript'}>
              <p class="settings-hint">
                Select a committed single-project compiler configuration. Configurations using
                extends or project references need a separately prepared single-project
                configuration; this includes many Vite and Tauri defaults. The protected tests must
                exercise emitted code in /build/project/dist.
              </p>
              <TextField
                label={`${entry().id}: TypeScript project`}
                value={entry().typescriptProject ?? ''}
                change={(typescriptProject) => profile(index, { typescriptProject })}
              />
            </Show>
            <Index each={entry().configurations}>
              {(configuration, ci) => {
                const change = (patch: Partial<WorkspaceProfileInput['configurations'][number]>) =>
                  profile(index, {
                    configurations: entry().configurations.map((value, i) =>
                      i === ci ? { ...value, ...patch } : value
                    ),
                  });
                return (
                  <fieldset>
                    <legend>Configuration {ci + 1}</legend>
                    <For each={['id', 'target', 'runtime'] as const}>
                      {(key) => (
                        <TextField
                          label={`${entry().id} configuration ${ci + 1}: ${key}`}
                          value={configuration()[key]}
                          change={(value) => change({ [key]: value })}
                        />
                      )}
                    </For>
                    <For each={['features', 'build_targets', 'flags'] as const}>
                      {(key) => (
                        <ListField
                          label={`${entry().id} configuration ${ci + 1}: ${title(key)}`}
                          value={configuration()[key]}
                          change={(value) => change({ [key]: value })}
                        />
                      )}
                    </For>
                    <label>
                      <input
                        type="checkbox"
                        checked={configuration().default_features}
                        onChange={(event) =>
                          change({ default_features: event.currentTarget.checked })
                        }
                      />{' '}
                      Enable default features
                    </label>
                    <button
                      type="button"
                      class="link-button"
                      disabled={entry().configurations.length === 1}
                      onClick={() =>
                        profile(index, {
                          configurations: entry().configurations.filter((_, i) => i !== ci),
                        })
                      }
                    >
                      Remove configuration
                    </button>
                  </fieldset>
                );
              }}
            </Index>
            <button
              type="button"
              class="link-button"
              onClick={() =>
                profile(index, {
                  configurations: [
                    ...entry().configurations,
                    {
                      ...suggestedProfile(entry().language).configurations[0],
                      id: `${entry().id}-tests-${entry().configurations.length + 1}`,
                    },
                  ],
                })
              }
            >
              Add configuration
            </button>
            <button
              type="button"
              class="link-button"
              onClick={() => update({ profiles: form().profiles.filter((_, i) => i !== index) })}
            >
              Remove profile
            </button>
          </details>
        )}
      </Index>
      <button
        type="button"
        class="link-button"
        onClick={() =>
          update({
            profiles: [
              ...form().profiles,
              suggestedProfile('typescript', `profile-${form().profiles.length + 1}`),
            ],
          })
        }
      >
        Add language profile
      </button>
      <Index each={form().requirements}>
        {(requirement, index) => {
          const change = (patch: Partial<WorkspaceSetupInput['requirements'][number]>) =>
            update({
              requirements: form().requirements.map((value, i) =>
                i === index ? { ...value, ...patch } : value
              ),
            });
          return (
            <fieldset>
              <legend>Acceptance requirement {index + 1}</legend>
              <For each={['id', 'statement', 'owner'] as const}>
                {(key) => (
                  <TextField
                    label={`Requirement ${index + 1}: ${key}`}
                    value={requirement()[key]}
                    change={(value) => change({ [key]: value })}
                    multiline={key === 'statement'}
                  />
                )}
              </For>
              <For
                each={form().profiles.flatMap((p) =>
                  p.configurations.map((c) => `${p.id}/${c.id}`)
                )}
              >
                {(key) => (
                  <ListField
                    label={`Requirement ${index + 1}: case IDs for ${key}`}
                    value={requirement().casesByConfiguration[key] ?? []}
                    change={(value) =>
                      change({
                        casesByConfiguration: {
                          ...requirement().casesByConfiguration,
                          [key]: value,
                        },
                      })
                    }
                  />
                )}
              </For>
              <button
                type="button"
                class="link-button"
                onClick={() =>
                  update({ requirements: form().requirements.filter((_, i) => i !== index) })
                }
              >
                Remove requirement
              </button>
            </fieldset>
          );
        }}
      </Index>
      <button
        type="button"
        class="link-button"
        onClick={() =>
          update({
            requirements: [
              ...form().requirements,
              {
                id: `requirement-${form().requirements.length + 1}`,
                statement: '',
                owner: '',
                casesByConfiguration: {},
              },
            ],
          })
        }
      >
        Add requirement
      </button>
      <details>
        <summary>Design and existing code</summary>
        <p class="settings-hint">
          Public API changes require the protected tests for every configuration and a separate
          design review. Unsafe code, FFI, resource-lifecycle and concurrency changes need
          additional reviewed evidence beyond this initial setup.
        </p>
        <For each={Object.entries(form().design.components)}>
          {([name, purpose]) => (
            <p>
              {name}: {purpose}{' '}
              <button
                type="button"
                class="link-button"
                onClick={() =>
                  design({
                    components: Object.fromEntries(
                      Object.entries(form().design.components).filter(([key]) => key !== name)
                    ),
                  })
                }
              >
                Remove component
              </button>
            </p>
          )}
        </For>
        <TextField label="Component name" value={componentName()} change={setComponentName} />
        <TextField
          label="Component responsibility"
          value={componentPurpose()}
          change={setComponentPurpose}
        />
        <button
          type="button"
          class="link-button"
          disabled={!componentName() || !componentPurpose()}
          onClick={() => {
            design({
              components: { ...form().design.components, [componentName()]: componentPurpose() },
            });
            setComponentName('');
            setComponentPurpose('');
          }}
        >
          Add component
        </button>
        <For each={form().design.existingCode}>
          {(code, index) => (
            <p>
              {code.file} — {code.symbol}: {code.rationale}{' '}
              <button
                type="button"
                class="link-button"
                onClick={() =>
                  design({
                    existingCode: form().design.existingCode.filter((_, i) => i !== index()),
                  })
                }
              >
                Remove code reference
              </button>
            </p>
          )}
        </For>
        <TextField label="Existing file" value={existingFile()} change={setExistingFile} />
        <TextField label="Existing symbol" value={existingSymbol()} change={setExistingSymbol} />
        <TextField
          label="Why reuse or change this code"
          value={existingRationale()}
          change={setExistingRationale}
        />
        <button
          type="button"
          class="link-button"
          disabled={!existingFile() || !existingSymbol() || !existingRationale()}
          onClick={() => {
            design({
              existingCode: [
                ...form().design.existingCode,
                { file: existingFile(), symbol: existingSymbol(), rationale: existingRationale() },
              ],
            });
            setExistingFile('');
            setExistingSymbol('');
            setExistingRationale('');
          }}
        >
          Add code reference
        </button>
        <For
          each={
            [
              'publicApiChanges',
              'resourceContracts',
              'errorContracts',
              'mechanismAlternatives',
            ] as const
          }
        >
          {(key) => (
            <ListField
              label={title(key)}
              value={form().design[key]}
              change={(value) => design({ [key]: value })}
            />
          )}
        </For>
        <For each={['concurrencyContract', 'mechanismPressure', 'mechanismCost'] as const}>
          {(key) => (
            <TextField
              label={title(key)}
              value={form().design[key]}
              change={(value) => design({ [key]: value })}
              multiline
            />
          )}
        </For>
      </details>
      <details>
        <summary>Protected execution environment and negative control</summary>
        <p class="settings-hint">
          JavaScript, TypeScript and Python tests run from the protected /oracle directory and must
          exercise /build/project (TypeScript output: /build/project/dist). Native test files and
          build inputs stay frozen. Paths refer to the selected execution host. The image must be
          immutable. Dependencies must already be installed in the offline directory; setup does not
          run arbitrary install commands.
        </p>
        <For each={['imageDigest', 'toolchainManifestPath', 'dependencyDirectory'] as const}>
          {(key) => (
            <TextField
              label={title(key)}
              value={form().environment[key]}
              change={(value) => update({ environment: { ...form().environment, [key]: value } })}
            />
          )}
        </For>
        <label class="setting-row">
          Container engine
          <select
            aria-label="Prepared environment engine"
            class="settings-select"
            value={form().environment.engine}
            onChange={(event) =>
              update({
                environment: {
                  ...form().environment,
                  engine: event.currentTarget.value as 'docker' | 'podman',
                },
              })
            }
          >
            <option value="docker">Docker</option>
            <option value="podman">Podman</option>
          </select>
        </label>
        <TextField
          label="Known failing Git revision"
          value={form().negativeControlRevision}
          change={(negativeControlRevision) => update({ negativeControlRevision })}
        />
        <ListField
          label="Negative control case IDs"
          value={form().negativeControlCases}
          change={(negativeControlCases) => update({ negativeControlCases })}
        />
        <Index each={form().baselineFailures}>
          {(failure, index) => {
            const change = (patch: Partial<WorkspaceSetupInput['baselineFailures'][number]>) =>
              update({
                baselineFailures: form().baselineFailures.map((value, i) =>
                  i === index ? { ...value, ...patch } : value
                ),
              });
            return (
              <fieldset>
                <legend>Expected baseline failure {index + 1}</legend>
                <For each={['profileId', 'configurationId', 'rationale'] as const}>
                  {(key) => (
                    <TextField
                      label={`Baseline failure ${index + 1}: ${title(key)}`}
                      value={failure()[key]}
                      change={(value) => change({ [key]: value })}
                    />
                  )}
                </For>
                <ListField
                  label={`Baseline failure ${index + 1}: case IDs`}
                  value={failure().caseIds}
                  change={(caseIds) => change({ caseIds })}
                />
                <button
                  type="button"
                  class="link-button"
                  onClick={() =>
                    update({
                      baselineFailures: form().baselineFailures.filter((_, i) => i !== index),
                    })
                  }
                >
                  Remove baseline failure
                </button>
              </fieldset>
            );
          }}
        </Index>
        <button
          type="button"
          class="link-button"
          onClick={() =>
            update({
              baselineFailures: [
                ...form().baselineFailures,
                {
                  profileId: form().profiles[0]?.id ?? '',
                  configurationId: form().profiles[0]?.configurations[0]?.id ?? '',
                  caseIds: [],
                  rationale: '',
                },
              ],
            })
          }
        >
          Add baseline failure
        </button>
      </details>
      <details>
        <summary>Model limits and run budgets</summary>
        <p class="settings-hint">
          Worker model: {props.model}. Cost limits use the same microunits as the explicit provider
          rates.
        </p>
        <For
          each={
            Object.keys(form().worker).filter((key) => key !== 'model_id') as Exclude<
              keyof WorkspaceSetupInput['worker'],
              'model_id'
            >[]
          }
        >
          {(key) => (
            <TextField
              label={`Worker ${title(key)}`}
              value={String(form().worker[key])}
              change={(value) => update({ worker: { ...form().worker, [key]: Number(value) } })}
            />
          )}
        </For>
        <For each={Object.keys(form().budgets)}>
          {(key) => (
            <TextField
              label={`Budget ${title(key)}`}
              value={String(form().budgets[key])}
              change={(value) => update({ budgets: { ...form().budgets, [key]: Number(value) } })}
            />
          )}
        </For>
        <TextField
          label="Run duration in milliseconds"
          value={String(form().durationMs)}
          change={(value) => update({ durationMs: Number(value) })}
        />
      </details>
      <Show when={problem()}>
        <p role="status" class="settings-status warning">
          {problem()}
        </p>
      </Show>
      <label>
        <input
          type="checkbox"
          checked={reviewed()}
          onChange={(event) => setReviewed(event.currentTarget.checked)}
        />{' '}
        I reviewed the profile facts, configuration matrix, cases, design, environment pins and
        limits.
      </label>
      <button
        type="button"
        class="chat-action-button"
        disabled={props.busy || !reviewed() || !!problem()}
        onClick={() => void props.onPrepare(form())}
      >
        Prepare this setup
      </button>
    </section>
  );
}
