import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';
import { reconnectUpdatedBackend, restartBackend } from './backendSession';
import { dismiss, notify } from './notifications';

/** Native comparison of the running backend with the app's installed backend. */
export interface BackendUpdateStatus {
  restartRequired: boolean;
  backendBoot: string;
  backendVersion: string;
  appVersion: string;
  backendProtocol: number;
  appProtocol: number;
  backendBuild: string;
  appBuild: string;
}

/** One notification follows the backend replacement through progress and recovery. */
export const BACKEND_UPDATE_NOTE = 'backend-update';

/** Inputs kept separate from the notification lifecycle for native and browser tests. */
export interface BackendUpdaterDeps {
  status(): Promise<BackendUpdateStatus | null>;
  restart(status: BackendUpdateStatus): Promise<void>;
  reconnect(): Promise<void>;
  onFocus(check: () => void): () => void;
}

const interruption =
  'Restarting stops active agents, preparation, commands and terminals in every Rovik window. Unsaved editor text and chat drafts stay open. Interrupted actions are not retried.';

/** Offers replacement explicitly; neither checking nor focusing the app stops work. */
export function createBackendUpdater(deps: BackendUpdaterDeps) {
  const [available, setAvailable] = createSignal(false);
  let stopped = false;
  let awaitingAppRelaunch = false;
  let checking: Promise<void> | undefined;
  let replacing: Promise<void> | undefined;
  let revision = 0;
  let offered: string | undefined;
  let failed = false;

  function offer(status: BackendUpdateStatus) {
    setAvailable(true);
    offered = JSON.stringify(status);
    failed = false;
    notify({
      id: BACKEND_UPDATE_NOTE,
      kind: 'warning',
      title: 'Backend update ready',
      message: `Rovik ${status.appVersion} is ready to use its updated backend. ${interruption}`,
      actions: [
        { label: 'Restart backend', primary: true, keepOpen: true, run: () => void restart() },
      ],
    });
  }

  function check(): Promise<void> {
    if (stopped || awaitingAppRelaunch || replacing) return Promise.resolve();
    if (checking) return checking;
    const current = revision;
    checking = deps
      .status()
      .then((status) => {
        if (stopped || current !== revision || failed) return;
        if (status?.restartRequired) {
          if (offered !== JSON.stringify(status)) offer(status);
        } else if (offered) {
          setAvailable(false);
          offered = undefined;
          dismiss(BACKEND_UPDATE_NOTE);
        }
      })
      .catch((error: unknown) => {
        if (stopped || current !== revision || failed) return;
        failed = true;
        notify({
          id: BACKEND_UPDATE_NOTE,
          kind: 'error',
          title: 'Couldn’t check the backend update',
          message: `${String(error)} Try checking again.`,
          actions: [
            {
              label: 'Check again',
              primary: true,
              keepOpen: true,
              run: () => {
                failed = false;
                offered = undefined;
                dismiss(BACKEND_UPDATE_NOTE);
                void check();
              },
            },
          ],
        });
      })
      .finally(() => {
        checking = undefined;
      });
    return checking;
  }

  function restart(): Promise<void> {
    if (stopped || awaitingAppRelaunch) return Promise.resolve();
    if (replacing) return replacing;
    revision++;
    notify({
      id: BACKEND_UPDATE_NOTE,
      title: 'Restarting backend…',
      message:
        'Starting the updated backend and restoring this window’s project and subscriptions.',
      progress: 'busy',
    });
    replacing = (async () => {
      try {
        // Another window may already have replaced it, or an earlier attempt
        // may have started the new backend before restoring the UI failed.
        const status = await deps.status();
        if (stopped || awaitingAppRelaunch) return;
        if (status?.restartRequired) await deps.restart(status);
        else await deps.reconnect();
        if (stopped || awaitingAppRelaunch) return;
        setAvailable(false);
        failed = false;
        offered = undefined;
        dismiss('backend-session');
        notify({
          id: BACKEND_UPDATE_NOTE,
          kind: 'success',
          title: 'Backend updated',
          message: 'This window is connected. Review interrupted work before starting it again.',
          timeout: 5000,
        });
      } catch (error) {
        if (stopped || awaitingAppRelaunch) return;
        failed = true;
        notify({
          id: BACKEND_UPDATE_NOTE,
          kind: 'error',
          title: 'Backend restart needs attention',
          message: `${String(error)} ${interruption}`,
          actions: [
            { label: 'Try again', primary: true, keepOpen: true, run: () => void restart() },
          ],
        });
      } finally {
        replacing = undefined;
      }
    })();
    return replacing;
  }

  function start(): () => void {
    stopped = false;
    const stopFocus = deps.onFocus(() => void check());
    void check();
    return () => {
      stopped = true;
      revision++;
      stopFocus();
    };
  }

  function deferUntilRelaunch(): void {
    // Installing can change the executable while this UI still speaks its old
    // protocol. Only the relaunched app can safely offer that backend.
    awaitingAppRelaunch = true;
    revision++;
    setAvailable(false);
    dismiss(BACKEND_UPDATE_NOTE);
  }

  return { start, check, restart, available, deferUntilRelaunch };
}

const updater = createBackendUpdater({
  status: () => invoke<BackendUpdateStatus | null>('desktop_backend_update_status'),
  restart: (status) =>
    restartBackend({
      expectedBoot: status.backendBoot,
      expectedVersion: status.backendVersion,
      expectedProtocol: status.backendProtocol,
      expectedBuild: status.backendBuild,
    }),
  reconnect: reconnectUpdatedBackend,
  onFocus: (check) => {
    window.addEventListener('focus', check);
    return () => window.removeEventListener('focus', check);
  },
});

/** Checks at launch and on focus, including after a newly installed app relaunches. */
export const startBackendUpdates = () => updater.start();
/** Whether native status confirmed a backend replacement is available. */
export const backendUpdateAvailable = updater.available;
/** Leaves installation/relaunch to the app updater while the current UI is still old. */
export const deferBackendUpdateUntilRelaunch = () => updater.deferUntilRelaunch();
