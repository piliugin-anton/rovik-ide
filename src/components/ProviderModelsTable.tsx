// ProviderModelsTable.tsx
//
// The models of a provider or a connector, as a table: which are enabled
// (shown in the chat's model picker), and each one's flags and numbers.
// Shared by Settings → Model providers and Settings → Connectors: a
// connector's models have no Vision, PDF or Tools flag (the harness reads
// the project itself and runs its own tools), so the columns are the
// caller's to choose. Below the table, a model can be added by its ID.
import { createMemo, createSignal, For, Show } from 'solid-js';
import type { ProviderModel } from '../types';
import { describeReasoning, settingOf } from '../lib/reasoning';
import { formatModelLabel } from '../lib/aiSettings';

const CONTEXT_MIN = 1_000;
const CONTEXT_MAX = 2_000_000;
const DEFAULT_CONTEXT = 32_000;
const FIRST_TOKEN_MIN = 5;
const FIRST_TOKEN_MAX = 1_800;
const DEFAULT_FIRST_TOKEN = 60;

/** The columns besides the model's name and its context window. */
export interface ModelColumns {
  vision?: boolean;
  pdf?: boolean;
  tools?: boolean;
  reasoning?: boolean;
  timeout?: boolean;
}

export const PROVIDER_COLUMNS: ModelColumns = {
  vision: true,
  pdf: true,
  tools: true,
  reasoning: true,
  timeout: true,
};

export const CONNECTOR_COLUMNS: ModelColumns = { reasoning: true, timeout: true };

export default function ProviderModelsTable(props: {
  /** The provider's name, for each model's full label. */
  providerName: string;
  models: ProviderModel[];
  onChange: (models: ProviderModel[]) => void;
  columns: ModelColumns;
  /** What a model's row says on hover, when the list came with one (a connector's alias). */
  describe?: (model: ProviderModel) => string | undefined;
  /** What the empty table says before any model is listed. */
  emptyHint?: string;
}) {
  const [modelFilter, setModelFilter] = createSignal('');
  const [manualModel, setManualModel] = createSignal('');

  const patch = (id: string, change: (m: ProviderModel) => ProviderModel) =>
    props.onChange(props.models.map((m) => (m.id === id ? change(m) : m)));

  const filteredModels = createMemo(() => {
    const q = modelFilter().trim().toLowerCase();
    return q ? props.models.filter((m) => m.id.toLowerCase().includes(q)) : props.models;
  });

  const enabledCount = () => props.models.filter((m) => m.enabled).length;

  /** Empty clears the override; out-of-range values are ignored. */
  function setContextOverride(id: string, raw: string) {
    const digits = raw.replace(/[^\d]/g, '');
    const value = digits ? Number(digits) : undefined;
    const valid = value === undefined || (value >= CONTEXT_MIN && value <= CONTEXT_MAX);
    if (!valid) return;
    patch(id, (m) => ({ ...m, contextOverride: value }));
  }

  /** Seconds to wait for the first token; empty restores the 60 s default. */
  function setFirstTokenTimeout(id: string, raw: string) {
    const digits = raw.replace(/[^\d]/g, '');
    const value = digits ? Number(digits) : undefined;
    if (value !== undefined && (value < FIRST_TOKEN_MIN || value > FIRST_TOKEN_MAX)) return;
    patch(id, (m) => ({ ...m, firstTokenTimeoutSec: value }));
  }

  function setAllFiltered(enabled: boolean) {
    const ids = new Set(filteredModels().map((m) => m.id));
    props.onChange(props.models.map((m) => (ids.has(m.id) ? { ...m, enabled } : m)));
  }

  function addManualModel() {
    const id = manualModel().trim();
    if (!id) return;
    if (!props.models.some((m) => m.id === id)) {
      props.onChange([...props.models, { id, enabled: true }]);
    }
    setManualModel('');
  }

  const columnCount = () =>
    2 +
    ['vision', 'pdf', 'tools', 'reasoning', 'timeout'].filter((c) => (props.columns as any)[c])
      .length;

  return (
    <div class="provider-models" classList={{ 'connector-models': !props.columns.vision }}>
      <div class="provider-models-header">
        <span>
          Models, {enabledCount()} of {props.models.length} enabled
        </span>
        <Show when={props.models.length > 0}>
          <span class="provider-models-bulk">
            <button class="link-button" onClick={() => setAllFiltered(true)}>
              All
            </button>
            <button class="link-button" onClick={() => setAllFiltered(false)}>
              None
            </button>
          </span>
        </Show>
      </div>

      <Show when={props.models.length > 8}>
        <input
          class="provider-models-filter"
          value={modelFilter()}
          onInput={(e) => setModelFilter(e.currentTarget.value)}
          placeholder="Filter models"
          spellcheck={false}
        />
      </Show>

      <div class="provider-models-list" data-columns={columnCount()}>
        {/* Column titles: stay at the top while the list scrolls. */}
        <Show when={filteredModels().length > 0}>
          <div class="provider-models-head" role="row">
            <span role="columnheader" title="Shown in the chat's model picker when checked">
              Model
            </span>
            <span
              role="columnheader"
              class="numeric"
              title="Context window in tokens: how much of the chat is sent with each request. Empty uses the size the provider reports, or the default."
            >
              Context
            </span>
            <Show when={props.columns.vision}>
              <span
                role="columnheader"
                title="The model accepts images: images attached in the chat are sent to it, and the agent gets the browser_screenshot tool"
              >
                Vision
              </span>
            </Show>
            <Show when={props.columns.pdf}>
              <span
                role="columnheader"
                title="The model accepts PDF files: PDFs attached in the chat are sent to it. OpenAI's and Anthropic's models and OpenRouter take them (Anthropic's are set from its model list); Gemini's OpenAI-compatible API doesn't"
              >
                PDF
              </span>
            </Show>
            <Show when={props.columns.tools}>
              <span
                role="columnheader"
                title="The model can call tools (read and edit files, run commands). Turn off for models without tool-call support."
              >
                Tools
              </span>
            </Show>
            <Show when={props.columns.reasoning}>
              <span
                role="columnheader"
                title="A reasoning effort can be chosen for the model in the chat; the forms its provider accepts are detected automatically"
              >
                Reasoning
              </span>
            </Show>
            <Show when={props.columns.timeout}>
              <span
                role="columnheader"
                class="numeric"
                title="How long to wait for the first token before retrying. Each retry after no answer waits twice as long. Raise it for slow reasoning models."
              >
                Timeout
              </span>
            </Show>
            <span role="columnheader" aria-label="Remove" />
          </div>
        </Show>
        <For
          each={filteredModels()}
          fallback={
            <div class="provider-models-empty">
              {props.models.length === 0
                ? (props.emptyHint ?? 'Test the connection to load models, or add one by ID below.')
                : 'No models match the filter.'}
            </div>
          }
        >
          {(m) => (
            <div class="provider-model-row">
              <label title={props.describe?.(m) ?? formatModelLabel(props.providerName, m)}>
                <input
                  type="checkbox"
                  checked={m.enabled}
                  onChange={(e) => patch(m.id, (x) => ({ ...x, enabled: e.currentTarget.checked }))}
                />
                <span class="provider-model-name">{m.id}</span>
              </label>
              <input
                class="context-input"
                inputMode="numeric"
                value={m.contextOverride?.toLocaleString() ?? ''}
                placeholder={
                  m.contextLength
                    ? `${m.contextLength.toLocaleString()} reported`
                    : `${DEFAULT_CONTEXT.toLocaleString()} default`
                }
                title="Context window in tokens. Leave empty to use the reported value or the default."
                aria-label={`Context window for ${m.id}`}
                onChange={(e) => {
                  setContextOverride(m.id, e.currentTarget.value);
                  // Show the stored value (or empty if rejected).
                  const stored = props.models.find((x) => x.id === m.id)?.contextOverride;
                  e.currentTarget.value = stored?.toLocaleString() ?? '';
                }}
              />
              <Show when={props.columns.vision}>
                <input
                  type="checkbox"
                  class="model-flag"
                  checked={!!m.vision}
                  onChange={() => patch(m.id, (x) => ({ ...x, vision: !x.vision }))}
                  aria-label={`${m.id} accepts images`}
                  title={m.vision ? 'Accepts images' : 'No images'}
                />
              </Show>
              <Show when={props.columns.pdf}>
                <input
                  type="checkbox"
                  class="model-flag"
                  checked={!!m.pdf}
                  onChange={() => patch(m.id, (x) => ({ ...x, pdf: !x.pdf }))}
                  aria-label={`${m.id} accepts PDF files`}
                  title={m.pdf ? 'Accepts PDF files' : 'No PDF files'}
                />
              </Show>
              <Show when={props.columns.tools}>
                <input
                  type="checkbox"
                  class="model-flag"
                  checked={!m.noTools}
                  onChange={() => patch(m.id, (x) => ({ ...x, noTools: !x.noTools }))}
                  aria-label={`${m.id} can use tools`}
                  title={
                    m.noTools
                      ? 'Tools off: this model is sent no tools (for models without tool-call support)'
                      : 'Tools on. Turn off for models that go silent or fail after tool calls'
                  }
                />
              </Show>
              <Show when={props.columns.reasoning}>
                <input
                  type="checkbox"
                  class="model-flag"
                  checked={settingOf(m).choice !== 'off'}
                  onChange={() =>
                    patch(m.id, (x) => ({
                      ...x,
                      reasoning: {
                        choice:
                          settingOf(x).choice === 'off' ? ('auto' as const) : ('off' as const),
                      },
                    }))
                  }
                  aria-label={`${m.id} takes a reasoning effort`}
                  title={
                    settingOf(m).choice === 'off'
                      ? 'Reasoning off: no effort is sent to this model, and the effort selector is hidden'
                      : `Reasoning on: the effort chosen in the chat is sent in the forms this model accepts. ${describeReasoning(settingOf(m))}`
                  }
                />
              </Show>
              <Show when={props.columns.timeout}>
                <input
                  class="context-input timeout-input"
                  inputMode="numeric"
                  value={m.firstTokenTimeoutSec ? `${m.firstTokenTimeoutSec} s` : ''}
                  placeholder={`${DEFAULT_FIRST_TOKEN} s`}
                  title="How long to wait for the first token before retrying. Each retry after no answer waits twice as long. Raise it for slow reasoning models."
                  aria-label={`First-token timeout for ${m.id}, in seconds`}
                  onChange={(e) => {
                    setFirstTokenTimeout(m.id, e.currentTarget.value);
                    const stored = props.models.find((x) => x.id === m.id)?.firstTokenTimeoutSec;
                    e.currentTarget.value = stored ? `${stored} s` : '';
                  }}
                />
              </Show>
              <button
                class="icon-button"
                onClick={() => props.onChange(props.models.filter((x) => x.id !== m.id))}
                aria-label={`Remove ${m.id}`}
                title="Remove"
              >
                ×
              </button>
            </div>
          )}
        </For>
      </div>

      <div class="provider-manual-row">
        <input
          value={manualModel()}
          onInput={(e) => setManualModel(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              addManualModel();
            }
          }}
          placeholder="Add model ID manually"
          spellcheck={false}
        />
        <button onClick={addManualModel} disabled={!manualModel().trim()}>
          Add
        </button>
      </div>
    </div>
  );
}
