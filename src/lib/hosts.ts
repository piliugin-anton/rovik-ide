// Settings → Cloud hosts (U01): the hosts this computer knows, each one's
// connection test, its deploy, upgrade and removal, and what its daemon
// says of itself, its keys and its storage. The commands are `hosts_*`
// (src-tauri/src/commands/hosts.rs, over rovik-client's hosts_page.rs).
// A connection test's steps come on `host-test` as each one ends.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export type AuthKind = 'key' | 'agent' | 'password' | 'keyboard-interactive' | 'system-ssh';

/**
 * Who approves on the host while nobody is there (6.9); `autonomous`: it
 * works on its own, its chats in Autopilot (autopilot-plan 3.5, H01).
 */
export type ApprovalPolicy = 'waitForMe' | 'aiReviewerThenMe' | 'autonomous';

/** Where a host that works on its own sends a task's result (D15). */
export type Delivery =
  | { kind: 'keepOnHost' }
  /** `deployKey`: its path on the server, never its contents. */
  | { kind: 'pushBranch'; deployKey: string; openPr: boolean };

/**
 * What a host's tasks follow while it works on its own
 * (rovik-protocol/src/autopilot.rs `AutonomySettings`): unset, as in this
 * computer's settings.
 */
export interface AutonomySettings {
  effort?: 'balanced' | 'thorough';
  budget?: { maxHours: number | null; maxCost: number | null };
  delivery: Delivery;
  /** Called when a task ends or is blocked; may carry a token. */
  notifyUrl?: string;
}

export const DEFAULT_AUTONOMY: AutonomySettings = { delivery: { kind: 'keepOnHost' } };

/** A host's block, its defaults where it has none. */
export const autonomyOf = (host: Host): AutonomySettings => host.autonomy ?? DEFAULT_AUTONOMY;

export type DeployState =
  { state: 'notDeployed' } | { state: 'deployed'; version: string; sha256?: string };

/** A host's connection, once deployed. */
export type HostState = 'connecting' | 'ready' | 'offline' | 'version-mismatch';

export interface Host {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  auth: AuthKind;
  keyPath?: string;
  /** The host key pinned on first use, as OpenSSH writes it. */
  hostKey?: string;
  approvalPolicy: ApprovalPolicy;
  /** While it works on its own (`autonomous`); kept under another policy. */
  autonomy?: AutonomySettings;
  deploy: DeployState;
  /** Run on the host after a deploy, and when asked (design §7). */
  setupScript?: string;
}

export interface HostRow extends Host {
  state?: HostState;
  /** Why the app couldn't connect to it, when it couldn't: no token saved here, say. */
  problem?: string;
  /** The user's chats there, or moving there. */
  chats: number;
  /** Whether a password or a key passphrase is saved: never the secret. */
  passwordSaved: boolean;
  passphraseSaved: boolean;
}

/** Where secrets are kept (design §6). */
export type StoreKind = { kind: 'keychain' } | { kind: 'file'; unlocked: boolean };

export interface HostsList {
  hosts: HostRow[];
  store: StoreKind;
  /**
   * What a deploy uploads: the build `ROVIK_AGENTD_BINARY` names, or this
   * app's release; absent when there's neither.
   */
  build?: string;
}

/** What a form changes of a host's secrets: absent leaves one, empty forgets it. */
export interface HostSecrets {
  password?: string;
  passphrase?: string;
}

export type Check =
  | 'address'
  | 'host-key'
  | 'log-in'
  | 'system'
  | 'account'
  | 'disk'
  | 'git'
  | 'chrome'
  | 'isolation'
  | 'service'
  | 'install'
  | 'build'
  | 'daemon';

export type Outcome = 'passed' | 'warning' | 'failed' | 'confirm' | 'skipped';

/** A step of the connection test (H04). */
export interface Step {
  check: Check;
  outcome: Outcome;
  message: string;
  /** The host's key, when it's to be confirmed. */
  hostKey?: { key: string; fingerprint: string };
  /** Rovik's build on the server, when it isn't the one trusted. */
  build?: { sha256: string; version?: string };
}

export interface HostInfo {
  version: string;
  protocol: number;
  minProtocol: number;
  os: string;
  arch: string;
  /** The account the daemon runs as. */
  user: string;
  virtualFiles: boolean;
  virtualFilesProblem?: string;
  chrome?: string;
  userNamespaces: boolean;
  install?: { admins: string[] };
  otherDaemons: number;
}

/** A provider whose key the host keeps: never the key. */
export interface KeyInfo {
  provider: string;
  baseUrl: string;
}

/** What Rovik's folders take on the host, in bytes. */
export interface Storage {
  projects: number;
  work: number;
  data: number;
  tmp: number;
}

export interface HostDetails {
  info: HostInfo;
  /** Absent from a daemon that can't say. */
  keys?: KeyInfo[];
  storage?: Storage;
  /** The user is one of Rovik's administrators there. */
  admin: boolean;
  /** The daemon is older than this app. */
  upgrade: boolean;
  appVersion: string;
  /** The daemon cleans up when asked ("Clean up now"). */
  cleanUp: boolean;
}

/** Why a deploy, an upgrade or a removal didn't happen. */
export type DeployFailure =
  | { kind: 'rootLogin'; message: string }
  | { kind: 'unsupported'; message: string }
  | { kind: 'sudoPassword'; message: string }
  | { kind: 'needsRoot'; command: string; message: string }
  | { kind: 'tampered'; message: string }
  | { kind: 'askAdmins'; admins: string[]; message: string }
  | { kind: 'failed'; message: string };

/** What removing Rovik for this user deletes: each folder and its size. */
export interface Removal {
  folders: [string, number][];
}

/** What the host's setup script printed, and how it ended. */
export interface SetupRun {
  /** Its exit status; null if the server didn't say. */
  status: number | null;
  output: string;
}

export const listHosts = () => invoke<HostsList>('hosts_list');

export const saveHost = (host: Host, secrets?: HostSecrets) =>
  invoke<Host>('hosts_save', { host, secrets: secrets ?? null });

export const removeHost = (id: string) => invoke<void>('hosts_remove', { id });

/** Tests the host's connection; each step comes on `host-test` as it ends. */
export const testHost = (id: string) => invoke<Step[]>('hosts_test', { id });

export const trustKey = (id: string, key: string) => invoke<void>('hosts_trust_key', { id, key });

/** Trusts the build of Rovik the server runs now, once the user compared it. */
export const trustBuild = (id: string, sha256: string) =>
  invoke<void>('hosts_trust_build', { id, sha256 });

export const setPolicy = (id: string, policy: ApprovalPolicy) =>
  invoke<void>('hosts_set_policy', { id, policy });

/** Saves a host's block, checked: resolves with it as kept. */
export const setAutonomy = (id: string, autonomy: AutonomySettings) =>
  invoke<AutonomySettings>('hosts_set_autonomy', { id, autonomy });

/**
 * What "Test delivery" found (H05; rovik-protocol `DeliveryTest`): the
 * branch the host pushed to the open project's remote and deleted, or why
 * not; and whether pull requests would open there, when asked for.
 */
export interface DeliveryTest {
  remote: string;
  branch: string;
  problem?: string;
  pullRequests?: string;
}

export const testDelivery = (id: string) => invoke<DeliveryTest>('hosts_test_delivery', { id });

/** Deploys Rovik; `sudoPassword` when sudo asked for one. Resolves with its version. */
export const deployHost = (id: string, sudoPassword?: string) =>
  invoke<string>('hosts_deploy', { id, sudoPassword: sudoPassword ?? null });

export const upgradeHost = (id: string) => invoke<void>('hosts_upgrade', { id });

/** Runs the host's setup script. */
export const runSetup = (id: string) => invoke<SetupRun>('hosts_run_setup', { id });

/** Opens the file that keeps secrets where there's no keychain. */
export const unlockSecrets = (passphrase: string) => invoke<void>('hosts_unlock', { passphrase });

export const removalOf = (id: string) => invoke<Removal>('hosts_removal', { id });

export const removeRovik = (id: string, removal: Removal) =>
  invoke<void>('hosts_remove_rovik', { id, removal });

export const hostDetails = (id: string) => invoke<HostDetails>('hosts_details', { id });

/** "Remove keys from this server": resolves with how many went. */
export const removeKeys = (id: string) => invoke<number>('hosts_remove_keys', { id });

/** "Clean up now": resolves with what Rovik's folders take on the host after. */
export const cleanUpHost = (id: string) => invoke<Storage>('hosts_clean_up', { id });

export const hostLimits = (id: string) => invoke<[string, string][]>('hosts_limits', { id });

export const setHostLimits = (id: string, changes: [string, string][]) =>
  invoke<void>('hosts_set_limits', { id, changes });

/**
 * Calls `handler` with each step of a connection test as it ends, until the
 * function it resolves with is called. Resolves once it listens: a test
 * started before that would have its first steps missed.
 */
export async function onHostTest(
  handler: (hostId: string, step: Step) => void
): Promise<() => void> {
  const stop = await listen<{ hostId: string; step: Step }>('host-test', (e) =>
    handler(e.payload.hostId, e.payload.step)
  );
  return stop;
}

/** A new host's form. */
export function emptyHost(): Host {
  return {
    id: '',
    name: '',
    host: '',
    port: 22,
    user: '',
    auth: 'key',
    approvalPolicy: 'waitForMe',
    deploy: { state: 'notDeployed' },
  };
}

/** The host of a row, without what the list adds to it. */
export function hostOf(row: HostRow): Host {
  const { state, problem, chats, passwordSaved, passphraseSaved, ...host } = row;
  return host;
}

/**
 * What a host's form saves of its secrets: what's typed in, nothing for a
 * field left empty, and the forgetting of a saved one the way in no longer
 * takes.
 */
export function secretsToSave(
  auth: AuthKind,
  typed: { password: string; passphrase: string },
  saved: { passwordSaved: boolean; passphraseSaved: boolean } | undefined
): HostSecrets {
  const usesPassword = auth === 'password' || auth === 'keyboard-interactive';
  const usesKey = auth === 'key';
  const pick = (uses: boolean, value: string, kept: boolean | undefined) => {
    if (uses) return value || undefined;
    return kept ? '' : undefined;
  };
  return {
    password: pick(usesPassword, typed.password, saved?.passwordSaved),
    passphrase: pick(usesKey, typed.passphrase, saved?.passphraseSaved),
  };
}

export const AUTH_LABELS: Record<AuthKind, string> = {
  key: 'A key file',
  agent: 'My SSH agent',
  password: 'A password',
  'keyboard-interactive': 'A password the server asks for (keyboard-interactive)',
  'system-ssh': 'My system ssh',
};

/** What each way in takes, under the form's "Log in with". */
export const AUTH_HINTS: Record<AuthKind, string> = {
  key: 'A private key on this computer whose public key is in ~/.ssh/authorized_keys on the server.',
  agent:
    'A key loaded in your SSH agent (ssh-add), or on Windows in the OpenSSH agent or Pageant. The key never leaves the agent.',
  password:
    'Your account’s password on the server. If the server refuses the right password, it may only take one when it asks for it: choose “A password the server asks for”.',
  'keyboard-interactive':
    'For servers that take a password only at their own prompts. Rovik answers each prompt with the password below, so a server that also asks for a two-factor code won’t let you in.',
  'system-ssh':
    'Your own ssh connects, with your configuration: jump hosts, security keys and certificates. Testing and deploying need one of the other ways in, which Rovik runs its setup commands through.',
};

export const CHECK_LABELS: Record<Check, string> = {
  address: 'Address',
  'host-key': 'Host key',
  'log-in': 'Log-in',
  system: 'System',
  account: 'Account',
  disk: 'Disk',
  git: 'git',
  chrome: 'Chrome',
  isolation: 'Isolation extras',
  service: 'Service',
  install: 'Rovik',
  build: 'Build',
  daemon: 'Daemon',
};

export type Tone = 'muted' | 'ok' | 'warn' | 'error';

/** What the page says of a host: deployed or not, and its connection. */
export function hostStatus(row: Pick<HostRow, 'deploy' | 'state'>): { label: string; tone: Tone } {
  if (row.deploy.state === 'notDeployed') return { label: 'Not deployed', tone: 'muted' };
  switch (row.state) {
    case 'ready':
      return { label: 'Connected', tone: 'ok' };
    case 'offline':
      return { label: 'Offline', tone: 'error' };
    case 'version-mismatch':
      return { label: 'Needs a newer app', tone: 'error' };
    default:
      return { label: 'Connecting…', tone: 'warn' };
  }
}

/** The connection test as a whole, from its steps so far. */
export function testVerdict(steps: Step[], running: boolean): { label: string; tone: Tone } | null {
  if (running) return { label: 'Testing…', tone: 'muted' };
  if (steps.length === 0) return null;
  const confirm = steps.find((s) => s.outcome === 'confirm');
  if (confirm) {
    const what = confirm.check === 'build' ? 'Rovik’s build there' : 'the server’s key';
    return { label: `Check ${what}`, tone: 'warn' };
  }
  const failed = steps.find((s) => s.outcome === 'failed');
  if (failed) return { label: `Failed: ${CHECK_LABELS[failed.check]}`, tone: 'error' };
  if (steps.some((s) => s.outcome === 'warning'))
    return { label: 'Works, with warnings', tone: 'warn' };
  return { label: 'Everything works', tone: 'ok' };
}

/** How a setup script ended, in words. */
export function setupVerdict(run: SetupRun): { label: string; tone: Tone } {
  if (run.status === 0) return { label: 'The setup script worked.', tone: 'ok' };
  if (run.status === null) {
    return { label: 'The setup script ended, but the server didn’t say how.', tone: 'warn' };
  }
  return { label: `The setup script failed, with exit status ${run.status}.`, tone: 'error' };
}

/** A command's rejection as a deploy failure: the object Rust sends, or text. */
export function asDeployFailure(err: unknown): DeployFailure {
  if (err && typeof err === 'object' && 'kind' in err && 'message' in err) {
    return err as DeployFailure;
  }
  return { kind: 'failed', message: String(err) };
}

/** `user@host`, and the port when it isn't 22. */
export function addressOf(host: Pick<Host, 'user' | 'host' | 'port'>): string {
  const port = host.port === 22 ? '' : `:${host.port}`;
  return `${host.user}@${host.host}${port}`;
}
