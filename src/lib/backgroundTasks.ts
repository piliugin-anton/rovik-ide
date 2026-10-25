// backgroundTasks.ts
//
// The commands a chat's agent left running (bash's `background`: a dev
// server, a watcher), by chat, for the row under its title row
// (OpenedPorts.tsx). The backend tells them as they start and end (the
// `backgroundTasks` agent event, lib/agentRuns.ts); a chat asks for them as
// it opens (`background_tasks`), as a window that opens later or a chat on a
// host that was away missed what was told; and its row stops one
// (`background_stop`), the agent hearing so. Where the chat runs: here, or
// on its host.
import { invoke } from '@tauri-apps/api/core';
import { createStore, produce, reconcile } from 'solid-js/store';

/** A command a chat's agent left running. */
export interface BackgroundTask {
  /** The agent's name for it: bg1, bg2… */
  id: string;
  command: string;
  /** When it started, in ms since the epoch. */
  startedAt: number;
}

const [state, setState] = createStore<{ tasks: Record<string, BackgroundTask[]> }>({
  tasks: {},
});

/** The chat's tasks as last told, oldest first; none without a chat. */
export function backgroundTasksOf(dialogId: string | null): BackgroundTask[] {
  return dialogId ? (state.tasks[dialogId] ?? []) : [];
}

/** The chat's tasks now, as the backend tells them; none leaves it out. */
export function noteBackgroundTasks(dialogId: string, tasks: BackgroundTask[]): void {
  if (tasks.length > 0) {
    setState('tasks', dialogId, reconcile(tasks, { key: 'id' }));
  } else {
    setState(
      'tasks',
      produce((all) => {
        delete all[dialogId];
      })
    );
  }
}

/** Asks where the chat runs what it has running now. */
export async function loadBackgroundTasks(dialogId: string): Promise<void> {
  try {
    noteBackgroundTasks(dialogId, await invoke<BackgroundTask[]>('background_tasks', { dialogId }));
  } catch (err) {
    // A host from before background tasks, or out of reach: none shown.
    console.error('Background tasks unavailable:', err);
  }
}

/**
 * Stops the chat's task `id`, with everything it started: whether it was
 * running. What the chat has running then is asked again, its event aside:
 * the row is right once this resolves.
 */
export async function stopBackgroundTask(dialogId: string, id: string): Promise<boolean> {
  const stopped = await invoke<boolean>('background_stop', { dialogId, id });
  await loadBackgroundTasks(dialogId);
  return stopped;
}
