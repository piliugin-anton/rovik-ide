// ConnectorsSettings.tsx
//
// Settings → Connectors: the coding harnesses chats can use in place of a
// model provider (lib/connectors.ts), Claude Code first. Each connector is
// a program on the computer the chat runs on. Its form is the same for
// every kind: its name; Connection, how it signs in (the kind's own
// fields) with its test and Set up; the kind's options (connectorFields.ts);
// and its models, as the Model providers page lists them
// (ProviderModelsTable.tsx). The models enabled here show in the chat's
// model picker, marked as a connector's.
import { createResource, createSignal, For, Show } from 'solid-js';
import { Dynamic } from 'solid-js/web';
import type { ConnectorKind, Provider } from '../types';
import { mergeRemoteModels } from '../lib/aiSettings';
import {
  authLine,
  authStateOf,
  cantRun,
  checkConnectorAuth,
  connectorAuthState,
  setConnectorAuthState,
  CONNECTOR_CATALOG,
  connectorTitle,
  isConnector,
  kindOfProvider,
  newConnector,
  probeConnector,
  probeModels,
  probeSummary,
  errorText,
  specOf,
  type ConnectorProbe,
} from '../lib/connectors';
import { CONNECTOR_FIELDS } from './connectorFields';
import { confirmDialog } from './ConfirmDialog';
import ConnectorSetup from './ConnectorSetup';
import ProviderModelsTable, { CONNECTOR_COLUMNS } from './ProviderModelsTable';
import { listHosts } from '../lib/hosts';

/** Lets the settings modal show the open form's name and close it with Back. */
export interface ConnectorsSettingsController {
  /** The connector being edited, or null on the list. */
  draftTitle: () => string | null;
  closeDraft: () => void;
}

interface ConnectorsSettingsProps {
  /** Every saved entry: providers and connectors (only the connectors are this page's). */
  providers: Provider[];
  /** Every change to the list (saved form, deletion), saved at once. */
  onChange: (providers: Provider[]) => void;
  controller?: (api: ConnectorsSettingsController) => void;
}

type TestState =
  | { kind: 'idle' }
  | { kind: 'testing' }
  | { kind: 'ok'; probe: ConnectorProbe; added: number; first: boolean }
  | { kind: 'error'; message: string };

export default function ConnectorsSettings(props: ConnectorsSettingsProps) {
  const connectors = () => props.providers.filter(isConnector);
  const setConnectors = (fn: (prev: Provider[]) => Provider[]) => {
    const plain = props.providers.filter((p) => !isConnector(p));
    props.onChange([...plain, ...fn(connectors())]);
  };
  const [draft, setDraft] = createSignal<Provider | null>(null);
  const [test, setTest] = createSignal<TestState>({ kind: 'idle' });
  const [withRequest, setWithRequest] = createSignal(false);
  const [testHost, setTestHost] = createSignal('');
  const [formError, setFormError] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);
  // The list's "Set up": the form opens and sets the connector up at once.
  const [setUpOnOpen, setSetUpOnOpen] = createSignal(false);
  // The cloud hosts, for "Test on": a connector is a program on the
  // computer the chat runs on, so a host has its own installation.
  const [hostsList] = createResource(() => listHosts().catch(() => null));
  const hosts = () => hostsList()?.hosts ?? [];

  // Ignore results from a test that was started before the settings changed.
  let testRun = 0;

  const patch = (changes: Partial<Provider>) => {
    const d = draft();
    if (d) setDraft({ ...d, ...changes });
  };
  /** A change to what its runs use (settings, key): the last test no longer says. */
  const update = (changes: Partial<Provider>) => {
    patch(changes);
    testRun++;
    setTest({ kind: 'idle' });
  };

  function openDraft(p: Provider, setUp = false) {
    setSetUpOnOpen(setUp);
    setDraft({ ...p, models: p.models.map((m) => ({ ...m })) });
    setTest({ kind: 'idle' });
    setFormError(null);
    setAdding(false);
    testRun++;
  }

  function closeDraft() {
    setDraft(null);
    setAdding(false);
    testRun++;
  }

  function add(kind: ConnectorKind) {
    openDraft(newConnector(kind));
  }

  /**
   * Tests the connector being edited where it's tested, shows what it found
   * and keeps its models; throws what failed. "Set up" tests with it too.
   */
  async function testNow(): Promise<ConnectorProbe> {
    const d = draft();
    if (!d) throw new Error('No connector is open.');
    const run = ++testRun;
    setTest({ kind: 'testing' });
    try {
      const probe = await probeConnector(d, withRequest(), testHost() || null);
      const current = draft();
      if (run !== testRun || !current) return probe;
      // Existing choices are kept; only the very first fetch enables everything.
      const firstFetch = current.models.length === 0;
      const { models, added } = mergeRemoteModels(current.models, probeModels(probe));
      setDraft({ ...current, models });
      // What it found about the sign-in is this computer's: the rows and
      // the menu show it (a host's is the host's own).
      if (probe.auth && !testHost())
        setConnectorAuthState(current.id, authStateOf({ auth: probe.auth }));
      setTest({ kind: 'ok', probe, added, first: firstFetch });
      return probe;
    } catch (err) {
      if (run === testRun) setTest({ kind: 'error', message: errorText(err) });
      throw err;
    }
  }

  const runTest = () => testNow().catch(() => {});
  const testWhere = () =>
    testHost()
      ? `on ${hosts().find((h) => h.id === testHost())?.name ?? 'the host'}`
      : 'on this computer';

  const okState = () => {
    const t = test();
    return t.kind === 'ok' ? t : null;
  };
  const errorState = () => {
    const t = test();
    return t.kind === 'error' ? t : null;
  };
  const describe = (m: { id: string }) =>
    okState()?.probe.models.find((x) => x.id === m.id)?.description ?? undefined;

  function saveDraft() {
    const d = draft();
    if (!d) return;
    const name = d.name.trim();
    if (!name) {
      setFormError('A name is required.');
      return;
    }
    const settings = specOf(d)?.settings(d.settings ?? {}) ?? d.settings;
    const clean: Provider = { ...d, name, settings };
    setConnectors((prev) =>
      prev.some((p) => p.id === clean.id)
        ? prev.map((p) => (p.id === clean.id ? clean : p))
        : [...prev, clean]
    );
    closeDraft();
    // Its sign-in may have changed with its settings (a token, a folder).
    void checkConnectorAuth(clean);
  }

  async function remove(connector: Provider) {
    const enabled = connector.models.filter((m) => m.enabled).length;
    const confirmed = await confirmDialog({
      title: 'Remove this connector?',
      message: (
        <>
          <strong>{connector.name}</strong>
          {connector.models.length > 0
            ? ` and its ${connector.models.length} model${connector.models.length === 1 ? '' : 's'}${
                enabled > 0 ? ` (${enabled} enabled)` : ''
              }`
            : ''}{' '}
          will be removed from Rovik. The program stays installed; chats using it will need another
          model.
        </>
      ),
      confirmLabel: 'Remove',
      danger: true,
    });
    if (confirmed) setConnectors((prev) => prev.filter((p) => p.id !== connector.id));
  }

  props.controller?.({
    draftTitle: () => {
      const d = draft();
      return d ? d.name || 'New connector' : null;
    },
    closeDraft,
  });

  return (
    <Show
      when={draft()}
      fallback={
        <>
          <p class="settings-hint">
            A connector is a coding harness installed on the computer a chat runs on, used in place
            of a model provider: the chat hands it the task, picks one of its models, and shows what
            it did, its tokens and its cost with the reply. Run as itself, the harness works in the
            project folder with its own tools and prompt, under the permission mode set here: by
            default, Rovik’s Permissions & approval decide each call it would ask about, and an
            approval waits in the chat. Run as Rovik’s agent, it gets Rovik’s prompt and tools (via
            MCP), and Rovik runs each call under Permissions & approval. On a cloud host, the host
            needs its own installation.
          </p>
          <For
            each={connectors()}
            fallback={
              <div class="empty-state">
                No connectors yet. Connect Claude Code to use it in chats.
              </div>
            }
          >
            {(c) => (
              <div class="profile-row connector-row">
                <div class="profile-row-info">
                  <span class="profile-row-label">{c.name}</span>
                  <span class="profile-row-sub">
                    {connectorTitle(kindOfProvider(c))} · {specOf(c)?.summary(c.settings ?? {})}
                  </span>
                  <span class="profile-row-sub">
                    {c.models.filter((m) => m.enabled).length} of {c.models.length} models enabled
                  </span>
                  <Show when={authLine(connectorAuthState(c.id))}>
                    {(line) => (
                      <span
                        class="profile-row-sub connector-auth"
                        classList={{ warning: line().warning }}
                        title={line().text}
                      >
                        {line().text}
                      </span>
                    )}
                  </Show>
                </div>
                <div class="profile-row-actions">
                  <button
                    onClick={() => void checkConnectorAuth(c)}
                    disabled={connectorAuthState(c.id)?.state === 'checking'}
                    title="Whether it's signed in on this computer (asks the program; no request is made)"
                  >
                    Check sign-in
                  </button>
                  <Show when={cantRun(connectorAuthState(c.id))}>
                    <button
                      class="primary"
                      onClick={() => openDraft(c, true)}
                      title="Installs it if it's missing and signs it in, on this computer"
                    >
                      Set up
                    </button>
                  </Show>
                  <button onClick={() => openDraft(c)}>Edit</button>
                  <button class="danger" onClick={() => void remove(c)}>
                    Remove
                  </button>
                </div>
              </div>
            )}
          </For>
          <Show
            when={adding()}
            fallback={
              <button class="add-profile-button" onClick={() => setAdding(true)}>
                + Add connector
              </button>
            }
          >
            <div class="connector-catalog">
              <For each={CONNECTOR_CATALOG}>
                {(entry) => (
                  <button class="connector-catalog-item" onClick={() => add(entry.kind)}>
                    <span class="profile-row-label">{entry.title}</span>
                    <span class="profile-row-sub connector-catalog-about">{entry.about}</span>
                  </button>
                )}
              </For>
              <button class="link-button" onClick={() => setAdding(false)}>
                Cancel
              </button>
            </div>
          </Show>
        </>
      }
    >
      {(d) => (
        <div class="profile-form connector-form">
          <label>
            Name
            <input
              value={d().name}
              onInput={(e) => patch({ name: e.currentTarget.value })}
              placeholder={connectorTitle(kindOfProvider(d()))}
            />
          </label>

          <section class="settings-section">
            <div class="settings-section-title">Connection</div>
            <Show when={kindOfProvider(d())}>
              {(kind) => (
                <Dynamic
                  component={CONNECTOR_FIELDS[kind()].Connection}
                  connector={d()}
                  update={update}
                />
              )}
            </Show>

            <div class="provider-test-row connector-test-row">
              <button onClick={() => void runTest()} disabled={test().kind === 'testing'}>
                {test().kind === 'testing'
                  ? 'Testing…'
                  : d().models.length > 0
                    ? 'Test & refresh models'
                    : 'Test connection'}
              </button>
              <label
                class="checkbox-row connector-test-option"
                title="Makes one small request too, to see that it's signed in (a few tokens)"
              >
                <input
                  type="checkbox"
                  checked={withRequest()}
                  onChange={(e) => setWithRequest(e.currentTarget.checked)}
                />
                with a request
              </label>
              <Show when={hosts().length > 0}>
                <select
                  class="settings-select connector-test-host"
                  value={testHost()}
                  onChange={(e) => setTestHost(e.currentTarget.value)}
                  aria-label="Where to test"
                  title="A connector is a program on the computer the chat runs on: test it where the chats run"
                >
                  <option value="">on this computer</option>
                  <For each={hosts()}>{(h) => <option value={h.id}>on {h.name}</option>}</For>
                </select>
              </Show>
            </div>
            <Show when={okState()}>
              {(ok) => (
                <div class="provider-test-status ok connector-test-status">
                  {probeSummary(ok().probe)}, {ok().probe.models.length} models
                  {!ok().first && ok().added > 0 ? ` (${ok().added} new, not enabled)` : ''}
                  <Show when={ok().probe.auth && authLine(authStateOf({ auth: ok().probe.auth! }))}>
                    {(line) => (
                      <div class="connector-auth" classList={{ warning: line().warning }}>
                        {line().text}
                      </div>
                    )}
                  </Show>
                  <Show when={ok().probe.update}>
                    {(version) => (
                      <div class="connector-test-note">
                        Version {version()} is available: Update below.
                      </div>
                    )}
                  </Show>
                  <For each={ok().probe.notes ?? []}>
                    {(note) => <div class="connector-test-note">{note}</div>}
                  </For>
                </div>
              )}
            </Show>
            <Show when={errorState()}>
              {(err) => (
                <div class="provider-test-status error connector-test-status" title={err().message}>
                  {err().message}
                </div>
              )}
            </Show>
            <ConnectorSetup
              connector={d()}
              title={connectorTitle(kindOfProvider(d()))}
              host={testHost() || null}
              where={testWhere()}
              probe={testNow}
              signedIn={okState()?.probe.auth?.loggedIn === true}
              update={okState()?.probe.update}
              autoStart={setUpOnOpen()}
            />
          </section>

          <Show when={kindOfProvider(d())}>
            {(kind) => (
              <Dynamic
                component={CONNECTOR_FIELDS[kind()].Options}
                connector={d()}
                update={update}
              />
            )}
          </Show>

          <ProviderModelsTable
            providerName={d().name}
            models={d().models}
            onChange={(models) => patch({ models })}
            columns={CONNECTOR_COLUMNS}
            describe={describe}
            emptyHint="Test the connection to list its models (aliases such as sonnet and opus, and full names), or add one by ID below."
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
