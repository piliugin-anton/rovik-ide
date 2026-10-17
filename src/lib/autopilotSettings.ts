// autopilotSettings.ts
//
// The Autopilot section of Settings → Subagents
// (docs/design/autopilot-plan.md, 3.5): how much a task spends on
// independence, and the commands its runs never run. Saved with the
// Subagents settings (lib/subagentSettings.ts, `autopilot`) and sent in
// `AgentConfig.autopilot` (rovik-protocol/src/autopilot.rs
// `AutopilotConfig`), which only chats in Autopilot read. The section's
// other settings (the budget, preparing the dependencies) come with the
// tasks that make them do something (U04, S03); until then the backend's
// defaults stand for them.
import type { AutopilotEffort } from './autopilot';
import type { SubagentKindId } from './subagentSettings';

export interface AutopilotSettings {
  effort: AutopilotEffort;
  /** The commands its runs never run (U01); unset: the defaults. */
  deniedCommands?: string[];
}

export const DEFAULT_AUTOPILOT_SETTINGS: AutopilotSettings = { effort: 'balanced' };

/**
 * The commands Autopilot never runs unless the settings change them:
 * rovik-protocol/src/autopilot.rs `DEFAULT_AUTOPILOT_DENYLIST`, which a
 * test keeps this the same as.
 */
export const DEFAULT_AUTOPILOT_DENYLIST = [
  'rm -rf /',
  'git push',
  'git commit --amend',
  'curl | sh',
  'curl | bash',
  'wget | sh',
  'wget | bash',
  'sudo',
  'shutdown',
  'dd',
];

/**
 * A denylist entry as it's kept: a program and the words it's run with,
 * single-spaced, or such entries joined by " | " (one piped into the
 * next). Empty when it says nothing, or a side of a pipe is missing.
 */
export function denylistEntry(text: string): string {
  const parts = text.split('|').map((part) => part.trim().split(/\s+/).filter(Boolean).join(' '));
  return parts.some((part) => !part) ? '' : parts.join(' | ');
}

/** The denylist the settings give: theirs, or the defaults. */
export function denylistOf(settings: AutopilotSettings): string[] {
  return settings.deniedCommands ?? DEFAULT_AUTOPILOT_DENYLIST;
}

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

/**
 * The kinds Autopilot turns on for its own chats, with the models their
 * rows give, whatever their switches and the master switch say (D17,
 * agent_run/autopilot.rs `needed_kinds`).
 */
export const AUTOPILOT_KINDS: SubagentKindId[] = [
  'planReviewer',
  'reviewer',
  'triage',
  'testWriter',
  'worker',
];

/** Saved settings (or nothing, or garbage) as valid settings; defaults fill gaps. */
export function normalizeAutopilotSettings(raw: unknown): AutopilotSettings {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const effort = AUTOPILOT_EFFORTS.find((e) => e.id === r.effort)?.id;
  const denied = Array.isArray(r.deniedCommands)
    ? [
        ...new Set(
          r.deniedCommands
            .filter((c): c is string => typeof c === 'string')
            .map(denylistEntry)
            .filter(Boolean)
        ),
      ]
    : undefined;
  return {
    effort: effort ?? DEFAULT_AUTOPILOT_SETTINGS.effort,
    ...(denied ? { deniedCommands: denied } : {}),
  };
}

/**
 * `AgentConfig.autopilot` as the backend takes it: what the page sets. The
 * fields left out take the backend's defaults (`AutopilotConfig`).
 */
export interface AutopilotConfig {
  effort: AutopilotEffort;
  deniedCommands?: string[];
}

export function autopilotConfig(settings: AutopilotSettings): AutopilotConfig {
  return {
    effort: settings.effort,
    ...(settings.deniedCommands ? { deniedCommands: settings.deniedCommands } : {}),
  };
}
