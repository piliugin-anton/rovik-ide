// agentLog.ts
//
// Diagnostics for tuning the agent: one JSON line per model reply, per
// verification round and per finished run, appended to agent-log.jsonl in
// the app's data folder (rotated at 5 MB; src-tauri/src/project_profile.rs).
// Linux: ~/.local/share/rovik-ide/agent-log.jsonl
//
// Never throws and never blocks the agent: a failed write is dropped.
import { invoke } from '@tauri-apps/api/core';

export type AgentLogEvent =
  | {
      event: 'reply';
      dialogId: string;
      model: string;
      /** "stop", "tool_calls", "length"... as the provider sent it. */
      finishReason: string | null;
      toolCalls: number;
      textChars: number;
      usage: Record<string, unknown> | null;
      ms: number;
      /** Failed requests: the error kind. */
      error?: string;
    }
  | {
      event: 'verify';
      dialogId: string;
      round: number;
      status: string;
      checks: {
        kind: string;
        command: string;
        status: string;
        exitCode?: number | null;
        ms?: number;
      }[];
    }
  | {
      event: 'run';
      dialogId: string;
      model: string;
      /** How it ended: done, verified, verify-gave-up, paused-loop, stopped, error... */
      outcome: string;
      toolCalls: number;
      filesChanged: number;
      verifyRounds: number;
      ms: number;
    };

export function logAgent(entry: AgentLogEvent): void {
  const line = JSON.stringify({ t: new Date().toISOString(), ...entry });
  console.debug('[agent]', line);
  void invoke('append_agent_log', { line }).catch(() => {});
}
