// wsl.ts
//
// WSL on Windows (Settings → This computer → WSL): the WSL2 distribution
// Rovik's Linux side runs in. The daemon runs inside it with the Linux
// sandbox for the agent's commands (bubblewrap) and FUSE for the chats'
// overlays, and the app reaches it through wsl.exe. The commands are
// `wsl_*` (src-tauri/src/commands/wsl.rs, over rovik-client's
// hosts_page.rs); a test's steps come on `host-test` with the host id
// `wsl`, as a cloud host's do (lib/hosts.ts).
import { invoke } from '@tauri-apps/api/core';
import type { DeployFailure, HostState, Step } from './hosts';

export const WSL_HOST = 'wsl';

export interface WslStatus {
  /** wsl.exe answers here: Windows with WSL installed. */
  available: boolean;
  /** Why it doesn't, when it doesn't. */
  unavailable?: string;
  /** This Windows can't run WSL2 (too old): no install would help. */
  unsupported: boolean;
  /** The distributions installed here. */
  distros: string[];
  /** The distribution set up, when one is, and the user in it. */
  distro?: string;
  user?: string;
  /** Rovik runs in it for this computer. */
  deployed: boolean;
  version?: string;
  state?: HostState;
  /** Why the app couldn't connect to it, when it couldn't. */
  problem?: string;
  /** Rovik there is older than this app: the app updates it at its start. */
  outdated: boolean;
  appVersion: string;
}

export const wslStatus = () => invoke<WslStatus>('wsl_status');

/** Sets `distro` up as this computer's Linux side; resolves with the daemon's version. */
export const setUpWsl = (distro: string, user?: string) =>
  invoke<string>('wsl_setup', { distro, user: user || null });

export const testWsl = () => invoke<Step[]>('wsl_test');

export const updateWsl = () => invoke<void>('wsl_update');

export const removeWsl = () => invoke<void>('wsl_remove');

/** Whether this computer runs Windows, where WSL is this computer's Linux side. */
export const onWindows = () => navigator.platform.toUpperCase().includes('WIN');

/** One line for the Settings menu. */
export function wslSummary(status: WslStatus): { text: string; warning: boolean } {
  if (!status.available) {
    return {
      text: status.unsupported ? 'WSL2 can’t run on this Windows' : 'WSL isn’t installed',
      warning: true,
    };
  }
  if (!status.deployed) return { text: 'Not set up', warning: true };
  switch (status.state) {
    case 'ready':
      return {
        text: `${status.distro ?? 'WSL'}, connected${status.outdated ? ', updating' : ''}`,
        warning: false,
      };
    case 'offline':
    case 'refused':
    case 'version-mismatch':
      return { text: `${status.distro ?? 'WSL'}: not connected`, warning: true };
    default:
      return { text: `${status.distro ?? 'WSL'}, connecting…`, warning: false };
  }
}

/** A command's rejection as a deploy failure: the object Rust sends, or text. */
export function asFailure(err: unknown): DeployFailure {
  if (err && typeof err === 'object' && 'kind' in err && 'message' in err) {
    return err as DeployFailure;
  }
  return { kind: 'failed', message: String(err) };
}
