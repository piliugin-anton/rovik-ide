// NetworkSettings.tsx
//
// Settings → Network: how long connecting may take, the proxy the app's
// connections go through (none, the first choice; the system's; or a
// custom HTTP or SOCKS5 one), and whether cloud hosts' SSH connections and
// the agent's browsers (this computer's, the cloud hosts', or both) go
// through it too, which only shows while there's a proxy. Each change
// applies at once (lib/network.ts, kept by the backend). The custom proxy
// is saved once it has a host and a port; until then the page says what's
// in use. Its password goes to the keychain.

import { createResource, createSignal, Show } from 'solid-js';
import {
  KIND_LABEL,
  PLACES_LABEL,
  USUAL_PORT,
  networkSettings,
  portOf,
  proxyAddress,
  saveNetwork,
  splitProxyAddress,
  systemProxyLine,
  type BrowserPlaces,
  type CustomProxy,
  type NetworkSettings as Settings,
  type NetworkView,
  type ProxyKind,
} from '../lib/network';
import { unlockSecrets } from '../lib/hosts';
import { NumberField, Segmented, SettingStatus } from './SettingsControls';

/** The custom proxy's fields as typed. */
interface Draft {
  kind: ProxyKind;
  host: string;
  port: string;
  username: string;
}

function draftOf(proxy: CustomProxy | undefined): Draft {
  return {
    kind: proxy?.kind ?? 'http',
    host: proxy?.host ?? '',
    port: proxy ? String(proxy.port) : '',
    username: proxy?.username ?? '',
  };
}

/** The proxy the fields make, once they have a host and a port. */
function proxyOf(draft: Draft): CustomProxy | null {
  const host = draft.host.trim();
  const port = portOf(draft.port);
  if (!host || port === undefined) return null;
  const username = draft.username.trim();
  return { kind: draft.kind, host, port, ...(username ? { username } : {}) };
}

export default function NetworkSettings() {
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [view, { mutate }] = createResource(() =>
    networkSettings().catch((e) => {
      setLoadError(String(e));
      return null;
    })
  );
  const [error, setError] = createSignal<string | null>(null);
  // The fields as typed, while they differ from what's saved.
  const [draft, setDraft] = createSignal<Draft | null>(null);
  // Custom chosen before it has a host and a port.
  const [wantsCustom, setWantsCustom] = createSignal(false);
  const [password, setPassword] = createSignal('');
  const [passphrase, setPassphrase] = createSignal('');
  const [unlockError, setUnlockError] = createSignal<string | null>(null);

  const saved = () => view()!.settings;
  const mode = () => (wantsCustom() ? 'custom' : saved().proxy);
  const fields = () => draft() ?? draftOf(saved().custom);

  async function save(next: Settings, passwordChange?: string): Promise<boolean> {
    setError(null);
    try {
      mutate(await saveNetwork(next, passwordChange));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }

  /** A field changed: saved once the proxy has a host and a port. */
  async function edit(patch: Partial<Draft>) {
    const next = { ...fields(), ...patch };
    setDraft(next);
    const proxy = proxyOf(next);
    if (!proxy) return;
    const choice = mode();
    if (await save({ ...saved(), custom: proxy, proxy: choice })) {
      setDraft(null);
      setWantsCustom(false);
    }
  }

  /** No proxy, or the system's: the custom one is kept for later. */
  async function chooseMode(proxy: 'direct' | 'system') {
    setWantsCustom(false);
    if (saved().proxy !== proxy) await save({ ...saved(), proxy });
  }

  async function chooseCustom() {
    const proxy = proxyOf(fields());
    if (!proxy) {
      setWantsCustom(true);
      return;
    }
    await save({ ...saved(), proxy: 'custom', custom: proxy });
  }

  async function savePassword() {
    if (!password()) return;
    if (await save(saved(), password())) setPassword('');
  }

  /** The encrypted file in the keychain's place: unlocked, the password typed is saved. */
  const locked = (v: NetworkView) => v.store?.kind === 'file' && !v.store.unlocked;
  async function unlock() {
    setUnlockError(null);
    try {
      await unlockSecrets(passphrase());
      setPassphrase('');
      mutate(await networkSettings());
      setError(null);
      await savePassword();
    } catch (e) {
      setUnlockError(String(e));
    }
  }

  /** What connections do while the custom proxy isn't complete. */
  const meanwhile = (v: NetworkView) => {
    const { proxy, custom } = v.settings;
    if (proxy === 'custom' && custom) {
      return `connections use the saved one, ${proxyAddress(custom)}`;
    }
    return proxy === 'direct' ? 'connections go directly' : 'connections use the system’s proxy';
  };
  const noSystemProxy = (v: NetworkView) => !v.system.https && !v.system.http;

  return (
    <div class="permissions-settings network-settings">
      <p class="settings-hint">
        How Rovik connects out: to model providers, to the servers it downloads from (rust-analyzer,
        the agent’s browser, the virtual files’ driver, Rovik for a cloud host), and if you choose,
        to cloud hosts. A provider’s connections are kept open and used again while chats are busy,
        and made afresh after a pause. Git and the commands chats run keep their own proxy settings.
        Applies to this computer only.
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
                  onChange={(timeoutSecs) => void save({ ...saved(), timeoutSecs })}
                />
              </div>
              <p class="settings-hint setting-explain">
                How long to wait for a server, a proxy or a cloud host to take a connection before
                trying again: {v().minTimeoutSecs} to {v().maxTimeoutSecs} seconds,{' '}
                {v().defaultTimeoutSecs} when empty. How long a model may take to start answering is
                set per model, in Model providers.
              </p>
            </section>

            <section class="settings-section">
              <div class="settings-section-title">Proxy</div>
              <div role="radiogroup" aria-label="Proxy">
                <label class="setting-checkbox">
                  <input
                    type="radio"
                    name="network-proxy"
                    checked={mode() === 'direct'}
                    onChange={() => void chooseMode('direct')}
                  />
                  <span>
                    No proxy
                    <span class="setting-checkbox-sub">
                      Connections go directly, even when this computer is set to use a proxy. The
                      agent’s browser follows its own computer’s settings.
                    </span>
                  </span>
                </label>
                <label class="setting-checkbox">
                  <input
                    type="radio"
                    name="network-proxy"
                    checked={mode() === 'system'}
                    onChange={() => void chooseMode('system')}
                  />
                  <span>
                    System proxy
                    <span class="setting-checkbox-sub">
                      The one this computer is set to use, in its network settings on macOS and
                      Windows, or in the HTTPS_PROXY, HTTP_PROXY and ALL_PROXY variables. Now:{' '}
                      {systemProxyLine(v())}
                    </span>
                  </span>
                </label>
                <label class="setting-checkbox">
                  <input
                    type="radio"
                    name="network-proxy"
                    checked={mode() === 'custom'}
                    onChange={() => void chooseCustom()}
                  />
                  <span>
                    Custom
                    <span class="setting-checkbox-sub">
                      A proxy for Rovik alone: HTTP or SOCKS5.
                    </span>
                  </span>
                </label>
              </div>

              <Show when={mode() === 'custom'}>
                <div class="profile-form network-proxy-form">
                  <div class="network-proxy-kind">
                    <span>Type</span>
                    <Segmented
                      label="Proxy type"
                      value={fields().kind}
                      options={[
                        {
                          value: 'http',
                          label: KIND_LABEL.http,
                          title: 'HTTPS goes through it by CONNECT',
                        },
                        {
                          value: 'socks5',
                          label: KIND_LABEL.socks5,
                          title: 'The proxy looks the names up itself',
                        },
                      ]}
                      onChange={(kind) => void edit({ kind })}
                    />
                  </div>
                  <div class="network-proxy-row">
                    <label class="network-proxy-host">
                      Host
                      <input
                        value={fields().host}
                        placeholder="proxy.example.com"
                        spellcheck={false}
                        autocomplete="off"
                        onInput={(e) => setDraft({ ...fields(), host: e.currentTarget.value })}
                        onChange={(e) => {
                          // A proxy's URL pasted whole gives its kind and port.
                          const { host, port, kind } = splitProxyAddress(e.currentTarget.value);
                          void edit({
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
                        value={fields().port}
                        placeholder={String(USUAL_PORT[fields().kind])}
                        onInput={(e) => setDraft({ ...fields(), port: e.currentTarget.value })}
                        onChange={(e) => void edit({ port: e.currentTarget.value })}
                      />
                    </label>
                  </div>
                  <div class="network-proxy-row">
                    <label>
                      Username
                      <input
                        value={fields().username}
                        placeholder="Only if the proxy asks for one"
                        spellcheck={false}
                        autocomplete="off"
                        onInput={(e) => setDraft({ ...fields(), username: e.currentTarget.value })}
                        onChange={(e) => void edit({ username: e.currentTarget.value })}
                      />
                    </label>
                    <label>
                      Password
                      <input
                        type="password"
                        value={password()}
                        placeholder={
                          v().passwordSaved
                            ? 'Saved: leave empty to keep it'
                            : 'Only if the proxy asks for one'
                        }
                        autocomplete="off"
                        onInput={(e) => setPassword(e.currentTarget.value)}
                        onChange={() => void savePassword()}
                      />
                    </label>
                  </div>
                  <Show when={locked(v())}>
                    <p class="settings-hint">
                      This computer has no keychain, so the password goes in a file encrypted under
                      a passphrase, with the API keys. It’s locked: enter its passphrase to save the
                      password. The first time, the passphrase you choose here makes the file.
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
                  <Show when={v().passwordSaved}>
                    <button
                      class="link-button network-forget"
                      onClick={() => void save(saved(), '')}
                    >
                      Forget the saved password
                    </button>
                  </Show>
                  <Show when={!proxyOf(fields())}>
                    <SettingStatus warning>
                      Not in use yet: enter the proxy’s host and port. Until then, {meanwhile(v())}.
                    </SettingStatus>
                  </Show>
                  <p class="settings-hint">
                    Addresses on this computer (localhost) are always reached directly.
                  </p>
                  <Show
                    when={
                      v().settings.browser && fields().kind === 'socks5' && fields().username.trim()
                    }
                  >
                    <p class="settings-hint">
                      The agent’s browser can’t sign in to a SOCKS5 proxy, so the pages it opens
                      won’t load through this one.
                    </p>
                  </Show>
                </div>
              </Show>

              {/* What goes through the proxy besides: nothing to choose with none. */}
              <Show when={mode() !== 'direct'}>
                <label class="setting-checkbox network-cloud-hosts">
                  <input
                    type="checkbox"
                    checked={v().settings.cloudHosts}
                    onChange={(e) => void save({ ...saved(), cloudHosts: e.currentTarget.checked })}
                  />
                  <span>
                    Use proxy for cloud hosts
                    <span class="setting-checkbox-sub">
                      SSH connections to cloud hosts go through the proxy too: an HTTP proxy by
                      CONNECT, a SOCKS5 proxy as it is. Hosts set to use your system’s ssh follow
                      its own settings (ProxyJump, ProxyCommand) instead.
                    </span>
                  </span>
                </label>
                <label class="setting-checkbox network-browser">
                  <input
                    type="checkbox"
                    checked={v().settings.browser}
                    onChange={(e) => void save({ ...saved(), browser: e.currentTarget.checked })}
                  />
                  <span>
                    Use proxy for browser
                    <span class="setting-checkbox-sub">
                      The agent’s browser goes through the proxy too: the one on this computer
                      (local), the ones cloud hosts start for the chats on them (cloud), or both.
                      Otherwise a browser follows its own computer’s proxy settings.
                    </span>
                  </span>
                </label>
                <div class="network-browser-places">
                  <select
                    class="settings-select"
                    aria-label="Which browsers"
                    value={v().settings.browserPlaces}
                    disabled={!v().settings.browser}
                    onChange={(e) =>
                      void save({
                        ...saved(),
                        browserPlaces: e.currentTarget.value as BrowserPlaces,
                      })
                    }
                  >
                    <option value="local">{PLACES_LABEL.local}</option>
                    <option value="cloud">{PLACES_LABEL.cloud}</option>
                    <option value="both">{PLACES_LABEL.both}</option>
                  </select>
                </div>
                <Show when={v().settings.browser && v().settings.browserPlaces !== 'local'}>
                  <p class="settings-hint setting-explain network-browser-note">
                    A cloud host’s browser is given the proxy, and its password, each time this
                    computer connects to the host: the host keeps the password in memory only, and
                    has to be able to reach the proxy.
                    {mode() === 'system' ? ' With the system’s proxy, that’s this computer’s.' : ''}
                  </p>
                </Show>
                <Show
                  when={
                    (v().settings.cloudHosts || v().settings.browser) &&
                    mode() === 'system' &&
                    noSystemProxy(v())
                  }
                >
                  <SettingStatus>No effect now: this computer has no proxy set.</SettingStatus>
                </Show>
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
