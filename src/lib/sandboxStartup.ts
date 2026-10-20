// sandboxStartup.ts
//
// The command sandbox at app start (lib/sandbox.ts), in the background. On
// Linux the agent's commands run under bubblewrap, on macOS under Seatbelt,
// and are refused without it, so when it's missing or doesn't work one
// notification says so, offering to install bubblewrap (the system asks for
// permission). "Don't ask again" stops the notification; the commands stay
// refused until it works.
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
}

const notNow = { label: 'Not now', run: () => {} };
const dontAsk = {
  label: 'Don’t ask again',
  run: () => savePrefs<Prefs>(PREFS_KEY, { quiet: true }),
};

async function install(program: string) {
  notify({
    id: NOTE,
    title: `Installing ${program}…`,
    message: 'The system will ask for permission.',
    progress: 'busy',
  });
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
        title: outcome.status === 'manual' ? 'Install it yourself' : 'Restart needed',
        message:
          outcome.status === 'manual' && outcome.command
            ? `${outcome.message} ${outcome.command}`
            : outcome.message,
        actions: [notNow],
      });
    }
  } catch (err) {
    notify({
      id: NOTE,
      kind: 'error',
      title: 'Installing failed',
      message: String(err),
      actions: [notNow],
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
  if (!status.required || status.working) return;
  if (loadPrefs<Prefs>(PREFS_KEY).quiet === true) return;
  const detail = status.detail ? `${status.detail} ` : '';
  notify({
    id: NOTE,
    kind: 'warning',
    title: `The agent’s commands need ${status.program}`,
    message: `${detail}The commands the agent runs are sandboxed with it, and refused without it.${status.canInstall ? ' Installing asks for administrator permission.' : ''}`,
    actions: status.canInstall
      ? [
          {
            label: `Install ${status.program}`,
            primary: true,
            run: () => void install(status.program),
          },
          notNow,
          dontAsk,
        ]
      : [notNow, dontAsk],
  });
}
