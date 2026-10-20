// wslStartup.ts
//
// WSL at app start, on Windows, in the background (lib/wsl.ts): Rovik in
// the distribution is brought to this app's version when an app update
// left it behind, without asking; a distribution not set up yet gets one
// notification pointing at Settings → This computer → WSL, where the
// agent's commands are refused until it's done. "Don't ask again" stops
// the notification. A distribution set up that can't start (virtualization
// off in the BIOS/UEFI, WSL turned off, its kernel out of date) is told of
// every time, with what to do: nothing runs until it's fixed.
import { openAppSettings } from './appSettings';
import { loadPrefs, savePrefs } from './localPrefs';
import { notify } from './notifications';
import { onWindows, updateWsl, wslStatus, type WslStatus } from './wsl';

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
const showProblem = { label: 'Show', primary: true, run: () => openAppSettings('wsl') };

/** How long a distribution set up gets to connect, before its problem is looked for. */
const CONNECT_WAIT_MS = 60_000;

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
    void warnOfStartupProblem(status);
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
    title: status.available
      ? 'Set up WSL for the agent’s commands'
      : status.unsupported
        ? 'WSL2 can’t run on this Windows'
        : 'The agent needs WSL',
    message: status.available
      ? 'On Windows the agent’s commands run in a WSL distribution, in a sandbox. Until one is set up, they’re refused.'
      : status.unsupported
        ? `${status.unavailable ?? ''} Until then the agent’s commands are refused on this computer.`
        : 'On Windows the agent’s commands run in a WSL distribution, in a sandbox. Install WSL (wsl --install in an administrator PowerShell), then set it up here.',
    actions: [showPage, notNow, dontAsk],
  });
}

/**
 * The distribution set up couldn't start, or the app couldn't connect to
 * it: said plainly, with what to do, once the connection has had its
 * time. Nothing while it connects, or once it has.
 */
async function warnOfStartupProblem(status: WslStatus) {
  let current = status;
  if (current.state === 'connecting' || !current.problem) {
    await new Promise((resolve) => setTimeout(resolve, CONNECT_WAIT_MS));
    try {
      current = await wslStatus();
    } catch {
      return;
    }
  }
  if (!current.problem || current.state === 'ready') return;
  notify({
    id: 'wsl-problem',
    kind: 'error',
    title: `WSL couldn’t start: the agent’s commands can’t run`,
    message: current.problem,
    actions: [showProblem, notNow],
  });
}
