// WslSettings.tsx
//
// Settings → This computer → WSL (Windows only): the WSL2 distribution
// Rovik's Linux side runs in. Setting it up installs Rovik's daemon in the
// distribution (as root inside it, no password), with bubblewrap for the
// agent's sandbox and FUSE for the chats' overlays, and connects it. The
// page shows what's there and what's wrong; Rovik in the distribution
// follows this app's updates on its own, so there's no upgrade here, and
// no limits or chat list: its chats are this computer's.
import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { confirmDialog } from './ConfirmDialog';
import { CHECK_LABELS, hostStatus, onHostTest, testVerdict, type Step } from '../lib/hosts';
import {
  afterInstall,
  asFailure,
  installWsl,
  removeWsl,
  setUpWsl,
  testWsl,
  updateWsl,
  wslStatus,
  WSL_HOST,
  type WslStatus,
} from '../lib/wsl';

export default function WslSettings() {
  const [status, setStatus] = createSignal<WslStatus | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [distro, setDistro] = createSignal('');
  const [user, setUser] = createSignal('');
  const [choosing, setChoosing] = createSignal(false);
  const [steps, setSteps] = createSignal<Step[]>([]);
  const [testing, setTesting] = createSignal(false);
  const [installed, setInstalled] = createSignal<string | null>(null);

  async function refresh() {
    try {
      const next = await wslStatus();
      setStatus(next);
      if (!distro()) setDistro(next.distro ?? next.distros[0] ?? '');
      if (!user() && next.user) setUser(next.user);
    } catch (err) {
      setError(String(err));
    }
  }

  onMount(() => void refresh());

  let stopListening: (() => void) | undefined;
  onMount(async () => {
    stopListening = await onHostTest((hostId, step) => {
      if (hostId === WSL_HOST) setSteps((s) => [...s, step]);
    });
  });
  onCleanup(() => stopListening?.());

  async function run(label: string, action: () => Promise<unknown>) {
    setBusy(label);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(asFailure(err).message);
    } finally {
      setBusy(null);
      await refresh();
    }
  }

  const setUp = () =>
    run('Setting up…', async () => {
      await setUpWsl(distro().trim(), user().trim() || undefined);
      setChoosing(false);
    });

  const update = () => run('Updating…', updateWsl);

  const install = () =>
    run('Installing WSL…', async () => {
      setInstalled(afterInstall(await installWsl(distro().trim() || undefined)));
    });

  async function test() {
    setSteps([]);
    setTesting(true);
    setError(null);
    try {
      await testWsl();
    } catch (err) {
      setError(String(err));
    } finally {
      setTesting(false);
    }
  }

  async function remove() {
    const ok = await confirmDialog({
      title: 'Remove Rovik from the distribution?',
      message:
        'Rovik’s daemon and its files in the distribution go. The distribution itself stays. Until WSL is set up again, the agent’s commands are refused on this computer.',
      confirmLabel: 'Remove',
      danger: true,
    });
    if (!ok) return;
    await run('Removing…', removeWsl);
  }

  const deployed = () => status()?.deployed === true;
  const connection = () => {
    const s = status();
    return s
      ? hostStatus({ deploy: { state: 'deployed', version: s.version ?? '' }, state: s.state })
      : null;
  };
  const verdict = () => testVerdict(steps(), testing());

  return (
    <div class="permissions-settings wsl-settings">
      <section class="settings-section">
        <div class="settings-section-title">What it is</div>
        <p class="settings-hint">
          On Windows, Rovik’s Linux side runs in a WSL2 distribution: every chat of a project moves
          there by itself when it first runs, and the agent’s commands run there in a sandbox
          (bubblewrap: the system read-only but for the chat’s own folders, your credentials hidden,
          no other process in sight), with the chats’ overlays on FUSE. Setting it up installs Rovik
          in the distribution, as root inside it, with the packages it needs, and starts its daemon;
          Rovik there follows this app’s updates on its own. Install the toolchains your projects
          need (node, cargo, python…) in the distribution, as the commands run there. A chat without
          a project stays on this computer, where commands are refused.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">WSL on this computer</div>
        <Show when={status()} fallback={<p class="settings-hint">Looking…</p>}>
          {(s) => (
            <>
              <Show when={!s().available}>
                <p class="settings-hint setting-explain warning">
                  WSL isn’t available: {s().unavailable}
                </p>
                <Show when={installed()}>
                  <p class="settings-hint setting-explain">{installed()}</p>
                </Show>
                <p class="settings-hint">
                  <Show when={!s().unsupported} fallback={<>Until Windows is updated, </>}>
                    Install it here (Windows asks for an administrator’s permission; it may take a
                    while, and the computer may have to restart), or from an administrator
                    PowerShell with <code>wsl --install</code>. Until then{' '}
                  </Show>
                  the agent’s commands are refused on this computer.
                </p>
                <Show when={!s().unsupported}>
                  <label>
                    Distribution to install
                    <input
                      value={distro()}
                      onInput={(e) => setDistro(e.currentTarget.value)}
                      placeholder="Ubuntu"
                      spellcheck={false}
                    />
                  </label>
                  <div class="host-actions">
                    <button
                      class="chat-action-button primary"
                      disabled={busy() != null}
                      onClick={() => void install()}
                    >
                      {busy() ?? 'Install WSL'}
                    </button>
                  </div>
                </Show>
              </Show>
              <Show when={s().available && (!deployed() || choosing())}>
                <label>
                  Distribution
                  <input
                    list="wsl-distros"
                    value={distro()}
                    onInput={(e) => setDistro(e.currentTarget.value)}
                    placeholder={s().distros[0] ?? 'Ubuntu'}
                    spellcheck={false}
                  />
                  <datalist id="wsl-distros">
                    <For each={s().distros}>{(name) => <option value={name} />}</For>
                  </datalist>
                </label>
                <Show when={s().distros.length === 0}>
                  <p class="settings-hint setting-explain warning">
                    No distribution is installed. Install one from an administrator PowerShell, e.g.{' '}
                    <code>wsl --install -d Ubuntu</code>, then come back here.
                  </p>
                </Show>
                <label>
                  User in the distribution
                  <input
                    value={user()}
                    onInput={(e) => setUser(e.currentTarget.value)}
                    placeholder="its default user (not root)"
                    spellcheck={false}
                  />
                </label>
                <div class="host-actions">
                  <button
                    class="chat-action-button primary"
                    disabled={busy() != null || !distro().trim()}
                    onClick={() => void setUp()}
                  >
                    {busy() ?? (deployed() ? 'Set up again' : 'Set up')}
                  </button>
                  <Show when={deployed()}>
                    <button disabled={busy() != null} onClick={() => setChoosing(false)}>
                      Cancel
                    </button>
                  </Show>
                </div>
              </Show>
              <Show when={s().available && deployed() && !choosing()}>
                <p class="settings-hint">
                  Rovik {s().version ?? ''} runs in <strong>{s().distro}</strong>
                  {s().user ? ` as ${s().user}` : ''} ·{' '}
                  <span class={`host-status ${connection()?.tone ?? 'muted'}`}>
                    {connection()?.label ?? ''}
                  </span>
                </p>
                <Show when={s().problem}>
                  <p class="settings-hint setting-explain warning">{s().problem}</p>
                </Show>
                <Show when={s().outdated}>
                  <p class="settings-hint setting-explain warning">
                    Rovik in the distribution is older than this app ({s().appVersion}). It’s
                    updated when the app starts; update it now if you’d rather not wait.
                  </p>
                </Show>
                <div class="host-actions">
                  <Show when={s().outdated}>
                    <button
                      class="chat-action-button primary"
                      disabled={busy() != null}
                      onClick={() => void update()}
                    >
                      {busy() ?? 'Update now'}
                    </button>
                  </Show>
                  <button disabled={busy() != null || testing()} onClick={() => void test()}>
                    {testing() ? 'Testing…' : 'Test'}
                  </button>
                  <button disabled={busy() != null} onClick={() => setChoosing(true)}>
                    Change distribution…
                  </button>
                  <button class="danger" disabled={busy() != null} onClick={() => void remove()}>
                    Remove…
                  </button>
                </div>
              </Show>
            </>
          )}
        </Show>
        <Show when={error()}>
          <div class="provider-test-status error">{error()}</div>
        </Show>
      </section>

      <Show when={steps().length > 0 || testing()}>
        <section class="settings-section">
          <div class="settings-section-title">
            Test{' '}
            <Show when={verdict()}>
              {(v) => <span class={`host-status ${v().tone}`}>{v().label}</span>}
            </Show>
          </div>
          <For each={steps()}>
            {(step) => (
              <p class="settings-hint" classList={{ warning: step.outcome === 'failed' }}>
                <strong>{CHECK_LABELS[step.check]}</strong> · {step.outcome}: {step.message}
              </p>
            )}
          </For>
        </section>
      </Show>
    </div>
  );
}
