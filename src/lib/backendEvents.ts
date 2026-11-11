import {
  backendGeneration,
  backendRecovering,
  reportBackendFailure,
  onBackendRestored,
} from './backendSession';
import { invoke } from '@tauri-apps/api/core';
import { followEvents, type EventBatch, type EventScope } from './eventReplay';
import { desktopScope } from './desktopScope';
import { chatStore } from './chatStore';
import { handleBackendAgent, initAgentRuns, recoverAgentRuns, type AgentStatus } from './agentRuns';
import { handleBackendCloud, initCloud, recoverCloud, type CloudEvent } from './cloud';
import { dismiss } from './notifications';

type AgentEvent = Parameters<typeof handleBackendAgent>[0];
async function refresh(scope: EventScope, current: () => boolean): Promise<void> {
  const status = await invoke<AgentStatus>('desktop_event_status', { scope });
  await Promise.all([chatStore.recoverEvents(current), recoverCloud(current)]);
  if (current()) recoverAgentRuns(status);
}

/** Starts only once listeners are ready; the read channel can reconnect independently. */
export function startBackendEvents(): () => void {
  let stopped = false;
  let stop = () => {};
  let started = false;
  const start = () =>
    Promise.all([initAgentRuns(false), initCloud()]).then(() => {
      if (stopped || started) return;
      started = true;
      let generation = backendGeneration();
      const follower = followEvents({
        scope: desktopScope,
        version: backendGeneration,
        poll: async (scope, cursor) => {
          if (backendRecovering()) throw new Error('Restoring this window');
          if (!(await invoke<boolean>('desktop_session_usable')))
            throw new Error('Command session ended');
          const current = backendGeneration();
          const batch = await invoke<EventBatch>('desktop_poll_events', {
            scope,
            cursor: current === generation ? cursor : null,
          });
          if (current !== backendGeneration() || backendRecovering())
            throw new Error('Session changed');
          generation = current;
          return batch;
        },
        refresh,
        apply: (event) => {
          if (event.channel === 'agent') handleBackendAgent(event.payload as AgentEvent);
          else if (event.channel === 'cloud') handleBackendCloud(event.payload as CloudEvent);
          else throw new Error('Unknown backend event channel');
        },
        failed: reportBackendFailure,
        recovered: () => dismiss('backend-session'),
        pause: () => new Promise((resolve) => setTimeout(resolve, 1000)),
      });
      stop = follower.stop;
    });
  const remove = onBackendRestored(async () => {
    if (stopped) return;
    const scope = desktopScope();
    const version = backendGeneration();
    await refresh(
      scope,
      () =>
        !stopped &&
        version === backendGeneration() &&
        JSON.stringify(scope) === JSON.stringify(desktopScope())
    );
    if (!started && !stopped) await start();
  });
  void start().catch(reportBackendFailure);
  return () => {
    stopped = true;
    remove();
    stop();
  };
}
