// autopilotSettings.ts
//
// The Autopilot section of Settings → Subagents
// (docs/design/autopilot-plan.md, 3.5): how much a task spends on
// independence, the commands its runs never run, and the budget a task
// may spend. Saved with the Subagents settings (lib/subagentSettings.ts,
// `autopilot`) and sent in `AgentConfig.autopilot`
// (rovik-protocol/src/autopilot.rs `AutopilotConfig`), which only chats in
// Autopilot read. And whether a task installs its dependencies before the
// first check (S03).
import type { AutopilotEffort } from './autopilot';
import type { SubagentKindId } from './subagentSettings';

/** Where a task stops on its own (U04): null, no limit. */
export interface AutopilotBudget {
  maxHours: number | null;
  /** In the provider's currency. */
  maxCost: number | null;
  /**
   * In tokens, as the providers report a request used them (most do, even
   * those that report no cost; the page shows millions). Unset: no limit.
   */
  maxTokens?: number | null;
}

export interface AutopilotSettings {
  effort: AutopilotEffort;
  /** The commands its runs never run (U01); unset: the defaults. */
  deniedCommands?: string[];
  /** Where a task stops on its own (U04); unset: its effort's defaults. */
  budget?: AutopilotBudget;
  /** false: no Prepare stage, and no install after an environment failure (S03); unset: on. */
  prepare?: false;
}

/**
 * A task's budget until the settings set one (3.5): Balanced's is 2 hours
 * and 5; Thorough's 6 hours, a limit every provider allows to keep (the
 * time counts whether or not it reports a cost).
 */
export const DEFAULT_BUDGETS: Record<AutopilotEffort, AutopilotBudget> = {
  balanced: { maxHours: 2, maxCost: 5 },
  thorough: { maxHours: 6, maxCost: null },
};

/** The budget the settings give: theirs, or their effort's. */
export function budgetOf(settings: AutopilotSettings): AutopilotBudget {
  return settings.budget ?? DEFAULT_BUDGETS[settings.effort];
}

/**
 * `budget` with `change` made, as it's kept: a token limit only when there
 * is one. Undefined: the result is `defaults`, which the settings then
 * leave unset.
 */
export function changedBudget(
  budget: AutopilotBudget,
  change: Partial<AutopilotBudget>,
  defaults: AutopilotBudget
): AutopilotBudget | undefined {
  const { maxTokens, ...limits } = { ...budget, ...change };
  const next: AutopilotBudget = { ...limits, ...(maxTokens ? { maxTokens } : {}) };
  const same =
    next.maxHours === defaults.maxHours &&
    next.maxCost === defaults.maxCost &&
    (next.maxTokens ?? null) === (defaults.maxTokens ?? null);
  return same ? undefined : next;
}

/** A budget limit as kept: a positive, finite number, else no limit. */
function budgetLimit(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : null;
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
 * agent_run/autopilot/config.rs `needed_kinds`).
 */
export const AUTOPILOT_KINDS: SubagentKindId[] = [
  'planReviewer',
  'reviewer',
  'triage',
  'testWriter',
  'worker',
  'acceptance',
  'planner',
  'judge',
  'breaker',
];

/** Of those, the kinds only Thorough uses (`needed_kinds(Effort::Thorough)`). */
export const THOROUGH_KINDS: SubagentKindId[] = ['planner', 'judge', 'breaker'];

/** The tag a kind's row on the Subagents page carries, if Autopilot uses it. */
export function autopilotKindTag(kind: SubagentKindId): string | null {
  if (THOROUGH_KINDS.includes(kind)) return 'Used by Autopilot (Thorough)';
  return AUTOPILOT_KINDS.includes(kind) ? 'Used by Autopilot' : null;
}

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
  const b = r.budget && typeof r.budget === 'object' ? (r.budget as Record<string, unknown>) : null;
  const tokens = b ? budgetLimit(b.maxTokens) : null;
  const budget = b
    ? {
        maxHours: budgetLimit(b.maxHours),
        maxCost: budgetLimit(b.maxCost),
        ...(tokens ? { maxTokens: Math.round(tokens) } : {}),
      }
    : null;
  return {
    effort: effort ?? DEFAULT_AUTOPILOT_SETTINGS.effort,
    ...(denied ? { deniedCommands: denied } : {}),
    ...(budget ? { budget } : {}),
    ...(r.prepare === false ? { prepare: false as const } : {}),
  };
}

/**
 * `AgentConfig.autopilot` as the backend takes it: what the page sets. The
 * fields left out take the backend's defaults (`AutopilotConfig`).
 */
export interface AutopilotConfig {
  effort: AutopilotEffort;
  deniedCommands?: string[];
  budget: AutopilotBudget;
  prepare: boolean;
}

export function autopilotConfig(settings: AutopilotSettings): AutopilotConfig {
  return {
    effort: settings.effort,
    ...(settings.deniedCommands ? { deniedCommands: settings.deniedCommands } : {}),
    budget: budgetOf(settings),
    prepare: settings.prepare !== false,
  };
}
