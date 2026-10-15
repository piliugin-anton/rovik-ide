// runPause.ts
//
// The line a paused run's record shows (`meta.paused`). A run pauses after a
// number of tool calls, or because it looked stuck; and with nothing wrong,
// for the chat's move to or from a host, or for an upgrade of the server's
// Rovik, which aren't shown as a stuck agent.
import type { RunPause } from '../types';

export function pauseText(pause: RunPause): string {
  switch (pause.kind) {
    case 'move':
      return pause.host ? `Moved to ${pause.host}.` : 'Moved.';
    case 'drain':
      return 'Paused for an upgrade.';
    case 'loop':
      return pause.reason
        ? `Paused after ${pause.toolCalls} tool calls: the agent seems stuck, ${pause.reason}.`
        : `Paused after ${pause.toolCalls} tool calls: the agent seems stuck.`;
    default:
      // A checkpoint, or a kind from a newer app.
      return `Paused after ${pause.toolCalls} tool calls.`;
  }
}
