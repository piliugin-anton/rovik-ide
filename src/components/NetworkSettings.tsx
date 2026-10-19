// NetworkSettings.tsx
//
// Settings → Network: how long connecting may take; the proxies Rovik can
// use, each typed once, its password in the keychain; and where each kind
// of connection goes, from this computer (model providers and downloads,
// SSH to cloud hosts, the agent's browser) and from cloud hosts (their
// requests and their browsers): automatically, by its own settings, with
// no proxy, through this computer's proxy, or through a saved one. A route
// applies as it's chosen, a proxy once it's saved (lib/network.ts, kept by
// the backend). Notes show only while they apply.

import { createResource, createSignal, For, Show } from 'solid-js';
import {
  KIND_LABEL,
  ROWS,
  USUAL_PORT,
  networkSettings,
  newProxyId,
  portOf,
  proxyAddress,
  proxyUses,
  routeOf,
  routeProxy,
  routeValue,
  rowOptions,
  saveNetwork,
  splitProxyAddress,
  systemHasProxy,
  systemProxyLine,
  type NetworkSettings as Settings,
  type NetworkView,
  type ProxyKind,
  type RouteRow,
  type SavedProxy,
} from '../lib/network';
import { unlockSecrets } from '../lib/hosts';
import { NumberField, Segmented, SettingStatus } from './SettingsControls';

/** The proxy editor's fields as typed. */
interface Draft {
  id: string;
  /** Not saved yet: Save adds it. */
  isNew: boolean;
  kind: ProxyKind;
  host: string;
  port: string;
  username: string;
  /** A new password; empty keeps the saved one. */
  password: string;
}

const EMPTY: Draft = {
  id: '',
  isNew: true,
  kind: 'http',
  host: '',
  port: '',
  username: '',
  password: '',
};

function draftOf(proxy: SavedProxy): Draft {
  return {
    id: proxy.id,
    isNew: false,
    kind: proxy.kind,
    host: proxy.host,
    port: String(proxy.port),
    username: proxy.username ?? '',
    password: '',
  };
}

/** The proxy the fields make, once they have a host and a port. */
function proxyOf(draft: Draft): SavedProxy | null {
  const host = draft.host.trim();
  const port = portOf(draft.port);
  if (!host || port === undefined) return null;
  const username = draft.username.trim();
  return { id: draft.id, kind: draft.kind, host, port, ...(username ? { username } : {}) };
}

/** Kinds of connection in a few words: "model providers and the browser". */
function namesOf(rows: RouteRow[]): string {
  const names = rows.map((row) => ROWS.find((r) => r.row === row)!.name);
  if (names.length < 2) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/** This computer's kinds of connection, which "System proxy" can be for. */
const HERE: RouteRow[] = ['models', 'ssh', 'browser'];

export default function NetworkSettings() {
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [view, { mutate }] = createResource(() =>
    networkSettings().catch((e) => {
      setLoadError(String(e));
      return null;
    })
  );
  // A route's or the timeout's save, refused.
  const [error, setError] = createSignal<string | null>(null);
  // The proxy being added or edited, and why its save was refused.
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [draftError, setDraftError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [passphrase, setPassphrase] = createSignal('');
  const [unlockError, setUnlockError] = createSignal<string | null>(null);

  const settings = () => view()!.settings;

  async function save(next: Settings): Promise<void> {
    setError(null);
    try {
      mutate(await saveNetwork(next));
    } catch (e) {
      setError(String(e));
    }
  }

  const choose = (row: RouteRow, value: string) =>
    void save({ ...settings(), routes: { ...settings().routes, [row]: routeOf(value) } });

  function add() {
    setDraft({ ...EMPTY, id: newProxyId(settings().proxies.map((p) => p.id)) });
    setDraftError(null);
  }

  function edit(proxy: SavedProxy) {
    setDraft(draftOf(proxy));
    setDraftError(null);
  }

  const change = (patch: Partial<Draft>) => setDraft({ ...(draft() ?? EMPTY), ...patch });

  function cancel() {
    setDraft(null);
    setDraftError(null);
    setUnlockError(null);
  }

  /** Saves the proxy the editor has, with the password typed. */
  async function saveDraft() {
    const typed = draft();
    const proxy = typed && proxyOf(typed);
    if (!typed || !proxy || busy()) return;
    const proxies = typed.isNew
      ? [...settings().proxies, proxy]
      : settings().proxies.map((p) => (p.id === proxy.id ? proxy : p));
    const passwords = typed.password ? { [typed.id]: typed.password } : undefined;
    setBusy(true);
    setDraftError(null);
    try {
      mutate(await saveNetwork({ ...settings(), proxies }, passwords));
      setDraft(null);
    } catch (e) {
      setDraftError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function forget(id: string) {
    setDraftError(null);
    try {
      mutate(await saveNetwork(settings(), { [id]: '' }));
    } catch (e) {
      setDraftError(String(e));
    }
  }

  async function remove(id: string) {
    if (draft()?.id === id) cancel();
    await save({ ...settings(), proxies: settings().proxies.filter((p) => p.id !== id) });
  }

  /** The encrypted file in the keychain's place: unlocked, the proxy is saved again. */
  const locked = (v: NetworkView) => v.store?.kind === 'file' && !v.store.unlocked;
  async function unlock() {
    setUnlockError(null);
    try {
      await unlockSecrets(passphrase());
      setPassphrase('');
      mutate(await networkSettings());
      await saveDraft();
    } catch (e) {
      setUnlockError(String(e));
    }
  }

  // What the notes are about.
  const cloudProxy = (v: NetworkView) =>
    routeProxy(v.settings.routes.hostModels) !== undefined ||
    routeProxy(v.settings.routes.hostBrowser) !== undefined;
  /** The SOCKS5 proxies a browser would have to sign in to, which it can't. */
  const socksSignIns = (v: NetworkView) => {
    const ids = [v.settings.routes.browser, v.settings.routes.hostBrowser].map(routeProxy);
    return v.settings.proxies.filter(
      (p) => ids.includes(p.id) && p.kind === 'socks5' && Boolean(p.username)
    );
  };
  const systemNone = (v: NetworkView) =>
    !systemHasProxy(v) && HERE.some((row) => v.settings.routes[row] === 'system');

  const proxyRow = (proxy: SavedProxy) => {
    const uses = () => proxyUses(settings(), proxy.id);
    return (
      <div class="profile-row network-proxy">
        <div class="profile-row-info">
          <span class="profile-row-label">
            {KIND_LABEL[proxy.kind]} {proxyAddress(proxy)}
          </span>
          <span class="profile-row-sub">
            {proxy.username
              ? `Signs in as ${proxy.username}${proxy.passwordSaved ? '' : ', no password saved'}`
              : 'No sign-in'}
            {uses().length > 0 ? ` · In use: ${namesOf(uses())}` : ''}
          </span>
        </div>
        <div class="profile-row-actions">
          <button onClick={() => edit(proxy)}>Edit</button>
          <button
            disabled={uses().length > 0}
            title={
              uses().length > 0
                ? `In use: ${namesOf(uses())}. Choose something else there first.`
                : undefined
            }
            onClick={() => void remove(proxy.id)}
          >
            Remove
          </button>
        </div>
      </div>
    );
  };

  const editor = () => {
    const d = () => draft() ?? EMPTY;
    const saved = () => settings().proxies.find((p) => p.id === d().id);
    return (
      <div class="profile-form network-proxy-form">
        <div class="network-proxy-kind">
          <span>Type</span>
          <Segmented
            label="Proxy type"
            value={d().kind}
            options={[
              {
                value: 'http',
                label: KIND_LABEL.http,
                title: 'HTTPS and SSH go through it by CONNECT',
              },
              {
                value: 'socks5',
                label: KIND_LABEL.socks5,
                title: 'The proxy looks the names up itself',
              },
            ]}
            onChange={(kind) => change({ kind })}
          />
        </div>
        <div class="network-proxy-row">
          <label class="network-proxy-host">
            Host
            <input
              value={d().host}
              placeholder="proxy.example.com"
              spellcheck={false}
              autocomplete="off"
              onInput={(e) => change({ host: e.currentTarget.value })}
              onChange={(e) => {
                // A proxy's URL pasted whole gives its kind and port.
                const { host, port, kind } = splitProxyAddress(e.currentTarget.value);
                change({
                  host,
                  ...(port !== undefined ? { port: String(port) } : {}),
                  ...(kind ? { kind } : {}),
                });
              }}
            />
          </label>
          <label class="network-proxy-port">
            Port
            <input
              type="number"
              min="1"
              max="65535"
              value={d().port}
              placeholder={String(USUAL_PORT[d().kind])}
              onInput={(e) => change({ port: e.currentTarget.value })}
            />
          </label>
        </div>
        <div class="network-proxy-row">
          <label>
            Username
            <input
              value={d().username}
              placeholder="Only if the proxy asks for one"
              spellcheck={false}
              autocomplete="off"
              onInput={(e) => change({ username: e.currentTarget.value })}
            />
          </label>
          <label>
            Password
            <input
              type="password"
              value={d().password}
              placeholder={
                saved()?.passwordSaved
                  ? 'Saved: leave empty to keep it'
                  : 'Only if the proxy asks for one'
              }
              autocomplete="off"
              onInput={(e) => change({ password: e.currentTarget.value })}
            />
          </label>
        </div>
        <Show when={saved()?.passwordSaved}>
          <button class="link-button network-forget" onClick={() => void forget(d().id)}>
            Forget the saved password
          </button>
        </Show>
        <Show when={locked(view()!) && Boolean(d().password || d().username.trim())}>
          <p class="settings-hint">
            This computer has no keychain, so the password goes in a file encrypted under a
            passphrase, with the API keys. It’s locked: enter its passphrase to save the password.
            The first time, the passphrase you choose here makes the file.
          </p>
          <div class="host-unlock">
            <input
              type="password"
              value={passphrase()}
              onInput={(e) => setPassphrase(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && passphrase()) void unlock();
              }}
              placeholder="Passphrase"
              aria-label="passphrase of the secrets file"
            />
            <button
              class="chat-action-button primary"
              disabled={!passphrase()}
              onClick={() => void unlock()}
            >
              Unlock
            </button>
          </div>
          <Show when={unlockError()}>
            <SettingStatus warning>{unlockError()}</SettingStatus>
          </Show>
        </Show>
        <Show when={draftError()}>
          <SettingStatus warning>{draftError()}</SettingStatus>
        </Show>
        <div class="profile-form-actions">
          <button onClick={cancel}>Cancel</button>
          <button
            class="primary"
            disabled={!proxyOf(d()) || busy()}
            onClick={() => void saveDraft()}
          >
            Save
          </button>
        </div>
      </div>
    );
  };

  return (
    <div class="permissions-settings network-settings">
      <p class="settings-hint">
        How Rovik connects out, from this computer and from cloud hosts: the proxies it can use, and
        which connections go through which. Git and the commands chats run keep their own proxy
        settings.
      </p>

      <Show when={loadError()}>
        <SettingStatus warning>The network settings couldn’t be read: {loadError()}</SettingStatus>
      </Show>
      <Show
        when={view()}
        fallback={
          <Show when={!loadError()}>
            <p class="settings-hint">Reading the settings…</p>
          </Show>
        }
      >
        {(v) => (
          <>
            <section class="settings-section">
              <div class="settings-section-title">Connections</div>
              <div class="setting-fields">
                <NumberField
                  label="Timeout (seconds)"
                  value={v().settings.timeoutSecs}
                  placeholder={v().defaultTimeoutSecs}
                  min={v().minTimeoutSecs}
                  max={v().maxTimeoutSecs}
                  onChange={(timeoutSecs) => void save({ ...settings(), timeoutSecs })}
                />
              </div>
              <p class="settings-hint setting-explain">
                How long to wait for a server, a proxy or a cloud host to take a connection:{' '}
                {v().minTimeoutSecs} to {v().maxTimeoutSecs} seconds, {v().defaultTimeoutSecs} when
                empty. Cloud hosts are given it too. How long a model may take to start answering is
                set per model, in Model providers.
              </p>
            </section>

            <section class="settings-section">
              <div class="settings-section-title">Proxies</div>
              <div class="network-proxies">
                <For each={v().settings.proxies}>
                  {(proxy) => (
                    <Show
                      when={draft()?.id === proxy.id && !draft()?.isNew}
                      fallback={proxyRow(proxy)}
                    >
                      {editor()}
                    </Show>
                  )}
                </For>
              </div>
              <Show when={draft()?.isNew}>{editor()}</Show>
              <Show when={!draft()}>
                <Show when={v().settings.proxies.length === 0}>
                  <p class="settings-hint">None yet: add one to send connections through it.</p>
                </Show>
                <button class="add-profile-button network-add-proxy" onClick={add}>
                  + Add a proxy
                </button>
              </Show>
              <p class="settings-hint network-system-line">
                This computer’s proxy, for “System proxy”: {systemProxyLine(v())}.
                <Show when={v().settings.proxies.length > 0}>
                  {' '}
                  Addresses on this computer (localhost) are reached directly, around saved proxies.
                </Show>
              </p>
            </section>

            <section class="settings-section">
              <div class="settings-section-title">Where connections go</div>
              <For each={['here', 'cloud'] as const}>
                {(place) => (
                  <div class="network-route-group">
                    <div class="network-route-group-title">
                      {place === 'here' ? 'This computer' : 'Cloud hosts'}
                    </div>
                    <For each={ROWS.filter((r) => r.place === place)}>
                      {(info) => {
                        const value = () => routeValue(v().settings.routes[info.row]);
                        return (
                          <label class="network-route">
                            <span class="network-route-label">{info.label}</span>
                            <select
                              class="settings-select"
                              aria-label={info.ariaLabel}
                              value={value()}
                              onChange={(e) => choose(info.row, e.currentTarget.value)}
                            >
                              <For each={rowOptions(v(), info.row)}>
                                {(option) => (
                                  <option value={option.value} selected={option.value === value()}>
                                    {option.label}
                                  </option>
                                )}
                              </For>
                            </select>
                          </label>
                        );
                      }}
                    </For>
                  </div>
                )}
              </For>

              <Show when={cloudProxy(v())}>
                <p class="settings-hint network-note">
                  Cloud hosts are given the proxy, and its password, each time this computer
                  connects to them, and keep the password in memory only. They have to be able to
                  reach it; a host other computers use follows the last one to connect.
                </p>
              </Show>
              <Show when={socksSignIns(v()).length > 0}>
                <p class="settings-hint network-note">
                  The agent’s browser can’t sign in to a SOCKS5 proxy, so pages it opens won’t load
                  through {socksSignIns(v()).map(proxyAddress).join(' or ')}.
                </p>
              </Show>
              <Show when={v().settings.routes.ssh !== 'direct'}>
                <p class="settings-hint network-note">
                  Hosts set to use your system’s ssh follow its own settings (ProxyJump,
                  ProxyCommand) instead.
                </p>
              </Show>
              <Show when={systemNone(v())}>
                <SettingStatus>
                  This computer has no proxy set, so “System proxy” goes directly for now.
                </SettingStatus>
              </Show>
              <Show when={error()}>
                <SettingStatus warning>{error()}</SettingStatus>
              </Show>
            </section>
          </>
        )}
      </Show>
    </div>
  );
}
