// VirtualFilesSettings.tsx
//
// Settings page for virtual files (lib/virtualFiles.ts): whether the OS's
// filesystem driver is installed, installing it, and the mounted volume.
//
// Right after installing, the volume is mounted, so anything the OS still
// needs (on macOS: switching macFUSE on in System Settings, or allowing its
// kernel extension) shows up at once, with the steps and a button to the
// right System Settings page. While such a step is pending, mounting is
// retried every few seconds and whenever the app is focused again, so the
// page turns to "Mounted" by itself once the user has done it.
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import {
  installVfsDriver,
  mountVfs,
  openVfsSettings,
  unmountVfs,
  vfsStatus,
  type InstallOutcome,
  type VfsStatus,
} from '../lib/virtualFiles';

const RETRY_MS = 3000;

function megabytes(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(bytes < 10 * 1024 * 1024 ? 1 : 0)} MB`;
}

export default function VirtualFilesSettings() {
  const [status, setStatus] = createSignal<VfsStatus | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<InstallOutcome | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  async function refresh() {
    try {
      setStatus(await vfsStatus());
    } catch (err) {
      setError(String(err));
    }
  }

  onMount(refresh);

  async function run(label: string, action: () => Promise<unknown>) {
    setBusy(label);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
      await refresh();
    }
  }

  /** A mount attempt; a failure is shown as the status's problem. */
  let mounting = false;
  async function tryMount() {
    if (mounting) return;
    mounting = true;
    try {
      await mountVfs();
    } catch {
      // Reported by vfs_status as `problem`.
    } finally {
      mounting = false;
      await refresh();
    }
  }

  const install = () =>
    run('Installing…', async () => {
      setOutcome(null);
      const result = await installVfsDriver();
      setOutcome(result);
      // Mount straight away: what the OS still needs shows up now.
      if (result.status === 'installed') await tryMount();
    });

  const driver = () => status()?.driver;
  const problem = () => (status()?.mountpoint ? null : (status()?.problem ?? null));
  const waiting = () => problem()?.retry === true && driver()?.installed === true;

  // Keep trying while the fix is up to the user, outside the app.
  createEffect(() => {
    if (!waiting()) return;
    const timer = setInterval(() => void tryMount(), RETRY_MS);
    const onFocus = () => void tryMount();
    window.addEventListener('focus', onFocus);
    onCleanup(() => {
      clearInterval(timer);
      window.removeEventListener('focus', onFocus);
    });
  });

  return (
    <div class="permissions-settings virtual-files-settings">
      <section class="settings-section">
        <div class="settings-section-title">Virtual files</div>
        <p class="settings-hint">
          Files the app keeps in memory that commands the agent runs can read, write and execute
          like any other file. They need the system’s user-space filesystem driver.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Driver</div>
        <Show when={driver()} fallback={<p class="settings-hint">Checking…</p>}>
          {(d) => (
            <>
              <p class="settings-hint">
                {d().installed
                  ? `${d().driver} is installed${d().version ? ` (${d().version})` : ''}.`
                  : (d().detail ?? `${d().driver} isn’t installed.`)}
              </p>
              <Show when={d().canInstall && (!d().installed || problem()?.kind === 'updateDriver')}>
                <button
                  class="chat-action-button primary"
                  disabled={busy() != null}
                  onClick={install}
                >
                  {busy() ?? (d().installed ? `Update ${d().driver}` : `Install ${d().driver}`)}
                </button>
                <p class="settings-hint">The system will ask for an administrator’s permission.</p>
              </Show>
            </>
          )}
        </Show>
        <Show when={outcome()} keyed>
          {(o) =>
            o.status === 'installed' ? null : (
              <p class="settings-hint setting-explain">
                {o.message}
                {o.status === 'manual' && o.command ? (
                  <code class="virtual-files-command">{o.command}</code>
                ) : null}
              </p>
            )
          }
        </Show>
      </section>

      <Show when={problem()} keyed>
        {(p) => (
          <section class="settings-section virtual-files-problem">
            <div class="settings-section-title">{p.title}</div>
            <Show when={p.steps.length > 0}>
              <ol class="virtual-files-steps">
                <For each={p.steps}>{(step) => <li>{step}</li>}</For>
              </ol>
            </Show>
            <div class="virtual-files-actions">
              <Show when={p.settings} keyed>
                {(pane) => (
                  <button
                    class="chat-action-button primary"
                    onClick={() => openVfsSettings(pane).catch((err) => setError(String(err)))}
                  >
                    Open System Settings
                  </button>
                )}
              </Show>
              <Show when={!p.retry && driver()?.installed}>
                <button
                  class="chat-action-button"
                  disabled={busy() != null}
                  onClick={() => run('Mounting…', tryMount)}
                >
                  {busy() ?? 'Try again'}
                </button>
              </Show>
            </div>
            <Show when={waiting()}>
              <p class="settings-hint">Checking automatically — this page updates once it works.</p>
            </Show>
            <Show when={p.detail}>
              <details class="virtual-files-detail">
                <summary>Details</summary>
                <code class="virtual-files-command">{p.detail}</code>
              </details>
            </Show>
          </section>
        )}
      </Show>

      <Show when={driver()?.installed && status()}>
        {(s) => (
          <section class="settings-section">
            <div class="settings-section-title">Volume</div>
            <p class="settings-hint">
              {s().mountpoint
                ? `Mounted at ${s().mountpoint}.`
                : `Not mounted. It’s mounted at ${s().defaultMountpoint} when first used.`}{' '}
              {megabytes(s().used)} of {megabytes(s().capacity)} used.
            </p>
            <Show when={s().mountpoint || !problem()}>
              <button
                class="chat-action-button"
                disabled={busy() != null}
                onClick={() =>
                  s().mountpoint ? run('Unmounting…', unmountVfs) : run('Mounting…', tryMount)
                }
              >
                {busy() ?? (s().mountpoint ? 'Unmount' : 'Mount now')}
              </button>
            </Show>
          </section>
        )}
      </Show>

      <Show when={error()}>
        <p class="settings-hint setting-explain warning">{error()}</p>
      </Show>
    </div>
  );
}
