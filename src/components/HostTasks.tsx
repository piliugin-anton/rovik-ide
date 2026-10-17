// HostTasks.tsx
//
// A cloud host's page, connected: this computer's chats there, each with
// its latest Autopilot task (autopilot-plan H08), as `rovik-agentd status`
// lists them on the server: its stage and how it is, its outcome, what it
// spent, and whether a run of it goes now.
import { createResource, For, Show } from 'solid-js';
import { reportSpent } from '../lib/autopilot';
import { hostTasks, type ChatTask } from '../lib/hosts';
import { summaryParts } from './StageRecord';

/** A stage's name as the strip says it (AutopilotStrip). */
const STAGES: Record<string, string> = {
  plan: 'Plan',
  planReview: 'Plan review',
  prepare: 'Prepare',
  tests: 'Tests',
  implement: 'Implement',
  review: 'Review',
  checks: 'Checks',
  acceptance: 'Acceptance',
  deliver: 'Deliver',
};

/** Where a chat's task is, in a few words. */
export function taskLine(chat: ChatTask): string {
  const task = chat.task;
  if (!task) return chat.outcome ? `No task: its last run ${chat.outcome}` : 'No task';
  const stage = STAGES[task.stage] ?? task.stage;
  const where =
    task.stage === 'deliver' && task.status !== 'running'
      ? task.status === 'failed'
        ? 'Ended: not delivered'
        : 'Delivered'
      : `${stage}: ${task.status}`;
  const outcome = task.outcome ? ` (${task.outcome})` : '';
  return `${where}${outcome} · ${reportSpent(task)}`;
}

export default function HostTasks(props: { hostId: string }) {
  const [tasks, { refetch }] = createResource(
    () => props.hostId,
    (id) => hostTasks(id)
  );
  return (
    <section class="settings-section host-tasks">
      <div class="settings-section-title">Tasks on this server</div>
      <Show when={!tasks.error} fallback={<p class="settings-hint">{String(tasks.error)}</p>}>
        <Show
          when={(tasks() ?? []).length}
          fallback={
            <p class="settings-hint">
              {tasks.loading ? 'Asking the server…' : 'No chats of this computer’s there.'}
            </p>
          }
        >
          <ul class="host-tasks-list">
            <For each={tasks()}>
              {(chat) => (
                <li classList={{ running: !!chat.running }}>
                  <span class="host-task-title">{chat.title || chat.chat}</span>
                  <span class="host-task-line">{taskLine(chat)}</span>
                  <Show when={chat.running}>
                    <span class="host-task-running">Running</span>
                  </Show>
                  <Show when={chat.task?.summary}>
                    {(summary) => (
                      <span class="settings-hint host-task-summary">{summaryParts(summary())}</span>
                    )}
                  </Show>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
      <div class="host-actions">
        <button disabled={tasks.loading} onClick={() => void refetch()}>
          Refresh
        </button>
      </div>
    </section>
  );
}
