import { Channel, invoke } from '@tauri-apps/api/core';
import type { HostSummary } from './cloud';

/** Streamed from the backend over each terminal's own channel (src-tauri/src/terminal.rs). */
export type TerminalEvent =
  | { type: 'output'; data: string }
  /** `code` is null when the exit status couldn't be read. */
  | { type: 'exit'; code: number | null };

export interface TerminalInfo {
  id: string;
  /** Shell program name, e.g. "bash", "zsh", "cmd". */
  shell: string;
}

/**
 * Spawns a shell ($SHELL on macOS/Linux, COMSPEC on Windows) on a new
 * pseudo-terminal. Output and exit arrive through `onEvent`, which is wired
 * up before the shell starts, so its first prompt is never missed.
 */
export async function spawnTerminal(
  cwd: string | null,
  cols: number,
  rows: number,
  onEvent: (event: TerminalEvent) => void
): Promise<TerminalInfo> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<TerminalInfo>('spawn_terminal', { cwd, cols, rows, onEvent: channel });
}

/**
 * Opens the user's login shell on cloud host `host` (its id), on a
 * pseudo-terminal there, reached as the host's settings say: Rovik's SSH,
 * on a connection of its own, or the user's own ssh. Its `shell` is the
 * shell's name there ("bash"), or "ssh" for the user's own. Output and exit
 * arrive through `onEvent`, as `spawnTerminal`'s do.
 */
export async function spawnRemoteTerminal(
  host: string,
  cols: number,
  rows: number,
  onEvent: (event: TerminalEvent) => void
): Promise<TerminalInfo> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<TerminalInfo>('spawn_remote_terminal', { host, cols, rows, onEvent: channel });
}

export async function writeToTerminal(id: string, data: string): Promise<void> {
  return invoke('write_to_terminal', { id, data });
}

export async function resizeTerminal(id: string, cols: number, rows: number): Promise<void> {
  return invoke('resize_terminal', { id, cols, rows });
}

/** Kills the shell, or hangs up on a host's; a no-op if it already exited. */
export async function closeTerminal(id: string): Promise<void> {
  return invoke('close_terminal', { id });
}

/** A cloud host a terminal runs on: its id, and its name. */
export interface TerminalHost {
  id: string;
  name: string;
}

/** The cloud hosts a terminal can open on now: those connected that SSH reaches. */
export function terminalHosts(hosts: HostSummary[]): HostSummary[] {
  return hosts.filter((h) => h.terminal && h.deployed && h.state === 'ready');
}

/** What the terminals call a cloud host: "Build (remote)". */
export function remoteLabel(name: string): string {
  return `${name} (remote)`;
}

/** A terminal's tab, as its label sees it. */
export interface LabeledTab {
  key: number;
  /** Its shell's name: "bash", or "shell" until it's known. */
  shell: string;
  /** The cloud host it runs on; null on this computer. */
  host: TerminalHost | null;
}

/**
 * A terminal tab's label, among `tabs`: its shell's name, numbered among
 * the tabs with the same shell in the same place ("bash 2"). Where it runs
 * follows when `placed` (a cloud host is connected, or a tab runs on one):
 * "bash (local)", or "bash (Build (remote))" on the host Build.
 */
export function terminalLabel(tab: LabeledTab, tabs: LabeledTab[], placed: boolean): string {
  const same = tabs.filter((t) => t.shell === tab.shell && t.host?.id === tab.host?.id);
  const index = same.findIndex((t) => t.key === tab.key);
  const title = same.length < 2 ? tab.shell : `${tab.shell} ${index + 1}`;
  if (!placed) return title;
  return `${title} (${tab.host ? remoteLabel(tab.host.name) : 'local'})`;
}
