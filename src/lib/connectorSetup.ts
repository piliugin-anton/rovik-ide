// connectorSetup.ts
//
// A connector's "Set up" (Settings → Connectors): its program installed if
// it's missing, tested, and signed in if it isn't, on this computer or on
// the cloud host its chats run on. The flow is the same for every
// connector; what each step runs is the connector's own, in the core
// (`Connector::setup_command`), which runs it as a job this page follows
// by its id (`connector_setup`): its output, the sign-in page it prints,
// the code the user pastes back.
import { invoke } from '@tauri-apps/api/core';
import type { Provider } from '../types';
import {
  connectorArgs,
  errorText,
  isMissingProgram,
  type ConnectorArgs,
  type ConnectorProbe,
} from './connectors';

export type SetupAction = 'install' | 'login' | 'logout' | 'update';

/** A step, as the computer running it reports it (rovik_protocol `SetupJob`). */
export interface SetupJob {
  id: string;
  action: SetupAction;
  /** What it runs, as typed: `curl -fsSL … | bash`, `claude auth login`. */
  command: string;
  output: string;
  /** The first web address it printed: the sign-in page. */
  url?: string | null;
  /** That page asks for a code the step printed (in `output`): nothing is pasted back. */
  deviceCode?: boolean;
  done: boolean;
  error?: string | null;
}

/** What `connector_setup` does with a step (rovik_protocol `SetupOp`). */
export type SetupOp =
  | { op: 'start'; args: ConnectorArgs; action: SetupAction }
  | { op: 'poll'; id: string }
  | { op: 'send'; id: string; text: string }
  | { op: 'cancel'; id: string };

/** One call on a step, here or on `host`. */
export type SetupCall = (op: SetupOp) => Promise<SetupJob>;

export function setupCall(host?: string | null): SetupCall {
  return (op) => invoke<SetupJob>('connector_setup', { op, host: host ?? null });
}

/** How often a running step is asked how it stands. */
const POLL_MS = 500;

/**
 * Runs `action` for the connector `p` to its end, reporting the step each
 * time it's asked; an error when it failed. `signal` stops it.
 */
export async function runSetupStep(
  call: SetupCall,
  p: Provider,
  action: SetupAction,
  onJob: (job: SetupJob) => void,
  signal?: AbortSignal,
  pollMs = POLL_MS
): Promise<SetupJob> {
  let job = await call({ op: 'start', args: connectorArgs(p), action });
  const stop = () => void call({ op: 'cancel', id: job.id }).catch(() => {});
  signal?.addEventListener('abort', stop, { once: true });
  try {
    onJob(job);
    while (!job.done) {
      await new Promise((resolve) => setTimeout(resolve, pollMs));
      job = await call({ op: 'poll', id: job.id });
      onJob(job);
    }
  } finally {
    signal?.removeEventListener('abort', stop);
  }
  if (job.error) throw new Error(job.error);
  return job;
}

/** What "Set up" needs from the page: the same for every connector. */
export interface SetupFlow {
  /** "Test & refresh models", as the page does it. */
  probe: () => Promise<ConnectorProbe>;
  /** Runs one step to its end (runSetupStep); throws when it failed. */
  step: (action: SetupAction) => Promise<SetupJob>;
  /** Asks before installing; false leaves it uninstalled. */
  confirmInstall: () => Promise<boolean>;
}

/** Thrown when the user declines to install: nothing failed. */
export class SetupDeclined extends Error {}

/**
 * Set up: tests the connector; installs its program if it's missing (once
 * the user agrees) and tests again; signs it in if it isn't and tests
 * again. The last test's result, which the page shows as a test's.
 */
export async function setUpConnector(flow: SetupFlow): Promise<ConnectorProbe> {
  let probe: ConnectorProbe;
  try {
    probe = await flow.probe();
  } catch (err) {
    if (!isMissingProgram(errorText(err))) throw err;
    if (!(await flow.confirmInstall())) throw new SetupDeclined('Not installed.');
    await flow.step('install');
    probe = await flow.probe();
  }
  if (probe.auth && !probe.auth.loggedIn) {
    await flow.step('login');
    probe = await flow.probe();
  }
  return probe;
}

/** For the user: what a step does while it runs. */
export function stepTitle(action: SetupAction): string {
  switch (action) {
    case 'install':
      return 'Installing';
    case 'login':
      return 'Signing in';
    case 'logout':
      return 'Signing out';
    case 'update':
      return 'Updating';
  }
}
