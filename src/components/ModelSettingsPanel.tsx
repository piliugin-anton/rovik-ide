import { createMemo, createSignal, For, Show } from 'solid-js';
import type { Provider, ProviderModel } from '../types';
import {
  fetchModels,
  formatModelLabel,
  guessProviderName,
  providerDomainLabel,
} from '../lib/aiSettings';

interface ModelSettingsPanelProps {
  providers: Provider[];
  onSave: (providers: Provider[]) => void;
  onClose: () => void;
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

const emptyDraft = (): Provider => ({
  id: crypto.randomUUID(),
  name: '',
  baseUrl: '',
  apiKey: '',
  stream: true,
  models: [],
});

export default function ModelSettingsPanel(props: ModelSettingsPanelProps) {
  const [providers, setProviders] = createSignal<Provider[]>(props.providers);
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

  function removeProvider(id: string) {
    setProviders((prev) => prev.filter((p) => p.id !== id));
  }

  function commitAndClose() {
    props.onSave(providers());
    props.onClose();
  }

  return (
    <div class="modal-overlay" onClick={commitAndClose}>
      <div class="modal model-settings" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <span>{draft() ? draft()!.name || 'New provider' : 'Model providers'}</span>
          <button class="icon-button" onClick={commitAndClose} aria-label="Close">
            ×
          </button>
        </div>

        <div class="modal-body">
          <Show
            when={draft()}
            fallback={
              <>
                <For
                  each={providers()}
                  fallback={
                    <div class="empty-state">No providers yet. Add one to start chatting.</div>
                  }
                >
                  {(p) => (
                    <div class="profile-row">
                      <div class="profile-row-info">
                        <span class="profile-row-label">{p.name}</span>
                        <span class="profile-row-sub" title={p.baseUrl}>
                          {p.baseUrl}
                        </span>
                        <span class="profile-row-sub">
                          {p.models.filter((m) => m.enabled).length} of {p.models.length} models
                          enabled
                        </span>
                      </div>
                      <div class="profile-row-actions">
                        <button onClick={() => openDraft(p, false)}>Edit</button>
                        <button class="danger" onClick={() => removeProvider(p.id)}>
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
                          <label title={formatModelLabel(providerDomainLabel(d()), m)}>
                            <input
                              type="checkbox"
                              checked={m.enabled}
                              onChange={(e) => toggleModel(m.id, e.currentTarget.checked)}
                            />
                            <span>{m.id}</span>
                          </label>
                          <button
                            class="vision-toggle"
                            classList={{ on: !!m.vision }}
                            onClick={() => toggleVision(m.id)}
                            aria-pressed={!!m.vision}
                            title={
                              m.vision
                                ? 'Accepts images: the agent can take browser screenshots'
                                : 'Mark as accepting images to enable browser screenshots'
                            }
                          >
                            vision
                          </button>
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
        </div>
      </div>
    </div>
  );
}
