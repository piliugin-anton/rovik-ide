// VirtualFilesSettings.tsx
//
// Settings page for virtual files (lib/virtualFiles.ts): whether the OS's
// filesystem driver is installed, installing it, and the mounted volume.
import { createSignal, onMount, Show } from 'solid-js';
import {
  installVfsDriver,
  mountVfs,
  unmountVfs,
  vfsStatus,
  type InstallOutcome,
  type VfsStatus,
} from '../lib/virtualFiles';

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

  const install = () =>
    run('Installing…', async () => {
      setOutcome(null);
      setOutcome(await installVfsDriver());
    });

  const driver = () => status()?.driver;

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
              <Show when={!d().installed && d().canInstall}>
                <button
                  class="chat-action-button primary"
                  disabled={busy() != null}
                  onClick={install}
                >
                  {busy() ?? `Install ${d().driver}`}
                </button>
                <p class="settings-hint">The system will ask for an administrator’s permission.</p>
              </Show>
            </>
          )}
        </Show>
        <Show when={outcome()} keyed>
          {(o) =>
            o.status === 'installed' ? (
              <p class="settings-hint">Installed.</p>
            ) : (
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
            <button
              class="chat-action-button"
              disabled={busy() != null}
              onClick={() =>
                s().mountpoint ? run('Unmounting…', unmountVfs) : run('Mounting…', () => mountVfs())
              }
            >
              {busy() ?? (s().mountpoint ? 'Unmount' : 'Mount now')}
            </button>
          </section>
        )}
      </Show>

      <Show when={error()}>
        <p class="settings-hint setting-explain warning">{error()}</p>
      </Show>
    </div>
  );
}
