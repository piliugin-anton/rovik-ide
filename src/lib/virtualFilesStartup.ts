// virtualFilesStartup.ts
//
// Virtual files at app start (lib/virtualFiles.ts), in the background:
// - the driver is installed → mount, so the folder exists before the agent
//   needs it; if that fails (macOS: macFUSE not switched on yet…), one
//   notification offers the steps (Settings → Virtual files, which keeps
//   retrying while the user does them);
// - it's missing → one notification offers to install it, then mounts.
// "Don't ask again" stops the notifications (not the mount attempt).
import { createSignal } from 'solid-js';
import { notify } from './notifications';
import { installVfsDriver, mountVfs, vfsStatus, type MountProblem } from './virtualFiles';

const PREFS_KEY = 'rovik.virtualFiles';
const NOTE = 'vfs-setup';

/** Stored in localStorage: this machine's choice. */
interface Prefs {
  /** No setup notifications at start. */
  quiet?: boolean;
}

function loadPrefs(): Prefs {
  try {
    return JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}') ?? {};
  } catch {
    return {};
  }
}

function savePrefs(patch: Partial<Prefs>) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ ...loadPrefs(), ...patch }));
  } catch {
    // storage unavailable: the choice just isn't remembered
  }
}

// "Show steps": the chat's settings open on the Virtual files page. The
// request waits until the chat panel (which owns the settings) is there.
const [settingsRequested, setSettingsRequested] = createSignal(false);
export const vfsSettingsRequested = settingsRequested;
export function requestVfsSettings() {
  setSettingsRequested(true);
}
export function clearVfsSettingsRequest() {
  setSettingsRequested(false);
}

const notNow = { label: 'Not now', run: () => {} };
const dontAsk = { label: 'Don’t ask again', run: () => savePrefs({ quiet: true }) };
const showSteps = { label: 'Show steps', primary: true, run: requestVfsSettings };

/** Mounts, reporting a failure; true when mounted. */
async function mountAndReport(): Promise<boolean> {
  try {
    const at = await mountVfs();
    notify({
      id: NOTE,
      kind: 'success',
      title: 'Virtual files are ready',
      message: at,
      timeout: 6000,
    });
    return true;
  } catch {
    const status = await vfsStatus().catch(() => null);
    reportProblem(status?.problem ?? null);
    return false;
  }
}

function reportProblem(problem: MountProblem | null) {
  notify({
    id: NOTE,
    kind: 'warning',
    title: problem?.title ?? 'Virtual files couldn’t be mounted',
    message: problem?.steps[0] ?? problem?.detail ?? undefined,
    actions: [showSteps, notNow, dontAsk],
  });
}

async function install() {
  const note = notify({
    id: NOTE,
    title: 'Installing…',
    message: 'The system will ask for permission.',
    progress: 'busy',
  });
  try {
    const outcome = await installVfsDriver();
    if (outcome.status === 'installed') {
      note.update({ title: 'Mounting virtual files…', message: undefined });
      await mountAndReport();
    } else {
      notify({
        id: NOTE,
        kind: 'warning',
        title: outcome.status === 'manual' ? 'Install it yourself' : 'Restart needed',
        message:
          outcome.status === 'manual' && outcome.command
            ? `${outcome.message} ${outcome.command}`
            : outcome.message,
        actions: [showSteps, notNow],
      });
    }
  } catch (err) {
    notify({
      id: NOTE,
      kind: 'error',
      title: 'Installing failed',
      message: String(err),
      actions: [showSteps, notNow],
    });
  }
}

let started = false;

/** Once per app run, in the background. */
export async function startVirtualFiles() {
  if (started) return;
  started = true;
  let status;
  try {
    status = await vfsStatus();
  } catch {
    return; // not supported here
  }
  const quiet = loadPrefs().quiet === true;
  if (status.driver.installed) {
    if (status.mountpoint) return;
    try {
      await mountVfs();
    } catch {
      if (!quiet) reportProblem((await vfsStatus().catch(() => null))?.problem ?? null);
    }
    return;
  }
  if (quiet || !status.driver.canInstall) return;
  notify({
    id: NOTE,
    title: `Virtual files need ${status.driver.driver}`,
    message:
      'They let commands the agent runs use files the app keeps in memory. Installing asks for administrator permission.',
    actions: [
      { label: `Install ${status.driver.driver}`, primary: true, run: () => void install() },
      notNow,
      dontAsk,
    ],
  });
}
