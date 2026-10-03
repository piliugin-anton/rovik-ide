import { Channel, invoke } from '@tauri-apps/api/core';

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

export async function writeToTerminal(id: string, data: string): Promise<void> {
  return invoke('write_to_terminal', { id, data });
}

export async function resizeTerminal(id: string, cols: number, rows: number): Promise<void> {
  return invoke('resize_terminal', { id, cols, rows });
}

/** Kills the shell; a no-op if it already exited. */
export async function closeTerminal(id: string): Promise<void> {
  return invoke('close_terminal', { id });
}
