// virtualFiles.ts
//
// Virtual files (src-tauri/src/vfs): files held in the app's memory that
// every other program sees as real files, through the OS's user-space
// filesystem driver (FUSE on Linux, macFUSE on macOS, WinFsp on Windows).
// Write one, hand its `realPath` to a command, and the command reads it with
// ordinary system calls; what the command writes there the app can read back.
import { Channel, invoke } from '@tauri-apps/api/core';

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

export type VirtualFileEventKind =
  'created' | 'opened' | 'read' | 'written' | 'truncated' | 'closed' | 'removed' | 'renamed';

/** Something a program (`source: 'process'`) or the app did to a virtual file. */
export type VirtualFileEvent = {
  ino: number;
  /** Path inside the volume; for a deleted file, the path it last had. */
  path: string;
  source: 'process' | 'app';
} & (
  | { op: 'created'; kind: VirtualEntry['kind'] }
  | { op: 'opened'; write: boolean }
  | { op: 'read'; offset: number; len: number }
  | { op: 'written'; offset: number; len: number }
  | { op: 'truncated'; size: number }
  /** `modified`: written since the previous close — "a program finished writing it". */
  | { op: 'closed'; modified: boolean }
  | { op: 'removed'; kind: VirtualEntry['kind'] }
  | { op: 'renamed'; from: string }
);

/**
 * Calls `handler` with batches of events (about every 50 ms while files are
 * in use) until the returned function is called. By default everything but
 * the per-request `read`/`written` and `opened` events ("changes"); pass
 * kinds to choose. `dropped` counts events lost because the app fell behind.
 */
export async function subscribeVirtualFiles(
  handler: (events: VirtualFileEvent[], dropped: number) => void,
  kinds?: (VirtualFileEventKind | 'changes' | 'all')[]
): Promise<() => Promise<void>> {
  const channel = new Channel<{ events: VirtualFileEvent[]; dropped: number }>();
  channel.onmessage = (batch) => handler(batch.events, batch.dropped);
  const id = await invoke<number>('vfs_subscribe', { kinds: kinds ?? null, onEvents: channel });
  return async () => {
    await invoke<boolean>('vfs_unsubscribe', { id });
  };
}
