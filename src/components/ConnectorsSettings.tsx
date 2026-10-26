// ConnectorsSettings.tsx
//
// Settings → Connectors: the coding harnesses chats can use in place of a
// model provider (lib/connectors.ts), Claude Code or Codex. Each connector is
// a program on the computer the chat runs on. Its form is the same for
// every kind: its name; Connection, how it signs in (the kind's own
// fields) with its test and Set up; the kind's options (connectorFields.ts);
// and its models, as the Model providers page lists them
// (ProviderModelsTable.tsx). The models enabled here show in the chat's
// model picker, marked as a connector's. What each connector's last check
// found (lib/connectors.ts: installed, signed in, a newer version) shows at
// once: the list checks each again as it opens, and a connector's form
// tests it again, with no request, as it opens and on another computer.
import { createResource, createSignal, For, onMount, Show } from 'solid-js';
import { Dynamic } from 'solid-js/web';
import type { ConnectorKind, Provider } from '../types';
import { mergeRemoteModels } from '../lib/aiSettings';
import {
  authLine,
  authStateOf,
  cantRun,
  checkConnector,
  connectorAuthState,
  setConnectorAuthState,
  CONNECTOR_CATALOG,
  connectorTitle,
  isConnector,
  kindOfProvider,
  lastFound,
  newConnector,
  probeConnector,
  probeModels,
  probeSummary,
  errorText,
  specOf,
  versionLine,
  type ConnectorProbe,
} from '../lib/connectors';
import { CONNECTOR_FIELDS } from './connectorFields';
import { confirmDialog } from './ConfirmDialog';
import ConnectorSetup from './ConnectorSetup';
import { SettingRow } from './SettingsControls';
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

/** What a test found: the form shows it, and offers Update and Sign out by it. */
type Tested = { kind: 'ok'; probe: ConnectorProbe; added: number; first: boolean };

type TestState =
  | { kind: 'idle' }
  /** A test runs: what the last one found stays shown meanwhile. */
  | { kind: 'testing'; last?: Tested }
  | Tested
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
  // The computer the open form tests and sets up on (its Computer row): a
  // cloud host's id, or '' for this computer.
  const [testHost, setTestHost] = createSignal('');
  const [formError, setFormError] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);
  // The list's "Set up": the form opens and sets the connector up at once.
  const [setUpOnOpen, setSetUpOnOpen] = createSignal(false);
  // The cloud hosts, for "Computer": a connector is a program on the
  // computer the chat runs on, so a host has its own installation.
  const [hostsList] = createResource(() => listHosts().catch(() => null));
  const hosts = () => hostsList()?.hosts ?? [];

  // As the page opens, each connector is checked again on this computer,
  // with no request: its row shows what the last check found meanwhile.
  onMount(() => {
    for (const c of connectors()) void checkConnector(c);
  });

  // Ignore results from a test that was started before the settings or the
  // computer changed.
  let testRun = 0;

  /** The last test no longer says: it's forgotten, and one still running is ignored. */
  function forgetTest() {
    testRun++;
    setTest({ kind: 'idle' });
  }

  const patch = (changes: Partial<Provider>) => {
    const d = draft();
    if (d) setDraft({ ...d, ...changes });
  };
  /** A change to what its runs use (settings, key): the last test no longer says. */
  const update = (changes: Partial<Provider>) => {
    patch(changes);
    forgetTest();
  };

  /** Another computer to test and set up on: the last test was the other one's, so it tests there. */
  function pickComputer(host: string) {
    forgetTest();
    setTestHost(host);
    void checkNow();
  }

  function openDraft(p: Provider, setUp = false) {
    // The last form's test is forgotten first: a Set up that starts as the
    // form opens tests at once, and its test is this form's.
    forgetTest();
    // Each form starts on this computer: the list's rows, and the Set up
    // they open, are this computer's, whatever host the last form picked.
    setTestHost('');
    // What the last check here found shows at once, while the form tests
    // again (Set up tests as it starts).
    const known = lastFound(connectorAuthState(p.id))?.probe;
    if (known) setTest({ kind: 'ok', probe: known, added: 0, first: false });
    setSetUpOnOpen(setUp);
    setDraft({ ...p, models: p.models.map((m) => ({ ...m })) });
    setFormError(null);
    setAdding(false);
    if (!setUp) void checkNow();
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
   * and keeps its models; throws what failed, and shows it as the test's
   * result when `showFailure`. "Set up" tests with it too, without: what
   * failed is its own to act on (a missing program, it installs) or to show.
   * With `request`, it makes one small request too (a few tokens).
   */
  async function testNow(showFailure: boolean, request = withRequest()): Promise<ConnectorProbe> {
    const d = draft();
    if (!d) throw new Error('No connector is open.');
    const run = ++testRun;
    // The computer it runs on: what it finds is that computer's.
    const host = testHost() || null;
    setTest((t) => ({
      kind: 'testing',
      last: t.kind === 'ok' ? t : t.kind === 'testing' ? t.last : undefined,
    }));
    try {
      const probe = await probeConnector(d, request, host);
      const current = draft();
      if (run !== testRun || !current) return probe;
      // Existing choices are kept; only the very first fetch enables everything.
      const firstFetch = current.models.length === 0;
      const { models, added } = mergeRemoteModels(current.models, probeModels(probe));
      setDraft({ ...current, models });
      // What it found here is this computer's status: the rows and the menu
      // show it (a host's is the host's own).
      if (!host) setConnectorAuthState(current.id, authStateOf({ probe }));
      setTest({ kind: 'ok', probe, added, first: firstFetch });
      return probe;
    } catch (err) {
      if (run === testRun) {
        if (!host) setConnectorAuthState(d.id, authStateOf({ error: err }));
        setTest(showFailure ? { kind: 'error', message: errorText(err) } : { kind: 'idle' });
      }
      throw err;
    }
  }

  const runTest = () => testNow(true).catch(() => {});
  /**
   * The form's own test, as it opens and on another computer: what's
   * installed there and whether it's signed in. No request, so no tokens.
   */
  const checkNow = () => testNow(true, false).catch(() => {});
  const testWhere = () =>
    testHost()
      ? `on ${hosts().find((h) => h.id === testHost())?.name ?? 'the host'}`
      : 'on this computer';

  /** What the last test found, also while another runs. */
  const okState = () => {
    const t = test();
    return t.kind === 'ok' ? t : t.kind === 'testing' ? (t.last ?? null) : null;
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
    // Its status may have changed with its settings (a token, a folder, the program).
    void checkConnector(clean);
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
                No connectors yet. Connect Claude Code or Codex to use it in chats.
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
                  <Show when={versionLine(connectorAuthState(c.id))}>
                    {(line) => <span class="profile-row-sub">{line()}</span>}
                  </Show>
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
                    onClick={() => void checkConnector(c)}
                    disabled={connectorAuthState(c.id)?.state === 'checking'}
                    title="Whether it's installed and signed in on this computer, and whether a newer version is out (asks the program; no request is made)"
                  >
                    Check status
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
          <section class="settings-section connector-name">
            <SettingRow label="Name">
              <input
                class="settings-input"
                aria-label="Name"
                value={d().name}
                onInput={(e) => patch({ name: e.currentTarget.value })}
                placeholder={connectorTitle(kindOfProvider(d()))}
              />
            </SettingRow>
          </section>

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
            <Show when={hosts().length > 0}>
              <SettingRow label="Computer">
                <select
                  class="settings-select"
                  value={testHost()}
                  onChange={(e) => pickComputer(e.currentTarget.value)}
                  aria-label="Where to test"
                  title="A connector is a program on the computer the chat runs on: test it and set it up where the chats run"
                >
                  <option value="">This computer</option>
                  <For each={hosts()}>{(h) => <option value={h.id}>{h.name}</option>}</For>
                </select>
              </SettingRow>
            </Show>

            <div class="connector-checks">
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
              </div>
              <Show when={okState()}>
                {(ok) => (
                  <div class="provider-test-status ok connector-test-status">
                    {probeSummary(ok().probe)}, {ok().probe.models.length} models
                    {!ok().first && ok().added > 0 ? ` (${ok().added} new, not enabled)` : ''}
                    <Show when={authLine(authStateOf({ probe: ok().probe }))}>
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
                  <div
                    class="provider-test-status error connector-test-status"
                    title={err().message}
                  >
                    {err().message}
                  </div>
                )}
              </Show>
              <ConnectorSetup
                connector={d()}
                title={connectorTitle(kindOfProvider(d()))}
                host={testHost() || null}
                where={testWhere()}
                probe={() => testNow(false)}
                signedIn={okState()?.probe.auth?.loggedIn === true}
                update={okState()?.probe.update}
                autoStart={setUpOnOpen()}
              />
            </div>
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
            emptyHint={specOf(d())?.modelsHint}
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
