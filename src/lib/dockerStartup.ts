// Local Docker setup uses the same backend-owned installation flow as the browser.
import { Channel, invoke } from '@tauri-apps/api/core';
import { notify, type NotificationOptions } from './notifications';
import { formatBytes } from './textUtils';

export interface DockerStatus {
  installed: boolean;
  ready: boolean;
  path: string | null;
  canInstall: boolean;
  message: string | null;
}

export interface DockerProgress {
  phase: string;
  message: string;
  downloaded: number;
  total: number | null;
}

let started = false;
let installing = false;
const NOTE = 'docker-install';

/** Automatically install a missing local runtime once per window startup. */
export async function startDocker() {
  if (started) return;
  started = true;
  let status: DockerStatus;
  try {
    status = await invoke<DockerStatus>('docker_status');
  } catch {
    return;
  }
  if (needsInstall(status)) await installDocker();
  else if (!status.ready) showStatus(status);
}

/** An existing but stopped Docker daemon must never trigger reinstallation. */
export function needsInstall(status: DockerStatus): boolean {
  return !status.installed && !status.ready && status.canInstall;
}

/** Installation success requires a separate successful daemon readiness check. */
export function statusNote(status: DockerStatus): NotificationOptions {
  return {
    id: NOTE,
    kind: status.ready ? 'success' : 'warning',
    title: status.ready
      ? 'Docker is ready'
      : status.installed
        ? 'Docker needs setup'
        : 'Docker is unavailable',
    message: status.message ?? undefined,
    progress: undefined,
    timeout: status.ready ? 5000 : undefined,
  };
}

export function progressNote(progress: DockerProgress): Partial<NotificationOptions> {
  if (progress.phase === 'downloading' && progress.total && progress.downloaded < progress.total) {
    return {
      progress: progress.downloaded / progress.total,
      message: `${formatBytes(progress.downloaded)} of ${formatBytes(progress.total)}`,
    };
  }
  return { progress: 'busy', message: progress.message };
}

function showStatus(status: DockerStatus) {
  notify({
    ...statusNote(status),
    actions: status.ready ? [] : [{ label: 'Check again', run: () => void checkAgain() }],
  });
}

async function checkAgain() {
  try {
    const status = await invoke<DockerStatus>('docker_status');
    if (needsInstall(status)) await installDocker();
    else showStatus(status);
  } catch (error) {
    notify({
      id: NOTE,
      kind: 'error',
      title: 'Docker status is unavailable',
      message: String(error),
    });
  }
}

async function installDocker() {
  if (installing) return;
  installing = true;
  const note = notify({
    id: NOTE,
    title: 'Setting up Docker for local containers…',
    message:
      'Your system may ask for administrator approval. On Linux, sign out and back in after installation to activate Docker access.',
    progress: 'busy',
  });
  try {
    const progress = new Channel<DockerProgress>();
    progress.onmessage = (update) => note.update(progressNote(update));
    const status = await invoke<DockerStatus>('docker_install', { onProgress: progress });
    showStatus(status);
  } catch (error) {
    note.update({
      kind: 'error',
      title: 'Docker setup could not finish',
      message: String(error),
      progress: undefined,
      actions: [{ label: 'Try again', primary: true, run: () => void checkAgain() }],
    });
  } finally {
    installing = false;
  }
}
