// sandbox.ts
//
// The sandbox the agent's commands run in
// (src-tauri/crates/rovik-core/src/overlay/confine.rs): bubblewrap on Linux,
// which the app offers to install at start (lib/sandboxStartup.ts), and
// Seatbelt on macOS, part of the system; required on both. Windows has none:
// its chats' commands run in WSL.
import { invoke } from '@tauri-apps/api/core';
import type { InstallOutcome } from './virtualFiles';

export interface SandboxStatus {
  /** Commands are refused without it (Linux, macOS). */
  required: boolean;
  /** What makes it: "bubblewrap" on Linux, "Seatbelt" on macOS; "" where there's none. */
  program: string;
  /** It's there and confines commands. */
  working: boolean;
  /** What's missing or wrong, for the user. */
  detail: string | null;
  /** The app can install it (otherwise `detail` says how). */
  canInstall: boolean;
}

export const sandboxStatus = () => invoke<SandboxStatus>('sandbox_status');

/** Installs the sandbox's program; the system asks for permission. */
export const installSandbox = () => invoke<InstallOutcome>('sandbox_install');
