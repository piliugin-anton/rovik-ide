// ModelProvidersSettings.tsx
//
// Settings → Model providers: the OpenAI-compatible endpoints chats can
// use, each with its API key and its models (ProviderModelsTable.tsx).
// Connectors, harnesses such as Claude Code, have their own page
// (ConnectorsSettings.tsx); the entries here are the providers alone.
import { createSignal, For, Show } from 'solid-js';
import type { Provider } from '../types';
import { fetchModels, guessProviderName, mergeRemoteModels } from '../lib/aiSettings';
import { isConnector } from '../lib/connectors';
import { confirmDialog } from './ConfirmDialog';
import ProviderModelsTable, { PROVIDER_COLUMNS } from './ProviderModelsTable';

/** Lets the settings modal show the open form's name and close it with Back. */
export interface ProvidersSettingsController {
  /** The provider being edited, or null on the list. */
  draftTitle: () => string | null;
  closeDraft: () => void;
}

interface ModelProvidersSettingsProps {
  providers: Provider[];
  /** Every change to the list (saved form, deletion), saved at once. */
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

const emptyDraft = (): Provider => ({
  id: crypto.randomUUID(),
  name: '',
  baseUrl: '',
  apiKey: '',
  stream: true,
  models: [],
});

export default function ModelProvidersSettings(props: ModelProvidersSettingsProps) {
  // The list is the dialog's state (lib/aiSettingsStore.ts): every change
  // goes up and comes back down, saved. Connectors are among the saved
  // entries (lib/connectors.ts) but not this page's: they're kept as they are.
  const providers = () => props.providers.filter((p) => !isConnector(p));
  const setProviders = (fn: (prev: Provider[]) => Provider[]) => {
    const connectors = props.providers.filter(isConnector);
    props.onChange([...fn(providers()), ...connectors]);
  };
  const [draft, setDraft] = createSignal<Provider | null>(null);
  const [nameTouched, setNameTouched] = createSignal(false);
  const [test, setTest] = createSignal<TestState>({ kind: 'idle' });
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

      // Existing choices are kept; only the very first fetch enables everything.
      const firstFetch = current.models.length === 0;
      const { models, added } = mergeRemoteModels(current.models, remote);

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
          <p class="settings-hint">
            Each provider is an OpenAI-compatible endpoint with your API key, on this computer. The
            models you enable show in the chat’s model picker and can be chosen for the AI reviewer
            and the subagents. A model’s flags say what it takes: images (Vision) and PDFs attached
            in the chat go only to models marked for them, Tools off sends it no tools, and
            Reasoning on lets the chat choose an effort.
          </p>
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

          <ProviderModelsTable
            providerName={d().name}
            models={d().models}
            onChange={(models) => patch({ models })}
            columns={PROVIDER_COLUMNS}
          />

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
