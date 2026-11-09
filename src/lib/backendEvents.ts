import { invoke } from '@tauri-apps/api/core';
import { followEvents, type EventBatch } from './eventReplay';
import { desktopScope } from './desktopScope';
import { chatStore } from './chatStore';
import { handleBackendAgent, initAgentRuns, recoverAgentRuns, type AgentStatus } from './agentRuns';
import { handleBackendCloud, initCloud, recoverCloud, type CloudEvent } from './cloud';
import { dismiss, notify } from './notifications';

type AgentEvent = Parameters<typeof handleBackendAgent>[0];
/** Starts only once listeners are ready; the read channel can reconnect independently. */
export function startBackendEvents(): () => void {
  let stopped = false;
  let stop = () => {};
  void Promise.all([initAgentRuns(false), initCloud()])
    .then(() => {
      if (stopped) return;
      const follower = followEvents({
        scope: desktopScope,
        poll: (scope, cursor) => invoke<EventBatch>('desktop_poll_events', { scope, cursor }),
        refresh: async (scope, current) => {
          const status = await invoke<AgentStatus>('desktop_event_status', { scope });
          await Promise.all([chatStore.recoverEvents(current), recoverCloud(current)]);
          if (current()) recoverAgentRuns(status);
        },
        apply: (event) => {
          if (event.channel === 'agent') handleBackendAgent(event.payload as AgentEvent);
          else if (event.channel === 'cloud') handleBackendCloud(event.payload as CloudEvent);
          else throw new Error('Unknown backend event channel');
        },
        failed: (error) =>
          notify({
            id: 'backend-events',
            kind: 'error',
            title: 'Live updates interrupted',
            message: `Reconnecting to the backend. If commands are also unavailable, reopen this window. ${String(error)}`,
          }),
        recovered: () => dismiss('backend-events'),
        pause: () => new Promise((resolve) => setTimeout(resolve, 1000)),
      });
      stop = follower.stop;
    })
    .catch((error) => console.error('Backend event reader failed to start:', error));
  return () => {
    stopped = true;
    stop();
  };
}
