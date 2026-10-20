// sandboxStartup.ts
//
// The command sandbox at app start (lib/sandbox.ts), in the background. On
// Linux the agent's commands run under bubblewrap, on macOS under Seatbelt,
// and are refused without it. Where it's missing or blocked and the app can
// fix that (bubblewrap's package, or the AppArmor profile Ubuntu 23.10+
// needs), it sets it up by itself: the system asks for administrator
// permission, nothing to click first. Only if that fails, or the permission
// is refused, does a notification offer it again. The .deb and .rpm do the
// same while installing, so this is for an AppImage or an older install.
// "Don't ask again" stops it; the commands stay refused until it works.
// Where bubblewrap can't run but the kernel's Landlock confines the commands
// instead (a lighter sandbox), they run, and the app sets bubblewrap up by
// itself once only, offering it again if that fails.
import { loadPrefs, savePrefs } from './localPrefs';
import { notify } from './notifications';
import { installSandbox, sandboxStatus, type SandboxStatus } from './sandbox';
import { onWindows } from './wsl';

const PREFS_KEY = 'rovik.sandbox';
const NOTE = 'sandbox-setup';

/** Stored in localStorage: this machine's choice. */
interface Prefs {
  /** No setup notification at start. */
  quiet?: boolean;
  /** The full sandbox's setup was tried once while the fallback worked. */
  triedFull?: boolean;
}

const notNow = { label: 'Not now', run: () => {} };
const dontAsk = {
  label: 'Don’t ask again',
  run: () => savePrefs<Prefs>(PREFS_KEY, { ...loadPrefs<Prefs>(PREFS_KEY), quiet: true }),
};

/**
 * What the app does at start: nothing where the sandbox works (or isn't
 * required, or the user said not to ask), sets it up by itself where it
 * can (the system asks for permission, nothing to click first), and
 * otherwise says what's missing. Where a fallback confines the commands,
 * the full sandbox is set up by itself once, and nothing is said after.
 */
export function startupStep(status: SandboxStatus, prefs: Prefs): 'none' | 'setUp' | 'tell' {
  if (!status.required || prefs.quiet) return 'none';
  if (status.fallback) return status.canInstall && !prefs.triedFull ? 'setUp' : 'none';
  if (status.working) return 'none';
  return status.canInstall ? 'setUp' : 'tell';
}

/**
 * Sets the sandbox up; a failure, or the permission refused, offers it again.
 * `fallback`: what confines the commands meanwhile, if anything.
 */
async function setUp(program: string, fallback: string | null) {
  notify({
    id: NOTE,
    title: `Setting up ${program} for the agent’s commands…`,
    message: 'The system asks for administrator permission.',
    progress: 'busy',
  });
  const again = {
    label: 'Try again',
    primary: true,
    run: () => void setUp(program, fallback),
  };
  try {
    const outcome = await installSandbox();
    if (outcome.status === 'installed') {
      notify({
        id: NOTE,
        kind: 'success',
        title: 'The agent’s commands are sandboxed',
        message: outcome.version ?? undefined,
        timeout: 6000,
      });
    } else {
      notify({
        id: NOTE,
        kind: 'warning',
        title: outcome.status === 'manual' ? 'Set it up yourself' : 'Restart needed',
        message:
          outcome.status === 'manual' && outcome.command
            ? `${outcome.message} ${outcome.command}`
            : outcome.message,
        actions: [notNow, dontAsk],
      });
    }
  } catch (err) {
    notify({
      id: NOTE,
      kind: 'error',
      title: `${program} couldn’t be set up`,
      message: fallback
        ? `${String(err)} Until it is, the agent’s commands run under ${fallback}, a lighter sandbox.`
        : `${String(err)} Until it is, the agent’s commands are refused.`,
      actions: [again, notNow, dontAsk],
    });
  }
}

let started = false;

/** Once per app run, in the background. */
export async function startSandbox() {
  // On Windows the sandbox is WSL's: wslStartup.ts sees to it.
  if (started || onWindows()) return;
  started = true;
  let status: SandboxStatus;
  try {
    status = await sandboxStatus();
  } catch {
    return; // not supported here
  }
  const prefs = loadPrefs<Prefs>(PREFS_KEY);
  switch (startupStep(status, prefs)) {
    case 'none':
      return;
    case 'setUp':
      if (status.fallback) savePrefs<Prefs>(PREFS_KEY, { ...prefs, triedFull: true });
      return setUp(status.program, status.fallback);
    case 'tell':
      notify({
        id: NOTE,
        kind: 'warning',
        title: `The agent’s commands need ${status.program}`,
        message: `${status.detail ? `${status.detail} ` : ''}The commands the agent runs are sandboxed with it, and refused without it.`,
        actions: [notNow, dontAsk],
      });
  }
}
