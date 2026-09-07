import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface TerminalDataEvent {
  id: string;
  data: string;
}

export interface TerminalExitEvent {
  id: string;
}

/**
 * Ask the Rust backend to spawn a shell (cmd.exe on Windows, $SHELL/bash
 * elsewhere) attached to a pseudo-terminal, and return the id used to
 * address it in subsequent calls/events.
 */
export async function spawnTerminal(cwd: string | null, cols = 80, rows = 24): Promise<string> {
  return invoke<string>('spawn_terminal', { cwd, cols, rows });
}

export async function writeToTerminal(id: string, data: string): Promise<void> {
  return invoke('write_to_terminal', { id, data });
}

export async function resizeTerminal(id: string, cols: number, rows: number): Promise<void> {
  return invoke('resize_terminal', { id, cols, rows });
}

export async function closeTerminal(id: string): Promise<void> {
  return invoke('close_terminal', { id });
}

/** Subscribe to output chunks for one terminal id. Returns the unlisten fn. */
export function onTerminalData(id: string, cb: (data: string) => void): Promise<UnlistenFn> {
  return listen<TerminalDataEvent>('terminal:data', (event) => {
    if (event.payload.id === id) cb(event.payload.data);
  });
}

/** Subscribe to the shell process exiting for one terminal id. */
export function onTerminalExit(id: string, cb: () => void): Promise<UnlistenFn> {
  return listen<TerminalExitEvent>('terminal:exit', (event) => {
    if (event.payload.id === id) cb();
  });
}
