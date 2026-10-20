// virtualFiles.ts
//
// Virtual files (src-tauri/src/vfs): files held in the app's memory that
// every other program sees as real files, through the OS's user-space
// filesystem driver (FUSE on Linux, macFUSE on macOS). Windows has none: the
// agent works in WSL, whose daemon serves them.
// Write one, hand its `realPath` to a command, and the command reads it with
// ordinary system calls; what the command writes there the app can read back.
import { Channel, invoke } from '@tauri-apps/api/core';

export interface DriverStatus {
  /** "FUSE" or "macFUSE"; "WSL" on Windows, which serves them. */
  driver: string;
  installed: boolean;
  version: string | null;
  /** What's missing, for the user. */
  detail: string | null;
  canInstall: boolean;
}

/** A System Settings page the app can open (macOS). */
export type SettingsPane = 'extensions' | 'privacySecurity';

/** Why the volume couldn't be mounted, and what to do about it. */
export interface MountProblem {
  kind: 'notInstalled' | 'enableExtension' | 'approveKernelExtension' | 'updateDriver' | 'failed';
  title: string;
  /** What to do, in order. */
  steps: string[];
  /** The page `openVfsSettings` opens for it. */
  settings: SettingsPane | null;
  /** The fix happens outside the app: keep trying to mount. */
  retry: boolean;
  /** The underlying error. */
  detail: string | null;
}

export interface VfsStatus {
  driver: DriverStatus;
  /** Where the volume is mounted, if it is. */
  mountpoint: string | null;
  defaultMountpoint: string;
  /** Why the last mount failed (until one succeeds). */
  problem: MountProblem | null;
  capacity: number;
  used: number;
  history: { revisions: number; bytes: number; limits: HistoryLimits };
}

/** How much history is kept. `maxBytes: 0` turns versioning off. */
export interface HistoryLimits {
  /** All revisions together. */
  maxBytes: number;
  /** Revisions kept per file (the oldest go first). */
  perFile: number;
  /** Larger files aren't versioned. */
  maxFileSize: number;
}

/**
 * A saved state of a file: taken when a program closes a file it modified,
 * or when the app writes one.
 */
export interface Revision {
  /** 1, 2, 3… per file (older ones may have been dropped). */
  number: number;
  size: number;
  /** Milliseconds since the Unix epoch. */
  time: number;
  source: 'process' | 'app';
}

/**
 * One line for the Settings menu: whether the volume is there, and what
 * that means for chats (isolated chats and CLM mode need it).
 */
export function vfsSummary(status: VfsStatus): { text: string; warning: boolean } {
  if (status.mountpoint) {
    return {
      text: `Mounted at ${status.mountpoint}: chats work in isolation`,
      warning: false,
    };
  }
  const without = 'chats work in the project folder directly';
  if (!status.driver.installed) {
    return { text: `${status.driver.driver} isn’t installed: ${without}`, warning: true };
  }
  if (status.problem) return { text: `${status.problem.title}: ${without}`, warning: true };
  return { text: `Mounts at ${status.defaultMountpoint} when first used`, warning: false };
}

export type InstallOutcome =
  | { status: 'installed'; version: string | null }
  | { status: 'needsRestart'; message: string }
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

/** Opens the System Settings page a mount problem points to (macOS). */
export function openVfsSettings(pane: SettingsPane): Promise<void> {
  return invoke<void>('vfs_open_settings', { pane });
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

/**
 * A file's revisions, oldest first. History follows the file through
 * renames and "safe saves"; a deleted file's history is found by the path
 * it had.
 */
export function listRevisions(path: string): Promise<Revision[]> {
  return invoke<Revision[]>('vfs_revisions', { path });
}

/** A revision's content: text, or base64 with `{ base64: true }`. */
export function readRevision(
  path: string,
  revision: number,
  options: { base64?: boolean } = {}
): Promise<string> {
  return invoke<string>('vfs_read_revision', {
    path,
    revision,
    encoding: options.base64 ? 'base64' : null,
  });
}

/** Makes a revision the file's content again (recreating a deleted file). */
export function restoreRevision(path: string, revision: number): Promise<VirtualFile> {
  return invoke<VirtualFile>('vfs_restore_revision', { path, revision });
}

export function setHistoryLimits(limits: HistoryLimits): Promise<void> {
  return invoke<void>('vfs_set_history_limits', { limits });
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
