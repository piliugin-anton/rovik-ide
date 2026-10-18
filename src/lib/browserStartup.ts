// browserStartup.ts
//
// The browser for the web tools: the app's own copy of the Chrome for
// Testing Rovik pins (src-tauri/src/pinned_browser.rs, D19). When the app
// starts and it isn't there, it's downloaded in the background (about
// 200 MB), checked against its pinned SHA-256, with a notification showing
// how far it's got. Until it's ready the web tools use an installed
// Chromium or Chrome, if there is one.
import { Channel, invoke } from '@tauri-apps/api/core';
import { notify, type NotificationOptions } from './notifications';
import { formatBytes } from './textUtils';

export interface BrowserStatus {
  name: string;
  version: string;
  /** Where it's installed, if it is. */
  path: string | null;
  /** The download's size, when there's a build for this system. */
  size: number | null;
}

const NOTE = 'browser-install';

let started = false;
let installing = false;

/** Once per app run: the browser downloaded, in the background, if it's missing. */
export async function startBrowser() {
  if (started) return;
  started = true;
  let status: BrowserStatus;
  try {
    status = await invoke<BrowserStatus>('browser_status');
  } catch {
    return;
  }
  if (needsDownload(status)) await install(status);
}

/** Missing, and there's a build of it for this system. */
export function needsDownload(status: BrowserStatus): boolean {
  return status.path === null && status.size !== null;
}

/** The notification while it downloads, then while it's unpacked. */
export function progressNote(downloaded: number, total: number): Partial<NotificationOptions> {
  if (downloaded >= total) return { progress: 'busy', message: 'Unpacking…' };
  return {
    progress: downloaded / total,
    message: `${formatBytes(downloaded)} of ${formatBytes(total)}`,
  };
}

async function install(status: BrowserStatus) {
  if (installing) return;
  installing = true;
  const note = notify({
    id: NOTE,
    title: `Downloading ${status.name} ${status.version} for the web tools…`,
    ...progressNote(0, status.size ?? 1),
  });
  try {
    const progress = new Channel<{ downloaded: number; total: number }>();
    progress.onmessage = ({ downloaded, total }) => note.update(progressNote(downloaded, total));
    await invoke<string>('browser_install', { onProgress: progress });
    note.update({
      kind: 'success',
      title: `${status.name} ${status.version} is ready for the web tools`,
      message: undefined,
      progress: undefined,
      timeout: 5000,
    });
  } catch (err) {
    note.update({
      kind: 'error',
      title: `${status.name} couldn’t be downloaded`,
      message: `${friendly(err)}\nUntil it is, the web tools use an installed Chromium or Chrome.`,
      progress: undefined,
      actions: [{ label: 'Try again', primary: true, run: () => void install(status) }],
    });
  } finally {
    installing = false;
  }
}

function friendly(err: unknown): string {
  const text = String(err);
  if (/dns|connect|timed out|network|offline/i.test(text)) {
    return 'Google’s download server couldn’t be reached. Check the network connection.';
  }
  return text;
}
