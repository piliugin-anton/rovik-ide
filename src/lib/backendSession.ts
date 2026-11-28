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
let prepareProject = async () => {};

/** Supplies the current folder and resolves it after an update blocked initial project loading. */
export function backendProject(
  read: typeof context,
  prepare: () => Promise<void> = async () => {}
): () => void {
  context = read;
  prepareProject = prepare;
  return () => {
    if (context === read) {
      context = () => ({ root: null, projectId: null });
      prepareProject = async () => {};
    }
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
const recoverBackend = sessionRecovery({
  context: () => {
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

/** Reconnects this window explicitly, coalescing concurrent requests. */
export const reconnectBackend = () => recoverBackend();

/** The exact backend identity reviewed before requesting its replacement. */
export interface BackendRestartTarget {
  expectedBoot: string;
  expectedVersion: string;
  expectedProtocol: number;
  expectedBuild: string;
}

const replacements = new Map<string, Promise<void>>();

function recoverUpdatedBackend(open: (project: ReturnType<typeof context>) => Promise<void>) {
  return recoverBackend(
    async (project) => {
      // An incompatible backend may have prevented the startup folder from
      // resolving. Bootstrap without a project, then resolve it on the new
      // backend before ordinary attachment and subscription restoration.
      const registered =
        !!project.root === !!project.projectId ? project : { root: null, projectId: null };
      await open(registered);
      await prepareProject();
    },
    () => context()
  );
}

/** Finishes an update whose native replacement succeeded before project restoration failed. */
export function reconnectUpdatedBackend(): Promise<void> {
  return recoverUpdatedBackend((project) => invoke<void>('desktop_reconnect', project));
}

/** Replaces the reviewed backend once, then restores this window's declarative state. */
export function restartBackend(target: BackendRestartTarget): Promise<void> {
  const key = JSON.stringify(target);
  const pending = replacements.get(key);
  if (pending) return pending;
  const replacing = recoverUpdatedBackend((project) =>
    invoke<void>('desktop_restart_backend', { ...target, ...project })
  ).finally(() => replacements.delete(key));
  replacements.set(key, replacing);
  return replacing;
}

/** Offers an explicit reconnect; uncertain commands and terminal jobs are never replayed. */
export function reportBackendFailure(error: unknown): void {
  if (busy()) return;
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
