// virtualFiles.ts
//
// Virtual files (src-tauri/src/vfs): files held in the app's memory that
// every other program sees as real files, through the OS's user-space
// filesystem driver (FUSE on Linux, macFUSE on macOS, WinFsp on Windows).
// Write one, hand its `realPath` to a command, and the command reads it with
// ordinary system calls; what the command writes there the app can read back.
import { invoke } from '@tauri-apps/api/core';

export interface DriverStatus {
  /** "FUSE", "macFUSE" or "WinFsp". */
  driver: string;
  installed: boolean;
  version: string | null;
  /** What's missing, for the user. */
  detail: string | null;
  canInstall: boolean;
}

export interface VfsStatus {
  driver: DriverStatus;
  /** Where the volume is mounted, if it is. */
  mountpoint: string | null;
  defaultMountpoint: string;
  capacity: number;
  used: number;
}

export type InstallOutcome =
  | { status: 'installed'; version: string | null }
  | { status: 'needsRestart'; message: string }
  | { status: 'needsApproval'; message: string }
  | { status: 'manual'; message: string; command: string | null };

export interface VirtualFile {
  /** Path inside the volume. */
  path: string;
  /** Path programs open it by. */
  realPath: string;
  size: number;
}

export interface VirtualEntry {
  name: string;
  kind: 'file' | 'dir' | 'symlink';
  size: number;
}

export function vfsStatus(): Promise<VfsStatus> {
  return invoke<VfsStatus>('vfs_status');
}

/** Installs the driver; the OS asks for administrator consent. */
export function installVfsDriver(): Promise<InstallOutcome> {
  return invoke<InstallOutcome>('vfs_install_driver');
}

/** Mounts the volume (at the default place unless given) and returns where. */
export function mountVfs(mountpoint?: string): Promise<string> {
  return invoke<string>('vfs_mount', { mountpoint: mountpoint ?? null });
}

export function unmountVfs(): Promise<void> {
  return invoke<void>('vfs_unmount');
}

/**
 * Creates or replaces a virtual file (mounting the volume if needed).
 * `content` is text, or base64 with `{ base64: true }`.
 */
export function writeVirtualFile(
  path: string,
  content: string,
  options: { base64?: boolean; executable?: boolean } = {}
): Promise<VirtualFile> {
  return invoke<VirtualFile>('vfs_write_file', {
    path,
    content,
    encoding: options.base64 ? 'base64' : null,
    executable: options.executable ?? false,
  });
}

export function readVirtualFile(path: string): Promise<string> {
  return invoke<string>('vfs_read_file', { path });
}

export function listVirtualFiles(path = ''): Promise<VirtualEntry[]> {
  return invoke<VirtualEntry[]>('vfs_list', { path });
}

export function removeVirtualFile(path: string): Promise<void> {
  return invoke<void>('vfs_remove', { path });
}
