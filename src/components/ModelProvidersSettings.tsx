import { describeReasoning, settingOf } from '../lib/reasoning';
import { createMemo, createSignal, For, Show } from 'solid-js';
import type { Provider, ProviderModel } from '../types';
import { fetchModels, formatModelLabel, guessProviderName } from '../lib/aiSettings';
import { confirmDialog } from './ConfirmDialog';

/** Lets the settings modal show the open form's name and close it with Back. */
export interface ProvidersSettingsController {
  /** The provider being edited, or null on the list. */
  draftTitle: () => string | null;
  closeDraft: () => void;
}

interface ModelProvidersSettingsProps {
  providers: Provider[];
  /** Every change to the list (saved form, deletion); stored when the modal closes. */
  onChange: (providers: Provider[]) => void;
  controller?: (api: ProvidersSettingsController) => void;
}

type TestState =
  | { kind: 'idle' }
  | { kind: 'testing' }
  | { kind: 'ok'; total: number; added: number; first: boolean }
  | { kind: 'error'; message: string };

const BASE_URL_PRESETS = [
  'https://api.openai.com/v1',
  'https://openrouter.ai/api/v1',
  'https://api.groq.com/openai/v1',
  'https://api.deepseek.com/v1',
  'https://api.mistral.ai/v1',
  'https://api.x.ai/v1',
  'http://localhost:11434/v1',
  'http://localhost:1234/v1',
];

const CONTEXT_MIN = 1_000;
const CONTEXT_MAX = 2_000_000;
const DEFAULT_CONTEXT = 32_000;
const FIRST_TOKEN_MIN = 5;
const FIRST_TOKEN_MAX = 1_800;
const DEFAULT_FIRST_TOKEN = 60;

const emptyDraft = (): Provider => ({
  id: crypto.randomUUID(),
  name: '',
  baseUrl: '',
  apiKey: '',
  stream: true,
  models: [],
});

export default function ModelProvidersSettings(props: ModelProvidersSettingsProps) {
  const [providers, setProvidersSignal] = createSignal<Provider[]>(props.providers);
  const setProviders = (fn: (prev: Provider[]) => Provider[]) => {
    const next = fn(providers());
    setProvidersSignal(next);
    props.onChange(next);
  };
  const [draft, setDraft] = createSignal<Provider | null>(null);
  const [nameTouched, setNameTouched] = createSignal(false);
  const [test, setTest] = createSignal<TestState>({ kind: 'idle' });
  const [modelFilter, setModelFilter] = createSignal('');
  const [manualModel, setManualModel] = createSignal('');
  const [formError, setFormError] = createSignal<string | null>(null);

  // Ignore results from a test that was started before the URL/key changed.
  let testRun = 0;

  const patch = (changes: Partial<Provider>) => {
    const d = draft();
    if (d) setDraft({ ...d, ...changes });
  };

  function openDraft(p: Provider, isNew: boolean) {
    setDraft({ ...p, models: p.models.map((m) => ({ ...m })) });
    setNameTouched(!isNew);
    setTest({ kind: 'idle' });
    setModelFilter('');
    setManualModel('');
    setFormError(null);
    testRun++;
  }

  function closeDraft() {
    setDraft(null);
    testRun++;
  }

  function updateConnection(changes: Partial<Pick<Provider, 'baseUrl' | 'apiKey'>>) {
    const next: Partial<Provider> = { ...changes };
    if (changes.baseUrl !== undefined && !nameTouched()) {
      next.name = guessProviderName(changes.baseUrl) ?? '';
    }
    patch(next);
    testRun++;
    setTest({ kind: 'idle' });
  }

  async function runTest() {
    const d = draft();
    if (!d || !d.baseUrl.trim()) {
      setTest({ kind: 'error', message: 'Enter a base URL first.' });
      return;
    }

    const run = ++testRun;
    setTest({ kind: 'testing' });

    try {
      const remote = await fetchModels(d.baseUrl.trim(), d.apiKey);
      if (run !== testRun) return;

      const current = draft();
      if (!current) return;

      // Keep existing choices; on the very first fetch enable everything,
      // afterwards new models arrive disabled so a refresh doesn't flood the picker.
      const firstFetch = current.models.length === 0;
      const known = new Map(current.models.map((m) => [m.id, m]));
      let added = 0;

      const merged: ProviderModel[] = remote.map((r) => {
        const existing = known.get(r.id);
        known.delete(r.id);
        const contextLength = r.context_length ?? undefined;
        if (existing) {
          return {
            ...existing,
            ownedBy: r.owned_by ?? existing.ownedBy,
            contextLength: contextLength ?? existing.contextLength,
          };
        }
        added++;
        return { id: r.id, ownedBy: r.owned_by ?? undefined, contextLength, enabled: firstFetch };
      });

      // Models added by hand that the server didn't list stay put.
      const models = [...merged, ...known.values()];

      setDraft({ ...current, models });
      setTest({ kind: 'ok', total: remote.length, added, first: firstFetch });
    } catch (err) {
      if (run !== testRun) return;
      setTest({ kind: 'error', message: String(err) });
    }
  }

  const okState = () => {
    const t = test();
    return t.kind === 'ok' ? t : null;
  };
  const errorState = () => {
    const t = test();
    return t.kind === 'error' ? t : null;
  };

  const filteredModels = createMemo(() => {
    const d = draft();
    if (!d) return [];
    const q = modelFilter().trim().toLowerCase();
    return q ? d.models.filter((m) => m.id.toLowerCase().includes(q)) : d.models;
  });

  const enabledCount = () => draft()?.models.filter((m) => m.enabled).length ?? 0;

  function toggleModel(id: string, enabled: boolean) {
    const d = draft();
    if (!d) return;
    patch({ models: d.models.map((m) => (m.id === id ? { ...m, enabled } : m)) });
  }

  /** Empty clears the override; out-of-range values are ignored. */
  function setContextOverride(id: string, raw: string) {
    const d = draft();
    if (!d) return;
    const digits = raw.replace(/[^\d]/g, '');
    const value = digits ? Number(digits) : undefined;
    const valid = value === undefined || (value >= CONTEXT_MIN && value <= CONTEXT_MAX);
    if (!valid) return;
    patch({ models: d.models.map((m) => (m.id === id ? { ...m, contextOverride: value } : m)) });
  }

  /**
   * Reasoning on (the effort is sent, in the forms detected for the model)
   * or off (never sent). Switching it on starts detection afresh.
   */
  function toggleReasoning(id: string) {
    const d = draft();
    if (!d) return;
    patch({
      models: d.models.map((m) =>
        m.id === id
          ? {
              ...m,
              reasoning: {
                choice: settingOf(m).choice === 'off' ? ('auto' as const) : ('off' as const),
              },
            }
          : m
      ),
    });
  }

  function toggleTools(id: string) {
    const d = draft();
    if (!d) return;
    patch({ models: d.models.map((m) => (m.id === id ? { ...m, noTools: !m.noTools } : m)) });
  }

  /** Seconds to wait for the first token; empty restores the 60 s default. */
  function setFirstTokenTimeout(id: string, raw: string) {
    const d = draft();
    if (!d) return;
    const digits = raw.replace(/[^\d]/g, '');
    const value = digits ? Number(digits) : undefined;
    if (value !== undefined && (value < FIRST_TOKEN_MIN || value > FIRST_TOKEN_MAX)) return;
    patch({
      models: d.models.map((m) => (m.id === id ? { ...m, firstTokenTimeoutSec: value } : m)),
    });
  }

  function toggleVision(id: string) {
    const d = draft();
    if (!d) return;
    patch({ models: d.models.map((m) => (m.id === id ? { ...m, vision: !m.vision } : m)) });
  }

  function setAllFiltered(enabled: boolean) {
    const d = draft();
    if (!d) return;
    const ids = new Set(filteredModels().map((m) => m.id));
    patch({ models: d.models.map((m) => (ids.has(m.id) ? { ...m, enabled } : m)) });
  }

  function removeModel(id: string) {
    const d = draft();
    if (d) patch({ models: d.models.filter((m) => m.id !== id) });
  }

  function addManualModel() {
    const id = manualModel().trim();
    const d = draft();
    if (!id || !d) return;
    if (!d.models.some((m) => m.id === id)) {
      patch({ models: [...d.models, { id, enabled: true }] });
    }
    setManualModel('');
  }

  function saveDraft() {
    const d = draft();
    if (!d) return;

    const name = d.name.trim();
    const baseUrl = d.baseUrl.trim();
    if (!name || !baseUrl) {
      setFormError('Name and base URL are required.');
      return;
    }

    const clean: Provider = { ...d, name, baseUrl };
    setProviders((prev) =>
      prev.some((p) => p.id === clean.id)
        ? prev.map((p) => (p.id === clean.id ? clean : p))
        : [...prev, clean]
    );
    closeDraft();
  }

  async function removeProvider(provider: Provider) {
    const enabled = provider.models.filter((m) => m.enabled).length;
    const confirmed = await confirmDialog({
      title: 'Delete this provider?',
      message: (
        <>
          <strong>{provider.name || provider.baseUrl}</strong>
          {provider.models.length > 0
            ? ` and its ${provider.models.length} model${provider.models.length === 1 ? '' : 's'}${
                enabled > 0 ? ` (${enabled} enabled)` : ''
              }`
            : ''}{' '}
          will be removed. Chats using it will need another model.
        </>
      ),
      confirmLabel: 'Delete',
      danger: true,
    });
    if (confirmed) setProviders((prev) => prev.filter((p) => p.id !== provider.id));
  }

  props.controller?.({
    draftTitle: () => {
      const d = draft();
      return d ? d.name || 'New provider' : null;
    },
    closeDraft,
  });

  return (
    <Show
      when={draft()}
      fallback={
        <>
          <For
            each={providers()}
            fallback={<div class="empty-state">No providers yet. Add one to start chatting.</div>}
          >
            {(p) => (
              <div class="profile-row">
                <div class="profile-row-info">
                  <span class="profile-row-label">{p.name}</span>
                  <span class="profile-row-sub" title={p.baseUrl}>
                    {p.baseUrl}
                  </span>
                  <span class="profile-row-sub">
                    {p.models.filter((m) => m.enabled).length} of {p.models.length} models enabled
                  </span>
                </div>
                <div class="profile-row-actions">
                  <button onClick={() => openDraft(p, false)}>Edit</button>
                  <button class="danger" onClick={() => void removeProvider(p)}>
                    Delete
                  </button>
                </div>
              </div>
            )}
          </For>
          <button class="add-profile-button" onClick={() => openDraft(emptyDraft(), true)}>
            + Add provider
          </button>
        </>
      }
    >
      {(d) => (
        <div class="profile-form">
          <label>
            Base URL
            <input
              value={d().baseUrl}
              list="provider-base-urls"
              onInput={(e) => updateConnection({ baseUrl: e.currentTarget.value })}
              placeholder="https://api.openai.com/v1"
              spellcheck={false}
            />
            <datalist id="provider-base-urls">
              <For each={BASE_URL_PRESETS}>{(url) => <option value={url} />}</For>
            </datalist>
          </label>

          <label>
            API key
            <input
              type="password"
              value={d().apiKey}
              onInput={(e) => updateConnection({ apiKey: e.currentTarget.value })}
              placeholder="sk-...  (leave empty for local servers)"
              spellcheck={false}
            />
          </label>

          <label>
            Name
            <input
              value={d().name}
              onInput={(e) => {
                setNameTouched(true);
                patch({ name: e.currentTarget.value });
              }}
              placeholder="e.g. OpenRouter"
            />
          </label>

          <label class="checkbox-row">
            <input
              type="checkbox"
              checked={d().stream}
              onChange={(e) => patch({ stream: e.currentTarget.checked })}
            />
            Stream responses
          </label>

          <div class="provider-test-row">
            <button onClick={() => void runTest()} disabled={test().kind === 'testing'}>
              {test().kind === 'testing'
                ? 'Testing…'
                : d().models.length > 0
                  ? 'Test & refresh models'
                  : 'Test connection'}
            </button>
            <Show when={okState()}>
              {(ok) => (
                <span class="provider-test-status ok">
                  Connected, {ok().total} models
                  {!ok().first && ok().added > 0 ? ` (${ok().added} new, not enabled)` : ''}
                </span>
              )}
            </Show>
            <Show when={errorState()}>
              {(err) => (
                <span class="provider-test-status error" title={err().message}>
                  {err().message}
                </span>
              )}
            </Show>
          </div>

          <div class="provider-models">
            <div class="provider-models-header">
              <span>
                Models, {enabledCount()} of {d().models.length} enabled
              </span>
              <Show when={d().models.length > 0}>
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

            <Show when={d().models.length > 8}>
              <input
                class="provider-models-filter"
                value={modelFilter()}
                onInput={(e) => setModelFilter(e.currentTarget.value)}
                placeholder="Filter models"
                spellcheck={false}
              />
            </Show>

            <div class="provider-models-list">
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
                  <span
                    role="columnheader"
                    title="The model accepts images (enables screenshots for the agent)"
                  >
                    Vision
                  </span>
                  <span
                    role="columnheader"
                    title="The model can call tools (read and edit files, run commands). Turn off for models without tool-call support."
                  >
                    Tools
                  </span>
                  <span
                    role="columnheader"
                    title="A reasoning effort can be chosen for the model in the chat; the forms its provider accepts are detected automatically"
                  >
                    Reasoning
                  </span>
                  <span
                    role="columnheader"
                    class="numeric"
                    title="How long to wait for the first token before retrying. Raise it for slow reasoning models."
                  >
                    Timeout
                  </span>
                  <span role="columnheader" aria-label="Remove" />
                </div>
              </Show>
              <For
                each={filteredModels()}
                fallback={
                  <div class="provider-models-empty">
                    {d().models.length === 0
                      ? 'Test the connection to load models, or add one by ID below.'
                      : 'No models match the filter.'}
                  </div>
                }
              >
                {(m) => (
                  <div class="provider-model-row">
                    <label title={formatModelLabel(d().name, m)}>
                      <input
                        type="checkbox"
                        checked={m.enabled}
                        onChange={(e) => toggleModel(m.id, e.currentTarget.checked)}
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
                        const stored = draft()?.models.find((x) => x.id === m.id)?.contextOverride;
                        e.currentTarget.value = stored?.toLocaleString() ?? '';
                      }}
                    />
                    <input
                      type="checkbox"
                      class="model-flag"
                      checked={!!m.vision}
                      onChange={() => toggleVision(m.id)}
                      aria-label={`${m.id} accepts images`}
                      title={m.vision ? 'Accepts images' : 'Text only'}
                    />
                    <input
                      type="checkbox"
                      class="model-flag"
                      checked={!m.noTools}
                      onChange={() => toggleTools(m.id)}
                      aria-label={`${m.id} can use tools`}
                      title={
                        m.noTools
                          ? 'Tools off: this model is sent no tools (for models without tool-call support)'
                          : 'Tools on. Turn off for models that go silent or fail after tool calls'
                      }
                    />
                    <input
                      type="checkbox"
                      class="model-flag"
                      checked={settingOf(m).choice !== 'off'}
                      onChange={() => toggleReasoning(m.id)}
                      aria-label={`${m.id} takes a reasoning effort`}
                      title={
                        settingOf(m).choice === 'off'
                          ? 'Reasoning off: no effort is sent to this model, and the effort selector is hidden'
                          : `Reasoning on: the effort chosen in the chat is sent in the forms this model accepts. ${describeReasoning(settingOf(m))}`
                      }
                    />
                    <input
                      class="context-input timeout-input"
                      inputMode="numeric"
                      value={m.firstTokenTimeoutSec ? `${m.firstTokenTimeoutSec} s` : ''}
                      placeholder={`${DEFAULT_FIRST_TOKEN} s`}
                      title="How long to wait for the first token before retrying. Raise it for slow reasoning models."
                      aria-label={`First-token timeout for ${m.id}, in seconds`}
                      onChange={(e) => {
                        setFirstTokenTimeout(m.id, e.currentTarget.value);
                        const stored = draft()?.models.find(
                          (x) => x.id === m.id
                        )?.firstTokenTimeoutSec;
                        e.currentTarget.value = stored ? `${stored} s` : '';
                      }}
                    />
                    <button
                      class="icon-button"
                      onClick={() => removeModel(m.id)}
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

          <Show when={formError()}>
            <div class="provider-test-status error">{formError()}</div>
          </Show>

          <div class="profile-form-actions">
            <button onClick={closeDraft}>Cancel</button>
            <button class="primary" onClick={saveDraft}>
              Save
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
