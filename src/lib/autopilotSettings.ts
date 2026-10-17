// autopilotSettings.ts
//
// The Autopilot section of Settings → Subagents
// (docs/design/autopilot-plan.md, 3.5): how much a task spends on
// independence. Saved with the Subagents settings (lib/subagentSettings.ts,
// `autopilot`) and sent in `AgentConfig.autopilot`
// (rovik-protocol/src/autopilot.rs `AutopilotConfig`), which only chats in
// Autopilot read. The section's other settings (the budget, preparing the
// dependencies, the commands Autopilot may never run) come with the tasks
// that make them do something (U04, S03, U01); until then the backend's
// defaults stand for them.
import type { AutopilotEffort } from './autopilot';

export interface AutopilotSettings {
  effort: AutopilotEffort;
}

export const DEFAULT_AUTOPILOT_SETTINGS: AutopilotSettings = { effort: 'balanced' };

/** The Effort radios, in order. */
export const AUTOPILOT_EFFORTS: { id: AutopilotEffort; label: string; hint: string }[] = [
  {
    id: 'balanced',
    label: 'Balanced',
    hint: 'Each stage once, with the subagents below.',
  },
  {
    id: 'thorough',
    label: 'Thorough',
    hint: 'Several candidates, a second model for every judgment, an adversarial tester; takes longer and costs more.',
  },
];

/** Saved settings (or nothing, or garbage) as valid settings; defaults fill gaps. */
export function normalizeAutopilotSettings(raw: unknown): AutopilotSettings {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const effort = AUTOPILOT_EFFORTS.find((e) => e.id === r.effort)?.id;
  return { effort: effort ?? DEFAULT_AUTOPILOT_SETTINGS.effort };
}

/**
 * `AgentConfig.autopilot` as the backend takes it: what the page sets. The
 * fields left out take the backend's defaults (`AutopilotConfig`).
 */
export interface AutopilotConfig {
  effort: AutopilotEffort;
}

export function autopilotConfig(settings: AutopilotSettings): AutopilotConfig {
  return { effort: settings.effort };
}
