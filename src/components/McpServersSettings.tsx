// McpServersSettings.tsx
//
// Settings → MCP servers (docs/design/mcp-plan.md, Q01, Q02, Q04): the
// servers whose tools the agent calls through its one `mcp` tool, each a
// command Rovik starts on this computer or an address it connects to. The
// list turns each on or off. A server's form says how it's reached, with
// its env vars or headers (a secret's value goes in the keychain, never in
// the settings: lib/mcpServers.ts's `savingServer`), tests it, and picks
// which of its tools the agent gets. Servers can also be added from the
// JSON another client's setup instructions give.
import { createResource, createSignal, For, Index, onCleanup, Show } from 'solid-js';
import type { SettingsPage } from '../lib/appSettings';
import {
  emptyTransport,
  forgettingServer,
  hasSavedSecret,
  importServers,
  mcpStore,
  newServer,
  saveMcpSecrets,
  savingServer,
  secretKey,
  serverProblem,
  serverSummary,
  testMcpServer,
  transportProblem,
  unsavedSecrets,
  valuesOf,
  withValues,
  type McpServer,
  type McpTested,
  type McpTransport,
  type McpValue,
} from '../lib/mcpServers';
import { onCloudEvent } from '../lib/cloud';
import { listHosts } from '../lib/hosts';
import { confirmDialog } from './ConfirmDialog';
import { SecretsUnlock, Segmented, SettingRow, SettingStatus } from './SettingsControls';

/** Lets the settings modal show the open form's name and close it with Back. */
export interface McpServersSettingsController {
  /** The server being edited, or null on the list. */
  draftTitle: () => string | null;
  closeDraft: () => void;
}

interface McpServersSettingsProps {
  /** The saved servers. */
  servers: McpServer[];
  /** Every change to the list (a saved form, a switch, a removal), saved at once. */
  onChange: (servers: McpServer[]) => void;
  /** The open project, where Test starts a command; none without a project. */
  rootPath: string | null;
  onOpenPage: (page: SettingsPage) => void;
  controller?: (api: McpServersSettingsController) => void;
}

type TestState =
  | { kind: 'idle' }
  | { kind: 'testing' }
  | { kind: 'ok'; tested: McpTested }
  | { kind: 'error'; message: string };

/** A copy to edit: its lists are its own. */
function copyOf(server: McpServer): McpServer {
  const t = server.transport;
  const transport = withValues(
    t.kind === 'command' ? { ...t, args: [...t.args] } : { ...t },
    valuesOf(t).map((v) => ({ ...v }))
  );
  return { ...server, transport, disabledTools: [...server.disabledTools] };
}

const hasSecrets = (server: McpServer) => valuesOf(server.transport).some((v) => v.secret);

export default function McpServersSettings(props: McpServersSettingsProps) {
  const [draft, setDraft] = createSignal<McpServer | null>(null);
  // The other transport's fields, kept while the form switches between them.
  const [otherTransport, setOtherTransport] = createSignal<McpTransport | null>(null);
  const [test, setTest] = createSignal<TestState>({ kind: 'idle' });
  const [formError, setFormError] = createSignal<string | null>(null);
  const [listError, setListError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [importing, setImporting] = createSignal(false);
  const [json, setJson] = createSignal('');
  // Where secret values go: a locked file is unlocked here before they're
  // saved, or read as a server starts.
  const [store, { refetch: refetchStore }] = createResource(() => mcpStore().catch(() => null));
  // The hosts whose daemon runs MCP servers: places to Test on. A host list
  // that won't load leaves only this computer.
  const [hosts, { refetch: refetchHosts }] = createResource(() => listHosts().catch(() => null));
  // A host connecting or dropping changes who can be chosen.
  onCleanup(
    onCloudEvent((event) => {
      if (event.type === 'host') void refetchHosts();
    })
  );
  const testHosts = () =>
    (hosts()?.hosts ?? []).filter((h) => h.state === 'ready' && h.serves.includes('mcp'));
  const [chosenHost, setChosenHost] = createSignal('');
  // The place chosen, while it's still offered: otherwise this computer ('').
  const testHost = () => (testHosts().some((h) => h.id === chosenHost()) ? chosenHost() : '');
  const locked = () => {
    const s = store();
    return s?.kind === 'file' && !s.unlocked;
  };
  const needsUnlock = () => {
    const d = draft();
    return locked() && (props.servers.some(hasSecrets) || (d ? hasSecrets(d) : importing()));
  };

  // A test's result is ignored once the form changed or closed.
  let testRun = 0;
  function forgetTest() {
    testRun++;
    setTest({ kind: 'idle' });
  }
  const tested = () => {
    const t = test();
    return t.kind === 'ok' ? t.tested : null;
  };
  const testError = () => {
    const t = test();
    return t.kind === 'error' ? t.message : null;
  };

  /** The server as saved, while its form is open (none for a new one). */
  const saved = () => {
    const d = draft();
    return d ? props.servers.find((s) => s.id === d.id) : undefined;
  };

  const patch = (changes: Partial<McpServer>) => {
    const d = draft();
    if (d) setDraft({ ...d, ...changes });
  };
  /** A change to how it's reached: the last test no longer says. */
  const setTransport = (transport: McpTransport) => {
    patch({ transport });
    forgetTest();
  };

  function openDraft(server: McpServer) {
    forgetTest();
    setDraft(copyOf(server));
    setOtherTransport(null);
    setFormError(null);
    setImporting(false);
  }

  function closeDraft() {
    forgetTest();
    setDraft(null);
    setFormError(null);
  }

  function switchKind(kind: McpTransport['kind']) {
    const d = draft();
    if (!d || d.transport.kind === kind) return;
    const other = otherTransport();
    setOtherTransport(d.transport);
    setTransport(other?.kind === kind ? other : emptyTransport(kind));
  }

  const values = () => {
    const d = draft();
    return d ? valuesOf(d.transport) : [];
  };
  const setValues = (next: McpValue[]) => {
    const d = draft();
    if (d) setTransport(withValues(d.transport, next));
  };
  const changeValue = (i: number, changes: Partial<McpValue>) =>
    setValues(values().map((v, j) => (j === i ? { ...v, ...changes } : v)));
  /** A new value is a secret until it's said not to be: most are tokens. */
  const addValue = () => setValues([...values(), { name: '', value: '', secret: true }]);
  const removeValue = (i: number) => setValues(values().filter((_, j) => j !== i));

  const valueHint = (v: McpValue) => {
    if (!v.secret) return 'Value';
    const d = draft();
    const key = d ? secretKey(d.transport, v.name.trim()) : '';
    return hasSavedSecret(saved(), key) ? 'Saved: leave empty to keep it' : 'Not saved yet';
  };

  function toggleTool(name: string, on: boolean) {
    const d = draft();
    if (!d) return;
    const others = d.disabledTools.filter((t) => t !== name);
    patch({ disabledTools: on ? others : [...others, name] });
  }

  /** Starts the server as the form has it, with the secrets typed, and lists its tools. */
  async function runTest() {
    const d = draft();
    if (!d) return;
    const problem = transportProblem(d.transport);
    if (problem) {
      setTest({ kind: 'error', message: problem });
      return;
    }
    const run = ++testRun;
    setTest({ kind: 'testing' });
    const { server, secrets } = savingServer(d, saved());
    // The values typed: an empty one is a forget, not a value.
    const typed = Object.fromEntries(Object.entries(secrets).filter(([, value]) => value));
    try {
      const result = await testMcpServer(server, typed, props.rootPath, testHost() || null);
      if (run === testRun) setTest({ kind: 'ok', tested: result });
    } catch (e) {
      if (run === testRun) setTest({ kind: 'error', message: String(e) });
    }
  }

  /** The secrets go in first: a keychain that refuses them leaves the settings as they were. */
  async function save() {
    const d = draft();
    if (!d || busy()) return;
    const problem = serverProblem(d, props.servers);
    if (problem) {
      setFormError(problem);
      return;
    }
    const { server, secrets } = savingServer(d, saved());
    setBusy(true);
    setFormError(null);
    try {
      if (Object.keys(secrets).length > 0) await saveMcpSecrets(server.id, secrets);
    } catch (e) {
      setFormError(String(e));
      return;
    } finally {
      setBusy(false);
    }
    props.onChange(
      saved()
        ? props.servers.map((s) => (s.id === server.id ? server : s))
        : [...props.servers, server]
    );
    closeDraft();
  }

  async function remove(server: McpServer) {
    const confirmed = await confirmDialog({
      title: 'Remove this MCP server?',
      message: (
        <>
          <strong>{server.name}</strong> will be removed from Rovik
          {hasSecrets(server) ? ', and the secret values saved for it forgotten' : ''}. A program it
          runs stays installed.
        </>
      ),
      confirmLabel: 'Remove',
      danger: true,
    });
    if (!confirmed) return;
    setListError(null);
    const forget = forgettingServer(server);
    try {
      if (Object.keys(forget).length > 0) await saveMcpSecrets(server.id, forget);
    } catch (e) {
      setListError(String(e));
      return;
    }
    props.onChange(props.servers.filter((s) => s.id !== server.id));
  }

  const setEnabled = (server: McpServer, enabled: boolean) =>
    props.onChange(props.servers.map((s) => (s.id === server.id ? { ...s, enabled } : s)));

  /**
   * Adds the servers the JSON pasted has, each saved as a form's would be,
   * its secrets first; one alone opens in its form, to finish and test.
   */
  async function addFromJson() {
    if (busy()) return;
    const result = importServers(json(), props.servers);
    if ('error' in result) {
      setListError(result.error);
      return;
    }
    setBusy(true);
    setListError(null);
    const added: McpServer[] = [];
    try {
      for (const imported of result.servers) {
        const { server, secrets } = savingServer(imported, undefined);
        if (Object.keys(secrets).length > 0) await saveMcpSecrets(server.id, secrets);
        added.push(server);
      }
    } catch (e) {
      setListError(String(e));
    } finally {
      setBusy(false);
    }
    if (added.length === 0) return;
    props.onChange([...props.servers, ...added]);
    if (added.length < result.servers.length) return;
    setImporting(false);
    setJson('');
    if (added.length === 1) openDraft(added[0]);
  }

  async function unlocked() {
    await refetchStore();
    setFormError(null);
    setListError(null);
  }

  props.controller?.({
    draftTitle: () => {
      const d = draft();
      return d ? d.name.trim() || 'New MCP server' : null;
    },
    closeDraft,
  });

  const commandOf = (s: McpServer) => (s.transport.kind === 'command' ? s.transport : null);
  const urlOf = (s: McpServer) => (s.transport.kind === 'url' ? s.transport : null);

  const ToolRow = (tool: { name: string; readOnly?: boolean; about?: string }) => (
    <label class="checkbox-row mcp-tool">
      <input
        type="checkbox"
        checked={!draft()?.disabledTools.includes(tool.name)}
        onChange={(e) => toggleTool(tool.name, e.currentTarget.checked)}
      />
      <span class="mcp-tool-name">{tool.name}</span>
      <Show when={tool.readOnly}>
        <span
          class="mcp-tool-mark"
          title="The server says it only reads: Plan mode and scouts can call it"
        >
          read-only
        </span>
      </Show>
      <Show when={tool.about}>
        <span class="mcp-tool-about" title={tool.about}>
          {tool.about}
        </span>
      </Show>
    </label>
  );

  return (
    <>
      <Show when={needsUnlock()}>
        <p class="settings-hint">
          This computer has no keychain, so secret values go in a file encrypted under a passphrase,
          with the API keys. It’s locked: enter its passphrase to save the servers’ secrets and for
          the servers to start with them. The first time, the passphrase you choose here makes the
          file.
        </p>
        <SecretsUnlock onUnlocked={unlocked} />
      </Show>
      <Show
        when={draft()}
        fallback={
          <>
            <p class="settings-hint">
              Servers whose tools the agent can call: GitHub, a database, your docs. Each runs on
              this computer, a command Rovik starts or an address it connects to. The agent finds
              their tools listed in its prompt and calls them with one tool, mcp, each call approved
              as{' '}
              <button class="link-button" onClick={() => props.onOpenPage('permissions')}>
                Permissions & approval
              </button>{' '}
              says for MCP tool calls.
            </p>
            <p class="settings-hint">
              A server works outside Rovik’s file protections and the allowed hosts, and what its
              tools say reaches the model: add only servers you trust. Chats on cloud hosts and in
              WSL don’t use them.
            </p>
            <For
              each={props.servers}
              fallback={
                <div class="empty-state">
                  No MCP servers yet. Add one with its command or URL, or paste the JSON its setup
                  instructions give.
                </div>
              }
            >
              {(s) => (
                <div class="profile-row mcp-server-row">
                  <div class="profile-row-info">
                    <span class="profile-row-label">{s.name}</span>
                    <span class="profile-row-sub" title={serverSummary(s)}>
                      {serverSummary(s)}
                    </span>
                    <Show when={unsavedSecrets(s).length > 0}>
                      <span class="profile-row-sub warning">
                        No value saved for {unsavedSecrets(s).join(', ')}: Edit to enter it
                      </span>
                    </Show>
                    <Show when={s.disabledTools.length > 0}>
                      <span class="profile-row-sub">
                        {s.disabledTools.length === 1
                          ? '1 tool turned off'
                          : `${s.disabledTools.length} tools turned off`}
                      </span>
                    </Show>
                  </div>
                  <div class="profile-row-actions">
                    <label
                      class="checkbox-row"
                      title="Off: its settings stay, and chats don’t use it"
                    >
                      <input
                        type="checkbox"
                        aria-label={`Use ${s.name}`}
                        checked={s.enabled}
                        onChange={(e) => setEnabled(s, e.currentTarget.checked)}
                      />
                      On
                    </label>
                    <button onClick={() => openDraft(s)}>Edit</button>
                    <button class="danger" onClick={() => void remove(s)}>
                      Remove
                    </button>
                  </div>
                </div>
              )}
            </For>
            <Show when={listError()}>
              <SettingStatus warning>{listError()}</SettingStatus>
            </Show>
            <Show
              when={importing()}
              fallback={
                <div class="mcp-add">
                  <button class="add-profile-button" onClick={() => openDraft(newServer())}>
                    + Add a server
                  </button>
                  <button
                    class="add-profile-button"
                    onClick={() => {
                      setImporting(true);
                      setListError(null);
                    }}
                  >
                    + Add from JSON
                  </button>
                </div>
              }
            >
              <div class="mcp-import">
                <p class="settings-hint">
                  Paste a server’s setup as its instructions give it for Claude Desktop, Claude
                  Code, Cursor or VS Code: an "mcpServers" object, or one server’s entry. Values
                  named like a token, a key or a password are kept as secrets.
                </p>
                <textarea
                  class="settings-textarea"
                  aria-label="Servers as JSON"
                  rows={8}
                  spellcheck={false}
                  value={json()}
                  placeholder={
                    '{\n  "mcpServers": {\n    "github": { "command": "npx", … }\n  }\n}'
                  }
                  onInput={(e) => setJson(e.currentTarget.value)}
                />
                <div class="profile-form-actions">
                  <button
                    onClick={() => {
                      setImporting(false);
                      setJson('');
                      setListError(null);
                    }}
                  >
                    Cancel
                  </button>
                  <button
                    class="primary"
                    disabled={!json().trim() || busy()}
                    onClick={() => void addFromJson()}
                  >
                    Add
                  </button>
                </div>
              </div>
            </Show>
          </>
        }
      >
        {(d) => (
          <div class="profile-form mcp-form">
            <section class="settings-section">
              <SettingRow label="Name">
                <input
                  aria-label="Name"
                  value={d().name}
                  placeholder="github"
                  spellcheck={false}
                  autocomplete="off"
                  onInput={(e) => patch({ name: e.currentTarget.value })}
                />
              </SettingRow>
              <p class="settings-hint setting-explain">
                The agent calls its tools by it: {d().name.trim() || 'github'}.create_issue.
              </p>
            </section>

            <section class="settings-section">
              <div class="settings-section-title">Connection</div>
              <SettingRow label="Type">
                <Segmented
                  label="How Rovik reaches the server"
                  value={d().transport.kind}
                  options={[
                    {
                      value: 'command',
                      label: 'Command',
                      title: 'A program Rovik starts on this computer',
                    },
                    { value: 'url', label: 'URL', title: 'An address Rovik connects to' },
                  ]}
                  onChange={switchKind}
                />
              </SettingRow>
              <Show when={commandOf(d())}>
                {(t) => (
                  <>
                    <SettingRow label="Command">
                      <input
                        aria-label="Command"
                        value={t().command}
                        placeholder="npx"
                        spellcheck={false}
                        autocomplete="off"
                        onInput={(e) => setTransport({ ...t(), command: e.currentTarget.value })}
                      />
                    </SettingRow>
                    <SettingRow label="Arguments">
                      <textarea
                        class="settings-textarea"
                        aria-label="Arguments"
                        rows={3}
                        spellcheck={false}
                        value={t().args.join('\n')}
                        placeholder={'-y\n@modelcontextprotocol/server-github'}
                        onInput={(e) =>
                          setTransport({ ...t(), args: e.currentTarget.value.split(/\r?\n/) })
                        }
                      />
                    </SettingRow>
                    <p class="settings-hint setting-explain">
                      One argument per line. It starts in{' '}
                      {props.rootPath ? 'the chat’s project folder' : 'your home folder'}, as Claude
                      Code starts its servers.
                    </p>
                  </>
                )}
              </Show>
              <Show when={urlOf(d())}>
                {(t) => (
                  <SettingRow label="URL">
                    <input
                      aria-label="URL"
                      value={t().url}
                      placeholder="https://mcp.example.com/mcp"
                      spellcheck={false}
                      autocomplete="off"
                      onInput={(e) => setTransport({ ...t(), url: e.currentTarget.value })}
                    />
                  </SettingRow>
                )}
              </Show>

              <SettingRow label={d().transport.kind === 'command' ? 'Environment' : 'Headers'}>
                <div class="mcp-values">
                  <Index each={values()}>
                    {(v, i) => (
                      <div class="mcp-value-row">
                        <input
                          aria-label={d().transport.kind === 'command' ? 'Variable' : 'Header'}
                          value={v().name}
                          placeholder={
                            d().transport.kind === 'command' ? 'GITHUB_TOKEN' : 'Authorization'
                          }
                          spellcheck={false}
                          autocomplete="off"
                          onInput={(e) => changeValue(i, { name: e.currentTarget.value })}
                        />
                        <input
                          type={v().secret ? 'password' : 'text'}
                          aria-label={`Value of ${v().name.trim() || 'it'}`}
                          value={v().value}
                          placeholder={valueHint(v())}
                          spellcheck={false}
                          autocomplete="off"
                          onInput={(e) => changeValue(i, { value: e.currentTarget.value })}
                        />
                        <label
                          class="checkbox-row"
                          title="Kept in the keychain, never in the settings file"
                        >
                          <input
                            type="checkbox"
                            checked={v().secret}
                            onChange={(e) => changeValue(i, { secret: e.currentTarget.checked })}
                          />
                          Secret
                        </label>
                        <button
                          class="icon-button"
                          aria-label={`Remove ${v().name.trim() || 'this value'}`}
                          title="Remove"
                          onClick={() => removeValue(i)}
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </Index>
                  <button class="link-button mcp-add-value" onClick={addValue}>
                    {d().transport.kind === 'command' ? '+ Add a variable' : '+ Add a header'}
                  </button>
                </div>
              </SettingRow>

              <div class="mcp-test">
                <div class="provider-test-row">
                  <button onClick={() => void runTest()} disabled={test().kind === 'testing'}>
                    {test().kind === 'testing' ? 'Testing…' : 'Test'}
                  </button>
                  <Show when={testHosts().length > 0}>
                    <select
                      aria-label="Test on"
                      value={testHost()}
                      onChange={(e) => {
                        setChosenHost(e.currentTarget.value);
                        forgetTest();
                      }}
                    >
                      <option value="">This computer</option>
                      <For each={testHosts()}>{(h) => <option value={h.id}>{h.name}</option>}</For>
                    </select>
                  </Show>
                </div>
                <Show when={tested()}>
                  {(t) => (
                    <div class="provider-test-status ok">
                      Connected to {t().server}:{' '}
                      {t().tools.length === 1 ? '1 tool' : `${t().tools.length} tools`}, MCP{' '}
                      {t().protocol}
                    </div>
                  )}
                </Show>
                <Show when={testError()}>
                  {(message) => (
                    <div class="provider-test-status error mcp-test-error">{message()}</div>
                  )}
                </Show>
              </div>
            </section>

            <section class="settings-section">
              <div class="settings-section-title">Tools</div>
              <Show
                when={tested()}
                fallback={
                  <>
                    <p class="settings-hint">
                      Test the server to list its tools and pick the ones the agent gets. Plan mode
                      and scouts get only the tools it marks read-only.
                    </p>
                    <Show when={d().disabledTools.length > 0}>
                      <p class="settings-hint">Turned off: tick one to turn it on again.</p>
                      <For each={d().disabledTools}>{(name) => <ToolRow name={name} />}</For>
                    </Show>
                  </>
                }
              >
                {(t) => (
                  <For
                    each={t().tools}
                    fallback={<p class="settings-hint">The server has no tools.</p>}
                  >
                    {(tool) => (
                      <ToolRow
                        name={tool.name}
                        readOnly={tool.readOnly}
                        about={tool.description.split('\n')[0]}
                      />
                    )}
                  </For>
                )}
              </Show>
            </section>

            <Show when={formError()}>
              <SettingStatus warning>{formError()}</SettingStatus>
            </Show>
            <div class="profile-form-actions">
              <button onClick={closeDraft}>Cancel</button>
              <button class="primary" disabled={busy()} onClick={() => void save()}>
                Save
              </button>
            </div>
          </div>
        )}
      </Show>
    </>
  );
}
