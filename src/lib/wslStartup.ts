// wslStartup.ts
//
// WSL at app start, on Windows, in the background (lib/wsl.ts): Rovik in
// the distribution is brought to this app's version when an app update
// left it behind, without asking; a distribution not set up yet gets one
// notification pointing at Settings → This computer → WSL, where the
// agent's commands are refused until it's done. "Don't ask again" stops
// the notification.
import { openAppSettings } from './appSettings';
import { loadPrefs, savePrefs } from './localPrefs';
import { notify } from './notifications';
import { onWindows, updateWsl, wslStatus } from './wsl';

const PREFS_KEY = 'rovik.wsl';
const NOTE = 'wsl-setup';

/** Stored in localStorage: this machine's choice. */
interface Prefs {
  /** No setup notification at start. */
  quiet?: boolean;
}

const notNow = { label: 'Not now', run: () => {} };
const dontAsk = {
  label: 'Don’t ask again',
  run: () => savePrefs<Prefs>(PREFS_KEY, { quiet: true }),
};
const showPage = { label: 'Set up WSL', primary: true, run: () => openAppSettings('wsl') };

let started = false;

/** Once per app run, in the background. */
export async function startWsl() {
  if (started || !onWindows()) return;
  started = true;
  let status;
  try {
    status = await wslStatus();
  } catch {
    return; // not supported here
  }
  if (status.deployed) {
    if (!status.outdated) return;
    try {
      await updateWsl();
      notify({
        id: NOTE,
        kind: 'success',
        title: `Rovik in ${status.distro ?? 'WSL'} is updated`,
        message: `It now runs Rovik ${status.appVersion}, as this app does.`,
        timeout: 6000,
      });
    } catch (err) {
      notify({
        id: NOTE,
        kind: 'warning',
        title: `Rovik in ${status.distro ?? 'WSL'} couldn’t be updated`,
        message: `${String(err)} Until it is, chats there run an older Rovik.`,
        actions: [showPage, notNow],
      });
    }
    return;
  }
  if (loadPrefs<Prefs>(PREFS_KEY).quiet === true) return;
  notify({
    id: NOTE,
    kind: 'warning',
    title: status.available ? 'Set up WSL for the agent’s commands' : 'The agent needs WSL',
    message: status.available
      ? 'On Windows the agent’s commands run in a WSL distribution, in a sandbox. Until one is set up, they’re refused.'
      : 'On Windows the agent’s commands run in a WSL distribution, in a sandbox. Install WSL (wsl --install in an administrator PowerShell), then set it up here.',
    actions: [showPage, notNow, dontAsk],
  });
}
