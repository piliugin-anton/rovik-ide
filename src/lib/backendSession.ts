import { onWindows } from './wsl';
import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';
import { sessionRecovery } from './sessionRecovery';
import { notify, dismiss } from './notifications';

const [generation, setGeneration] = createSignal(0);
const [busy, setBusy] = createSignal(false);
/** Changes only when an old session is deliberately abandoned. */
export const backendGeneration = generation;
export const backendRecovering = busy;
const lost = new Set<() => void>();
const restored = new Set<() => void | Promise<void>>();
let context = (): { root: string | null; projectId: string | null } => ({
  root: null,
  projectId: null,
});

/** The mounted editor supplies its current folder without disposing its models or drafts. */
export function backendProject(read: typeof context): () => void {
  context = read;
  return () => {
    if (context === read) context = () => ({ root: null, projectId: null });
  };
}
/** Invalidates handles such as terminals and LSP processes; never restarts jobs. */
export function onBackendLost(callback: () => void): () => void {
  lost.add(callback);
  return () => {
    lost.delete(callback);
  };
}
/** Restores declarative subscriptions and read state after project registration succeeds. */
export function onBackendRestored(callback: () => void | Promise<void>): () => void {
  restored.add(callback);
  return () => {
    restored.delete(callback);
  };
}
/** Reconnects this window explicitly, coalescing concurrent requests. */
export const reconnectBackend = sessionRecovery({
  context: () => {
    if (typeof navigator !== 'undefined' && onWindows())
      throw new Error('Reopen this window to restart its embedded backend');
    const project = context();
    if (!!project.root !== !!project.projectId)
      throw new Error('Wait for the project to finish opening before reconnecting');
    return project;
  },
  open: (project) => invoke<void>('desktop_reconnect', project),
  lost: () => {
    setGeneration((value) => value + 1);
    for (const callback of [...lost]) {
      try {
        callback();
      } catch (error) {
        console.error('Could not invalidate an old backend handle:', error);
      }
    }
  },
  restore: async () => {
    const results = await Promise.allSettled([...restored].map(async (callback) => callback()));
    for (const result of results) if (result.status === 'rejected') throw result.reason;
  },
  busy: setBusy,
});

/** Offers an explicit reconnect; uncertain commands and terminal jobs are never replayed. */
export function reportBackendFailure(error: unknown): void {
  if (busy()) return;
  if (typeof navigator !== 'undefined' && onWindows()) {
    notify({
      id: 'backend-session',
      kind: 'error',
      title: 'Backend connection interrupted',
      message: `Reopen this window to restart its embedded backend. ${String(error)}`,
    });
    return;
  }
  notify({
    id: 'backend-session',
    kind: 'error',
    title: 'Backend connection interrupted',
    message: `Reconnect to restore this window. Unsaved editor text and chat drafts stay open. Check the result of interrupted actions before retrying them. ${String(error)}`,
    actions: [
      {
        label: 'Reconnect',
        primary: true,
        keepOpen: true,
        run: () => {
          notify({ id: 'backend-session', title: 'Reconnecting…', progress: 'busy' });
          void reconnectBackend()
            .then(() => dismiss('backend-session'))
            .catch(reportBackendFailure);
        },
      },
    ],
  });
}
