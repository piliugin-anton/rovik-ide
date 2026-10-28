// CloudHostsSettings.tsx
//
// Settings → Cloud hosts (U01): the servers chats can move to, reached
// over SSH. The list; each host's form (design §7, without the agent port,
// which a socket replaced); and a host's own page:
// - Test connection, a step at a time, with the server's key to compare
//   and trust the first time;
// - Rovik deployed there, upgraded or removed;
// - who approves while nobody is there (6.9);
// - while what the server says isn't in yet, a spinner in place of the
//   sections it fills;
// - what Rovik's folders take on the server, and "Clean up now";
// - the providers whose key the server keeps, never the keys, with
//   "Remove keys from this server";
// - Rovik on the server, its version and administrators, the connectors
//   installed there, and for an administrator, the limits and "Upgrade for
//   everyone".
import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  Match,
  on,
  onCleanup,
  onMount,
  Show,
  Switch,
} from 'solid-js';
import { confirmDialog } from './ConfirmDialog';
import { SecretsUnlock } from './SettingsControls';
import HostAutonomy from './HostAutonomy';
import HostTasks from './HostTasks';
import { onCloudEvent, refreshHosts } from '../lib/cloud';
import { connectorTitle } from '../lib/connectors';
import { formatBytes, plural } from '../lib/textUtils';
import type { ConnectorKind } from '../types';
import {
  addressOf,
  asDeployFailure,
  AUTH_HINTS,
  AUTH_LABELS,
  CHECK_LABELS,
  cleanUpHost,
  deployHost,
  emptyHost,
  hostDetails,
  hostLimits,
  hostOf,
  hostStatus,
  hostTasks,
  listHosts,
  onHostTest,
  removalOf,
  removeHost,
  removeKeys,
  removeRovik,
  runSetup,
  saveHost,
  secretsToSave,
  setHostLimits,
  setPolicy,
  setupVerdict,
  testHost,
  testVerdict,
  trustBuild,
  trustKey,
  upgradeHost,
  type ApprovalPolicy,
  type AuthKind,
  type DeployFailure,
  type Host,
  type HostDetails,
  type HostRow,
  type HostsList,
  type SetupRun,
  type Step,
} from '../lib/hosts';

/** Lets the Settings dialog title the page and step back within it. */
export interface HostsSettingsController {
  /** The host or form open, or null on the list. */
  title: () => string | null;
  /** Back from a host's form to its page, or from its page to the list. */
  back: () => void;
}

interface CloudHostsSettingsProps {
  controller?: (api: HostsSettingsController) => void;
}

type View =
  { kind: 'list' } | { kind: 'form'; host: Host; isNew: boolean } | { kind: 'host'; id: string };

const OUTCOME_MARK: Record<Step['outcome'], string> = {
  passed: '✓',
  warning: '!',
  failed: '✗',
  confirm: '?',
  skipped: '–',
};

export default function CloudHostsSettings(props: CloudHostsSettingsProps) {
  const [list, setList] = createSignal<HostsList | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [view, setView] = createSignal<View>({ kind: 'list' });

  async function refresh() {
    try {
      setList(await listHosts());
      setError(null);
    } catch (err) {
      setError(String(err));
    }
    // The chats' menus offer the hosts as they are now.
    void refreshHosts();
  }
  onMount(refresh);
  // A host's connection changing shows at once, with why it was refused.
  const stopEvents = onCloudEvent((event) => {
    if (event.type !== 'host') return;
    setList((current) =>
      current
        ? {
            ...current,
            hosts: current.hosts.map((h) =>
              h.id === event.host ? { ...h, state: event.state, problem: event.problem } : h
            ),
          }
        : current
    );
  });
  onCleanup(stopEvents);

  const row = (id: string) => list()?.hosts.find((h) => h.id === id);

  props.controller?.({
    title: () => {
      const v = view();
      if (v.kind === 'form') return v.isNew ? 'New host' : v.host.name || 'Host';
      if (v.kind === 'host') return row(v.id)?.name ?? null;
      return null;
    },
    back: () => {
      const v = view();
      setView(v.kind === 'form' && !v.isNew ? { kind: 'host', id: v.host.id } : { kind: 'list' });
    },
  });

  return (
    <div class="permissions-settings cloud-hosts-settings">
      <Show when={error()}>
        <p class="settings-hint setting-explain warning">{error()}</p>
      </Show>
      <Switch>
        <Match when={view().kind === 'list'}>
          <HostList
            list={list()}
            onOpen={(id) => setView({ kind: 'host', id })}
            onAdd={() => setView({ kind: 'form', host: emptyHost(), isNew: true })}
            onUnlocked={refresh}
          />
        </Match>
        <Match when={view().kind === 'form' && (view() as Extract<View, { kind: 'form' }>)}>
          {(form) => (
            <HostForm
              host={form().host}
              isNew={form().isNew}
              row={row(form().host.id)}
              onCancel={() =>
                setView(form().isNew ? { kind: 'list' } : { kind: 'host', id: form().host.id })
              }
              onSaved={async (saved) => {
                await refresh();
                setView({ kind: 'host', id: saved.id });
              }}
              onRemoved={async () => {
                await refresh();
                setView({ kind: 'list' });
              }}
            />
          )}
        </Match>
        <Match when={view().kind === 'host' && row((view() as Extract<View, { kind: 'host' }>).id)}>
          {(host) => (
            <HostPage
              row={host()}
              build={list()?.build}
              onEdit={() => setView({ kind: 'form', host: hostOf(host()), isNew: false })}
              onChanged={refresh}
            />
          )}
        </Match>
      </Switch>
    </div>
  );
}

function HostList(props: {
  list: HostsList | null;
  onOpen: (id: string) => void;
  onAdd: () => void;
  onUnlocked: () => Promise<void>;
}) {
  const store = () => props.list?.store;
  const locked = () => {
    const s = store();
    return s?.kind === 'file' && !s.unlocked;
  };

  return (
    <>
      <section class="settings-section">
        <div class="settings-section-title">Cloud hosts</div>
        <p class="settings-hint">
          Servers your chats can move to, reached over SSH. A chat there keeps running while this
          computer sleeps, and comes back when you move it back.
        </p>
        <Show when={store()}>
          {(s) => (
            <p class="settings-hint">
              {s().kind === 'keychain'
                ? 'Passwords and key passphrases are kept in your system’s keychain.'
                : 'This computer has no keychain, so passwords, key passphrases and API keys go in a file encrypted under a passphrase.'}
            </p>
          )}
        </Show>
        <Show when={locked()}>
          <p class="settings-hint">
            The file is locked: enter its passphrase to save passwords and reach your hosts. The
            first time, the passphrase you choose here makes the file. Unlock it here again each
            time Rovik starts.
          </p>
          <SecretsUnlock onUnlocked={props.onUnlocked} />
        </Show>
      </section>
      <Show when={props.list} fallback={<p class="settings-hint">Loading…</p>}>
        {(list) => (
          <>
            <For
              each={list().hosts}
              fallback={
                <div class="empty-state">No hosts yet. Add a server you reach over SSH.</div>
              }
            >
              {(host) => {
                const status = () => hostStatus(host);
                return (
                  <div class="profile-row host-row">
                    <div class="profile-row-info">
                      <span class="profile-row-label">{host.name}</span>
                      <span class="profile-row-sub">{addressOf(host)}</span>
                      <span class="profile-row-sub">
                        <span class={`host-status ${status().tone}`}>{status().label}</span>
                        {host.chats > 0 ? ` · ${plural(host.chats, 'chat')}` : ''}
                      </span>
                    </div>
                    <div class="profile-row-actions">
                      <button onClick={() => props.onOpen(host.id)}>Open</button>
                    </div>
                  </div>
                );
              }}
            </For>
            <button class="add-profile-button" onClick={props.onAdd}>
              + Add host
            </button>
          </>
        )}
      </Show>
    </>
  );
}

function HostForm(props: {
  host: Host;
  isNew: boolean;
  row?: HostRow;
  onCancel: () => void;
  onSaved: (host: Host) => void;
  onRemoved: () => void;
}) {
  const [draft, setDraft] = createSignal<Host>({ ...props.host });
  const [password, setPassword] = createSignal('');
  const [passphrase, setPassphrase] = createSignal('');
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const patch = (changes: Partial<Host>) => setDraft((d) => ({ ...d, ...changes }));
  const auth = () => draft().auth;

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const typed = { password: password(), passphrase: passphrase() };
      props.onSaved(await saveHost(draft(), secretsToSave(auth(), typed, props.row)));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    const ok = await confirmDialog({
      title: `Remove ${props.host.name}?`,
      message:
        'This computer forgets the host and the password, passphrase and token it kept for it. Rovik stays on the server.',
      confirmLabel: 'Remove',
      danger: true,
    });
    if (!ok) return;
    try {
      await removeHost(props.host.id);
      props.onRemoved();
    } catch (err) {
      setError(String(err));
    }
  }

  const savedHint = (saved: boolean | undefined) => (saved ? 'Saved: leave empty to keep it' : '');

  return (
    <div class="profile-form host-form">
      <label>
        Name
        <input
          value={draft().name}
          onInput={(e) => patch({ name: e.currentTarget.value })}
          placeholder="e.g. Build server"
        />
      </label>
      <label>
        Host
        <input
          value={draft().host}
          onInput={(e) => patch({ host: e.currentTarget.value.trim() })}
          placeholder="build.example.com or 203.0.113.7"
          spellcheck={false}
        />
      </label>
      <label>
        SSH port
        <input
          type="number"
          min="1"
          max="65535"
          value={draft().port}
          onInput={(e) => patch({ port: Number(e.currentTarget.value) || 0 })}
        />
      </label>
      <label>
        User
        <input
          value={draft().user}
          onInput={(e) => patch({ user: e.currentTarget.value.trim() })}
          placeholder="your account on the server, not root"
          spellcheck={false}
        />
      </label>
      <label>
        Log in with
        <select
          value={auth()}
          onChange={(e) => patch({ auth: e.currentTarget.value as AuthKind })}
          aria-describedby="host-auth-hint"
        >
          <For each={Object.entries(AUTH_LABELS)}>
            {([kind, label]) => <option value={kind}>{label}</option>}
          </For>
        </select>
      </label>
      <p class="settings-hint" id="host-auth-hint">
        {AUTH_HINTS[auth()]}
      </p>
      <Show when={auth() === 'key'}>
        <label>
          Key file
          <input
            value={draft().keyPath ?? ''}
            onInput={(e) => patch({ keyPath: e.currentTarget.value || undefined })}
            placeholder="~/.ssh/id_ed25519"
            spellcheck={false}
          />
        </label>
        <label>
          Key passphrase
          <input
            type="password"
            value={passphrase()}
            onInput={(e) => setPassphrase(e.currentTarget.value)}
            placeholder={savedHint(props.row?.passphraseSaved) || 'Only if the key has one'}
          />
        </label>
      </Show>
      <Show when={auth() === 'password' || auth() === 'keyboard-interactive'}>
        <label>
          Password
          <input
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            placeholder={savedHint(props.row?.passwordSaved)}
          />
        </label>
      </Show>
      <label>
        Setup script
        <textarea
          rows="4"
          value={draft().setupScript ?? ''}
          onInput={(e) => patch({ setupScript: e.currentTarget.value || undefined })}
          placeholder="e.g. sudo apt-get install -y ffmpeg&#10;(Rust and Node install on their own)"
          spellcheck={false}
        />
      </label>
      <p class="settings-hint">
        Runs on the server as you, in your home folder, after Rovik is deployed and whenever you
        ask: install what your projects’ checks or the agent need there (ffmpeg, a database…). Rust
        and Node versions a project names install on their own, once for the whole server.
      </p>
      <Show when={error()}>
        <div class="provider-test-status error">{error()}</div>
      </Show>
      <div class="profile-form-actions">
        <Show when={!props.isNew}>
          <button class="danger" onClick={() => void remove()}>
            Remove host
          </button>
        </Show>
        <button onClick={props.onCancel}>Cancel</button>
        <button class="primary" disabled={busy()} onClick={() => void save()}>
          Save
        </button>
      </div>
    </div>
  );
}

function HostPage(props: {
  row: HostRow;
  build?: string;
  onEdit: () => void;
  onChanged: () => Promise<void>;
}) {
  const id = props.row.id;
  const deployed = () => props.row.deploy.state === 'deployed';
  const status = () => hostStatus(props.row);

  // The connection test.
  const [steps, setSteps] = createSignal<Step[]>([]);
  const [testing, setTesting] = createSignal(false);
  const [testError, setTestError] = createSignal<string | null>(null);
  let stopListening: (() => void) | null = null;
  onCleanup(() => stopListening?.());

  async function runTest() {
    setSteps([]);
    setTestError(null);
    setTesting(true);
    try {
      stopListening = await onHostTest((hostId, step) => {
        if (hostId === id) setSteps((s) => [...s, step]);
      });
      setSteps(await testHost(id));
    } catch (err) {
      setTestError(String(err));
    } finally {
      stopListening?.();
      stopListening = null;
      setTesting(false);
    }
  }

  /** Trusts what the test showed (`confirm`), then tests again. */
  async function trust(confirm: () => Promise<void>) {
    try {
      await confirm();
      await props.onChanged();
      await runTest();
    } catch (err) {
      setTestError(String(err));
    }
  }

  // Deploy, upgrade, removal.
  const [busy, setBusy] = createSignal<string | null>(null);
  const [failure, setFailure] = createSignal<DeployFailure | null>(null);
  const [sudoPassword, setSudoPassword] = createSignal('');

  /** Runs `action`, then looks at the host again: whether it worked. */
  async function act(label: string, action: () => Promise<unknown>): Promise<boolean> {
    setBusy(label);
    setFailure(null);
    try {
      await action();
      await props.onChanged();
      return true;
    } catch (err) {
      setFailure(asDeployFailure(err));
      return false;
    } finally {
      setBusy(null);
    }
  }

  async function deploy() {
    const ok = await act('Deploying…', async () => {
      await deployHost(id, sudoPassword() || undefined);
      setSudoPassword('');
    });
    if (ok && hasSetup()) await setup();
  }

  // The setup script, after a deploy and when asked.
  const hasSetup = () => !!props.row.setupScript?.trim();
  const [setupRun, setSetupRun] = createSignal<SetupRun | null>(null);
  const [settingUp, setSettingUp] = createSignal(false);
  const [setupError, setSetupError] = createSignal<string | null>(null);
  async function setup() {
    setSettingUp(true);
    setSetupRun(null);
    setSetupError(null);
    try {
      setSetupRun(await runSetup(id));
    } catch (err) {
      setSetupError(asDeployFailure(err).message);
    } finally {
      setSettingUp(false);
    }
  }

  const upgrade = () => act('Upgrading…', () => upgradeHost(id));

  async function removeFromServer() {
    const chats = props.row.chats;
    if (chats > 0) {
      const them = chats === 1 ? 'it' : 'them';
      setFailure({
        kind: 'failed',
        message: `${plural(chats, 'chat')} ${chats === 1 ? 'is' : 'are'} on this server: move ${them} back or delete ${them} first.`,
      });
      return;
    }
    let removal;
    try {
      removal = await removalOf(id);
    } catch (err) {
      setFailure(asDeployFailure(err));
      return;
    }
    const folders = removal.folders.map(([path, size]) => `${path} (${formatBytes(size)})`);
    const ok = await confirmDialog({
      title: `Remove Rovik from ${props.row.name}?`,
      message: `Your daemon there stops, and this deletes:\n${folders.join('\n') || 'nothing: there are no folders of Rovik’s'}\nYour chats and projects on the server go with them. Rovik stays installed for its other users.`,
      confirmLabel: 'Remove',
      danger: true,
    });
    if (ok) await act('Removing…', () => removeRovik(id, removal));
  }

  // What the daemon says, once connected.
  const connected = () => deployed() && props.row.state === 'ready';
  const [details, setDetails] = createSignal<HostDetails | null>(null);
  const [detailsError, setDetailsError] = createSignal<string | null>(null);
  // Asked and not in yet. Only the latest asking's answer counts.
  const [asking, setAsking] = createSignal(connected());
  let asked = 0;
  async function loadDetails() {
    const ask = ++asked;
    if (!connected()) {
      setDetails(null);
      setAsking(false);
      return;
    }
    setAsking(true);
    try {
      const answer = await hostDetails(id);
      if (ask !== asked) return;
      setDetails(answer);
      setDetailsError(null);
    } catch (err) {
      if (ask === asked) setDetailsError(String(err));
    } finally {
      if (ask === asked) setAsking(false);
    }
  }
  // Asked again whenever the connection comes up, and after a deploy, an
  // upgrade or a removal.
  const connection = createMemo(() => {
    const deploy = props.row.deploy;
    return `${deploy.state === 'deployed' ? deploy.version : 'none'} ${props.row.state ?? ''}`;
  });
  createEffect(on(connection, () => void loadDetails()));
  // This computer's chats there and their tasks (H08), asked with the
  // details.
  const [tasks, { refetch: refetchTasks }] = createResource(
    () => connected() && connection(),
    () => hostTasks(id)
  );
  /**
   * Connected, and what the server says not in yet: the sections it fills
   * are hidden, a spinner in their place. Once in, they stay while the
   * server is asked again.
   */
  const waiting = () => connected() && ((asking() && !details()) || tasks.state === 'pending');

  // "Clean up now": what the server no longer needs goes, as each day, and
  // what that freed, measured just before.
  const [cleaning, setCleaning] = createSignal(false);
  const [cleaned, setCleaned] = createSignal<string | null>(null);
  async function cleanUp() {
    setCleaning(true);
    setCleaned(null);
    try {
      const { storage, freed } = await cleanUpHost(id);
      setDetails((d) => (d ? { ...d, storage } : d));
      setCleaned(freed > 0 ? `${formatBytes(freed)} freed.` : 'Nothing more to clean up.');
    } catch (err) {
      setDetailsError(String(err));
    } finally {
      setCleaning(false);
    }
  }

  async function forgetKeys() {
    const ok = await confirmDialog({
      title: 'Remove keys from this server?',
      message:
        'The server forgets every API key it keeps. Runs going now finish with theirs, and the next chat you start there brings its key again.',
      confirmLabel: 'Remove keys',
      danger: true,
    });
    if (!ok) return;
    try {
      await removeKeys(id);
      await loadDetails();
    } catch (err) {
      setDetailsError(String(err));
    }
  }

  async function choosePolicy(policy: ApprovalPolicy) {
    try {
      await setPolicy(id, policy);
      await props.onChanged();
    } catch (err) {
      setDetailsError(String(err));
    }
  }

  const verdict = () => testVerdict(steps(), testing());
  const admins = () => details()?.info.install?.admins ?? [];

  const deployedNote = () => {
    const parts: string[] = [];
    const d = details();
    // The daemon's own version, once it answers: someone may have upgraded it.
    const kept = props.row.deploy.state === 'deployed' ? props.row.deploy.version : null;
    const version = d?.info.version ?? kept;
    if (version) parts.push(`Version ${version}.`);
    if (d && admins().length > 0) {
      parts.push(`Its administrators: ${admins().join(', ')}.`);
      if (d.admin) parts.push('You’re one of them.');
    }
    return parts.join(' ');
  };

  /** The connectors installed there, by name; a kind this app doesn't know, by its id. */
  const connectorsNote = (kinds: string[]) => {
    const names = kinds.map((kind) => connectorTitle(kind as ConnectorKind));
    return `Connectors installed: ${names.join(', ') || 'none'}.`;
  };

  /** An older daemon than this app: who can upgrade it. */
  const upgradeNote = (d: HostDetails) => {
    const older = `Rovik ${d.info.version} here is older than this app (${d.appVersion}).`;
    if (d.admin) return older;
    return `${older} Ask ${admins().join(' or ') || 'the server’s administrator'} to upgrade it.`;
  };

  return (
    <>
      <section class="settings-section host-summary">
        <div class="host-actions">
          <span class="settings-hint">
            {addressOf(props.row)} ·{' '}
            <span class={`host-status ${status().tone}`}>{status().label}</span>
          </span>
          <button onClick={props.onEdit}>Edit</button>
        </div>
        <Show when={deployed() && props.row.problem}>
          <p class="settings-hint setting-explain warning">{props.row.problem}</p>
        </Show>
        <Show when={deployed() && props.row.state === 'offline' && !props.row.problem}>
          <p class="settings-hint">
            The app can’t reach Rovik there right now. Test the connection to see why.
          </p>
        </Show>
      </section>

      <section class="settings-section host-test">
        <div class="settings-section-title">Connection</div>
        <div class="host-actions">
          <button
            class="chat-action-button primary"
            disabled={testing()}
            onClick={() => void runTest()}
          >
            {testing() ? 'Testing…' : 'Test connection'}
          </button>
          <Show when={verdict()}>
            {(v) => <span class={`host-status ${v().tone}`}>{v().label}</span>}
          </Show>
        </div>
        <Show when={testError()}>
          <p class="settings-hint setting-explain warning">{testError()}</p>
        </Show>
        <Show when={steps().length > 0}>
          <ol class="host-steps">
            <For each={steps()}>
              {(step) => (
                <li class={`host-step ${step.outcome}`}>
                  <span class="host-step-mark" aria-label={step.outcome}>
                    {OUTCOME_MARK[step.outcome]}
                  </span>
                  <span class="host-step-check">{CHECK_LABELS[step.check]}</span>
                  <span class="host-step-message">{step.message}</span>
                  <Show when={step.hostKey}>
                    {(key) => (
                      <div class="host-key-confirm">
                        <code>{key().fingerprint}</code>
                        <button
                          class="chat-action-button primary"
                          onClick={() => void trust(() => trustKey(id, key().key))}
                        >
                          Trust this key
                        </button>
                      </div>
                    )}
                  </Show>
                  <Show when={step.build}>
                    {(build) => (
                      <div class="host-key-confirm host-build-confirm">
                        <code>{build().sha256}</code>
                        <button
                          class="chat-action-button primary"
                          onClick={() => void trust(() => trustBuild(id, build().sha256))}
                        >
                          Trust this build
                        </button>
                      </div>
                    )}
                  </Show>
                </li>
              )}
            </For>
          </ol>
        </Show>
      </section>

      <Show
        when={!waiting()}
        fallback={
          <section class="settings-section host-waiting" role="status">
            <span class="host-spinner" aria-hidden="true" />
            <span class="settings-hint">Asking the server…</span>
          </section>
        }
      >
        <section class="settings-section host-deploy">
          <div class="settings-section-title">Rovik on this server</div>
          <Show
            when={deployed()}
            fallback={
              <>
                <p class="settings-hint">
                  Rovik isn’t deployed for you here. Deploying installs it (with sudo, the first
                  time anyone does) and starts your daemon.
                </p>
                <Show
                  when={props.build}
                  fallback={
                    <p class="settings-hint setting-explain warning">
                      There’s no build to deploy: this build of Rovik has no release key to check a
                      downloaded daemon with. Set ROVIK_AGENTD_BINARY to a rovik-agentd built for
                      the server (scripts/build-agentd.sh makes one).
                    </p>
                  }
                >
                  <button
                    class="chat-action-button primary"
                    disabled={busy() != null}
                    onClick={() => void deploy()}
                  >
                    {busy() ?? 'Deploy'}
                  </button>
                </Show>
              </>
            }
          >
            <p class="settings-hint">{deployedNote()}</p>
            <Show when={details()?.info.connectors}>
              {(kinds) => <p class="settings-hint host-connectors">{connectorsNote(kinds())}</p>}
            </Show>
            <Show when={details()?.upgrade && details()}>
              {(d) => (
                <>
                  <p
                    class="settings-hint host-upgrade"
                    classList={{ 'setting-explain': !d().admin, warning: !d().admin }}
                  >
                    {upgradeNote(d())}
                  </p>
                  <Show when={d().admin}>
                    <button
                      class="chat-action-button primary"
                      disabled={busy() != null}
                      onClick={() => void upgrade()}
                    >
                      {busy() ?? 'Upgrade for everyone'}
                    </button>
                  </Show>
                </>
              )}
            </Show>
            <div class="host-actions">
              <button
                class="danger"
                disabled={busy() != null}
                onClick={() => void removeFromServer()}
              >
                Remove Rovik from this server…
              </button>
            </div>
          </Show>
          <Show when={failure()}>{(f) => <DeployProblem failure={f()} />}</Show>
          <Show when={failure()?.kind === 'sudoPassword'}>
            <div class="host-sudo">
              <input
                type="password"
                value={sudoPassword()}
                onInput={(e) => setSudoPassword(e.currentTarget.value)}
                placeholder="Your password on the server, for sudo"
                aria-label="sudo password"
              />
              <button
                class="chat-action-button primary"
                disabled={!sudoPassword() || busy() != null}
                onClick={() => void deploy()}
              >
                Deploy
              </button>
            </div>
          </Show>
        </section>
      </Show>

      <Show when={deployed() && hasSetup()}>
        <section class="settings-section host-setup">
          <div class="settings-section-title">Setup script</div>
          <div class="host-actions">
            <button
              class="chat-action-button primary"
              disabled={settingUp()}
              onClick={() => void setup()}
            >
              {settingUp() ? 'Running…' : 'Run the setup script'}
            </button>
            <Show when={setupRun()}>
              {(run) => (
                <span class={`host-status ${setupVerdict(run()).tone}`}>
                  {setupVerdict(run()).label}
                </span>
              )}
            </Show>
          </div>
          <Show when={setupError()}>
            <p class="settings-hint setting-explain warning">{setupError()}</p>
          </Show>
          <Show when={setupRun()?.output}>
            {(output) => <pre class="host-setup-output">{output()}</pre>}
          </Show>
        </section>
      </Show>

      <section class="settings-section host-policy">
        <div class="settings-section-title">While nobody is there</div>
        <p class="settings-hint">
          What a chat on this server does when it needs an approval and this computer is away.
        </p>
        <label class="setting-checkbox">
          <input
            type="radio"
            name={`policy-${id}`}
            checked={props.row.approvalPolicy === 'waitForMe'}
            onChange={() => void choosePolicy('waitForMe')}
          />
          Wait for me
        </label>
        <label class="setting-checkbox">
          <input
            type="radio"
            name={`policy-${id}`}
            checked={props.row.approvalPolicy === 'aiReviewerThenMe'}
            onChange={() => void choosePolicy('aiReviewerThenMe')}
          />
          Ask the AI reviewer first, then me
        </label>
        <label class="setting-checkbox">
          <input
            type="radio"
            name={`policy-${id}`}
            checked={props.row.approvalPolicy === 'autonomous'}
            onChange={() => void choosePolicy('autonomous')}
          />
          <span>
            Works on its own
            <span class="setting-checkbox-sub">
              Chats on this server run in Autopilot: they plan, review, implement, check and deliver
              a task without asking. Nothing waits for you. What the agent can’t decide, it writes
              down.
            </span>
          </span>
        </label>
        <Show when={props.row.approvalPolicy === 'autonomous'}>
          <HostAutonomy host={props.row} onSaved={() => props.onChanged()} />
        </Show>
      </section>

      <Show when={deployed() && !waiting()}>
        <Show when={detailsError()}>
          <p class="settings-hint setting-explain warning">{detailsError()}</p>
        </Show>
        <Show
          when={details()}
          fallback={
            <Show when={!connected()}>
              <section class="settings-section">
                <p class="settings-hint">Connect to the server to see its storage and keys.</p>
              </section>
            </Show>
          }
        >
          {(d) => (
            <>
              <Show when={d().storage}>
                {(s) => (
                  <section class="settings-section host-storage">
                    <div class="settings-section-title">Server storage</div>
                    <ul class="host-storage-list">
                      <li>Projects: {formatBytes(s().projects)}</li>
                      <li>Chats’ working copies: {formatBytes(s().work)}</li>
                      <li>Chats and attachments: {formatBytes(s().data)}</li>
                      <li>Temporary files: {formatBytes(s().tmp)}</li>
                    </ul>
                    <p class="settings-hint">
                      What deleted chats and chats moved back left there, files no message uses, and
                      temporary files unchanged for a day go each day. “Clean up now” doesn’t wait:
                      only temporary files changed in the last hour stay, as a command may still use
                      them.
                    </p>
                    <Show when={d().cleanUp}>
                      <div class="host-actions">
                        <button disabled={cleaning()} onClick={() => void cleanUp()}>
                          {cleaning() ? 'Cleaning up…' : 'Clean up now'}
                        </button>
                        <Show when={cleaned()}>
                          {(note) => <span class="settings-hint">{note()}</span>}
                        </Show>
                      </div>
                    </Show>
                  </section>
                )}
              </Show>
              <HostTasks tasks={tasks} onRefresh={() => void refetchTasks()} />
              <section class="settings-section host-keys">
                <div class="settings-section-title">API keys on this server</div>
                <p class="settings-hint">
                  Your API keys are stored in your account on this server, so chats there run while
                  this computer is away. The agent’s commands there and anyone with root on it can
                  read them.
                </p>
                <Show
                  when={d().keys}
                  fallback={
                    <p class="settings-hint">This server’s Rovik is too old to list its keys.</p>
                  }
                >
                  {(keys) => (
                    <>
                      <ul class="host-keys-list">
                        <For
                          each={keys()}
                          fallback={<li class="settings-hint">No keys are kept there.</li>}
                        >
                          {(key) => (
                            <li>
                              <span class="host-key-provider">{key.provider}</span>{' '}
                              <span class="profile-row-sub">{key.baseUrl}</span>
                            </li>
                          )}
                        </For>
                      </ul>
                      <Show when={keys().length > 0}>
                        <button class="danger" onClick={() => void forgetKeys()}>
                          Remove keys from this server
                        </button>
                      </Show>
                    </>
                  )}
                </Show>
              </section>
              <Show when={d().admin}>
                <HostLimits id={id} />
              </Show>
            </>
          )}
        </Show>
      </Show>
    </>
  );
}

/** Why a deploy, an upgrade or a removal didn't happen, and what to do. */
function DeployProblem(props: { failure: DeployFailure }) {
  return (
    <Switch>
      <Match when={props.failure.kind === 'needsRoot' && props.failure}>
        {(f) => (
          <div class="host-needs-root">
            <p class="settings-hint setting-explain warning">
              Installing Rovik takes root, and sudo isn’t yours on this server. Ask someone with
              root to run this, then deploy again:
            </p>
            <code class="host-command">{(f() as { command: string }).command}</code>
          </div>
        )}
      </Match>
      <Match when={props.failure.kind === 'sudoPassword'}>
        <p class="settings-hint">
          sudo asks for your password on the server to install Rovik. It goes to that one command
          and isn’t kept.
        </p>
      </Match>
      <Match when={true}>
        <p class="settings-hint setting-explain warning">{props.failure.message}</p>
      </Match>
    </Switch>
  );
}

/** The limits on each user's daemon, for Rovik's administrators there. */
function HostLimits(props: { id: string }) {
  const [limits, setLimits] = createSignal<[string, string][] | null>(null);
  const [edits, setEdits] = createSignal<Record<string, string>>({});
  const [message, setMessage] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function load() {
    setBusy(true);
    try {
      setLimits(await hostLimits(props.id));
      setEdits({});
      setMessage(null);
    } catch (err) {
      setMessage(asDeployFailure(err).message);
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    const changes = Object.entries(edits()).map(([name, value]): [string, string] => [
      name,
      value.trim(),
    ]);
    if (changes.length === 0) return;
    setBusy(true);
    try {
      await setHostLimits(props.id, changes);
      await load();
      setMessage('Saved. Each daemon takes the new limits when it next starts.');
    } catch (err) {
      setMessage(asDeployFailure(err).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="settings-section host-limits">
      <div class="settings-section-title">Limits for everyone</div>
      <p class="settings-hint">
        You’re one of Rovik’s administrators here: these bind every user’s daemon. “none” takes a
        limit away.
      </p>
      <Show
        when={limits()}
        fallback={
          <button disabled={busy()} onClick={() => void load()}>
            {busy() ? 'Asking the server…' : 'Show the limits'}
          </button>
        }
      >
        {(all) => (
          <>
            <div class="host-limits-list">
              <For each={all()}>
                {([name, value]) => (
                  <label class="host-limit">
                    <span>{name}</span>
                    <input
                      value={edits()[name] ?? value}
                      onInput={(e) =>
                        setEdits((current) => ({
                          ...current,
                          [name]: e.currentTarget.value,
                        }))
                      }
                      spellcheck={false}
                    />
                  </label>
                )}
              </For>
            </div>
            <button
              class="chat-action-button primary"
              disabled={busy() || Object.keys(edits()).length === 0}
              onClick={() => void save()}
            >
              Save limits
            </button>
          </>
        )}
      </Show>
      <Show when={message()}>
        <p class="settings-hint">{message()}</p>
      </Show>
    </section>
  );
}
