// subagentSettings.ts
//
// The Subagents settings (docs/design/subagents-plan.md, D14 and S08): a
// master switch, a default subagent model with fallbacks, and for each kind
// of subagent a switch, a model with fallbacks, a reasoning effort and a
// budget. Saved with the AI settings (lib/aiSettings.ts), off by default:
// with the master switch off, chats work exactly as without subagents.
//
// The backend gets them resolved (`resolveSubagentConfig`): for each kind,
// the models to try in order (its model, its fallbacks, the default model
// and its fallbacks, then the chat's model), enabled models only.
import type { ModelProfile, ModelSelection, OpenAIChatOptions, Provider } from '../types';
import { formatsToSend, type Effort } from './reasoning';
import { sameModel } from './permissions';
import {
  DEFAULT_AUTOPILOT_SETTINGS,
  normalizeAutopilotSettings,
  type AutopilotSettings,
} from './autopilotSettings';

/** The kinds built so far (agent_run/subagent.rs `Kind`). */
export type SubagentKindId =
  | 'scout'
  | 'reviewer'
  | 'triage'
  | 'planReviewer'
  | 'worker'
  | 'testWriter'
  | 'acceptance'
  | 'planner';

export const SUBAGENT_KINDS: {
  id: SubagentKindId;
  label: string;
  hint: string;
  /** When it's used. */
  when: string;
  /** Default budget (agent_run/subagent.rs). */
  toolCalls: number;
  minutes: number;
}[] = [
  {
    id: 'scout',
    label: 'Scout',
    hint: 'Explores the code and answers one question. Read-only.',
    when: 'The agent sends scouts in chats with a project folder, in Agent and Plan mode.',
    toolCalls: 20,
    minutes: 3,
  },
  {
    id: 'reviewer',
    label: 'Change reviewer',
    hint: 'Reviews the whole change with fresh eyes before the checks run. Read-only.',
    when: 'Runs when the agent finishes a change of at least the size set in Limits, at most 3 times a run.',
    toolCalls: 30,
    minutes: 5,
  },
  {
    id: 'triage',
    label: 'Failure triage',
    hint: 'Reads a large or repeated check failure and tells the agent its root cause. Read-only.',
    when: 'Runs when a check’s output is too long to send whole, or the same check failed in the round before.',
    toolCalls: 15,
    minutes: 3,
  },
  {
    id: 'planReviewer',
    label: 'Plan reviewer',
    hint: 'Checks a plan against the code before it’s saved. Read-only.',
    when: 'Runs in Plan mode when the agent submits a plan, at most twice a run; small revisions aren’t reviewed.',
    toolCalls: 25,
    minutes: 4,
  },
  {
    id: 'worker',
    label: 'Step worker',
    hint: 'Carries out one step of a plan, writing only that step’s files.',
    when: 'The agent hands a plan’s steps to workers one at a time, for plans of at least the size set in Limits.',
    toolCalls: 60,
    minutes: 15,
  },
  {
    id: 'testWriter',
    label: 'Test writer',
    hint: 'Writes tests from the request and the plan, without reading the changed code. Writes test files only.',
    when: 'The agent can call it after implementing a change; its tests run with the project’s checks.',
    toolCalls: 40,
    minutes: 10,
  },
  {
    id: 'acceptance',
    label: 'Acceptance checker',
    hint: 'Judges a finished change by its plan’s criteria, one verdict each, with evidence. Read-only.',
    when: 'Runs in Autopilot once a task’s checks pass; ordinary chats don’t use it.',
    toolCalls: 30,
    minutes: 5,
  },
  {
    id: 'planner',
    label: 'Planner',
    hint: 'Plans a task on its own, one of three; the plan that passes the plan’s checks is taken and reviewed. Read-only.',
    when: 'Runs in Autopilot’s Thorough effort, three at once before a task’s plan; ordinary chats don’t use it.',
    toolCalls: 40,
    minutes: 8,
  },
];

/** "default": the default subagent model; "chat": the chat's own model. */
export type KindModel = 'default' | 'chat' | ModelSelection;

export interface KindSettings {
  enabled: boolean;
  model: KindModel;
  fallbacks: ModelSelection[];
  effort: Effort;
  /** The budget; undefined: the kind's default. */
  maxToolCalls?: number;
  maxMinutes?: number;
}

export interface SubagentSettings {
  /** The master switch. */
  enabled: boolean;
  /** "chat": the chat's own model. */
  defaultModel: 'chat' | ModelSelection;
  defaultFallbacks: ModelSelection[];
  kinds: Record<SubagentKindId, KindSettings>;
  /** Subagents running at once in one chat. */
  maxAtOnce: number;
  /** Subagents one run may start. */
  maxPerRun: number;
  /** The change reviewer runs on changes of at least this many lines. */
  reviewMinLines: number;
  /** Plans of at least this many steps go to step workers. */
  workerMinSteps: number;
  /** Plan groups run as parallel workers (isolated chats only). */
  parallelWorkers: boolean;
  /** The test writer's tests also run on the code before the change. */
  redCheck: boolean;
  /** The repair loop fixes high-confidence causes in different workers' files at once. */
  parallelRepair: boolean;
  /** Tokens one run's subagents may use (as reported); undefined: no limit. */
  maxRunTokens?: number;
  /** The page's Autopilot section (lib/autopilotSettings.ts), sent apart. */
  autopilot: AutopilotSettings;
}

export const LIMITS = {
  atOnce: { min: 1, max: 8, default: 3 },
  perRun: { min: 1, max: 50, default: 12 },
  reviewMinLines: { min: 1, max: 10_000, default: 10 },
  workerMinSteps: { min: 1, max: 100, default: 6 },
  runTokens: { min: 1_000, max: 100_000_000 },
  toolCalls: { min: 1, max: 200 },
  minutes: { min: 0.25, max: 120 },
};

type Limit = { min: number; max: number };

/** The whole-number limits that always have a value: the field, its row's label and its range. */
export const LIMIT_FIELDS: {
  field: 'maxAtOnce' | 'maxPerRun' | 'reviewMinLines' | 'workerMinSteps';
  label: string;
  limit: Limit & { default: number };
}[] = [
  { field: 'maxAtOnce', label: 'At once per chat', limit: LIMITS.atOnce },
  { field: 'maxPerRun', label: 'Per run', limit: LIMITS.perRun },
  { field: 'reviewMinLines', label: 'Review changes from (lines)', limit: LIMITS.reviewMinLines },
  {
    field: 'workerMinSteps',
    label: 'Step workers from (plan steps)',
    limit: LIMITS.workerMinSteps,
  },
];

const DEFAULT_KIND: KindSettings = {
  enabled: false,
  model: 'default',
  fallbacks: [],
  effort: 'default',
};

export const DEFAULT_SUBAGENT_SETTINGS: SubagentSettings = {
  enabled: false,
  defaultModel: 'chat',
  defaultFallbacks: [],
  kinds: {
    scout: { ...DEFAULT_KIND },
    reviewer: { ...DEFAULT_KIND },
    triage: { ...DEFAULT_KIND },
    planReviewer: { ...DEFAULT_KIND },
    worker: { ...DEFAULT_KIND },
    testWriter: { ...DEFAULT_KIND },
    acceptance: { ...DEFAULT_KIND },
    planner: { ...DEFAULT_KIND },
  },
  maxAtOnce: LIMITS.atOnce.default,
  maxPerRun: LIMITS.perRun.default,
  reviewMinLines: LIMITS.reviewMinLines.default,
  workerMinSteps: LIMITS.workerMinSteps.default,
  parallelWorkers: false,
  redCheck: false,
  parallelRepair: false,
  autopilot: DEFAULT_AUTOPILOT_SETTINGS,
};

const EFFORT_IDS: Effort[] = ['default', 'none', 'low', 'medium', 'high'];

function isSelection(v: unknown): v is ModelSelection {
  const s = v as ModelSelection | null;
  return !!s && typeof s.providerId === 'string' && typeof s.modelId === 'string';
}

/** A selection's own fields, whatever else was saved with it. */
const pickSelection = (s: ModelSelection): ModelSelection => ({
  providerId: s.providerId,
  modelId: s.modelId,
});

function selections(v: unknown): ModelSelection[] {
  if (!Array.isArray(v)) return [];
  const out: ModelSelection[] = [];
  for (const s of v.filter(isSelection)) {
    if (!out.some((o) => sameModel(o, s))) out.push(pickSelection(s));
  }
  return out;
}

/** `v` within `limit`; undefined unless it's a finite number. */
function inRange(v: unknown, limit: Limit): number | undefined {
  return typeof v === 'number' && Number.isFinite(v)
    ? Math.min(limit.max, Math.max(limit.min, v))
    : undefined;
}

/** `v` within `limit`, whole; undefined unless it's a finite number. */
function wholeInRange(v: unknown, limit: Limit): number | undefined {
  const n = inRange(v, limit);
  return n === undefined ? undefined : Math.round(n);
}

function normalizeKind(raw: unknown): KindSettings {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const model: KindModel =
    r.model === 'chat' || r.model === 'default'
      ? r.model
      : isSelection(r.model)
        ? pickSelection(r.model)
        : 'default';
  const toolCalls = wholeInRange(r.maxToolCalls, LIMITS.toolCalls);
  const minutes = inRange(r.maxMinutes, LIMITS.minutes);
  return {
    enabled: r.enabled === true,
    model,
    fallbacks: selections(r.fallbacks).filter(
      (f) => typeof model === 'string' || !sameModel(f, model)
    ),
    effort: EFFORT_IDS.includes(r.effort as Effort) ? (r.effort as Effort) : 'default',
    ...(toolCalls !== undefined ? { maxToolCalls: toolCalls } : {}),
    ...(minutes !== undefined ? { maxMinutes: minutes } : {}),
  };
}

/** Saved settings (or nothing, or garbage) as valid settings; defaults fill gaps. */
export function normalizeSubagentSettings(raw: unknown): SubagentSettings {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const kinds = (r.kinds && typeof r.kinds === 'object' ? r.kinds : {}) as Record<string, unknown>;
  const defaultModel = isSelection(r.defaultModel) ? pickSelection(r.defaultModel) : 'chat';
  const maxRunTokens = wholeInRange(r.maxRunTokens, LIMITS.runTokens);
  return {
    enabled: r.enabled === true,
    defaultModel,
    defaultFallbacks: selections(r.defaultFallbacks).filter(
      (f) => defaultModel === 'chat' || !sameModel(f, defaultModel)
    ),
    kinds: Object.fromEntries(
      SUBAGENT_KINDS.map((k) => [k.id, normalizeKind(kinds[k.id])])
    ) as Record<SubagentKindId, KindSettings>,
    ...(Object.fromEntries(
      LIMIT_FIELDS.map(({ field, limit }) => [
        field,
        wholeInRange(r[field], limit) ?? limit.default,
      ])
    ) as Record<(typeof LIMIT_FIELDS)[number]['field'], number>),
    parallelWorkers: r.parallelWorkers === true,
    redCheck: r.redCheck === true,
    parallelRepair: r.parallelRepair === true,
    ...(maxRunTokens !== undefined ? { maxRunTokens } : {}),
    autopilot: normalizeAutopilotSettings(r.autopilot),
  };
}

/*
 * ---------------------------------------------------------------------------
 * Resolution for the backend (AgentConfig.subagents)
 * ---------------------------------------------------------------------------
 */

/** A model in a kind's list, as the backend takes it (subagent.rs `ModelChoice`). */
export type ModelChoice = ModelProfile | { chat: true };

export interface KindConfig {
  enabled: boolean;
  models: ModelChoice[];
  options?: OpenAIChatOptions;
  maxToolCalls?: number;
  maxMinutes?: number;
}

export interface SubagentConfig {
  enabled: boolean;
  kinds: Partial<Record<SubagentKindId, KindConfig>>;
  maxAtOnce: number;
  maxPerRun: number;
  reviewMinLines: number;
  workerMinSteps: number;
  parallelWorkers: boolean;
  redCheck: boolean;
  parallelRepair: boolean;
  maxRunTokens?: number;
}

const CHAT = 'chat' as const;
type ChainEntry = typeof CHAT | ModelSelection;

const sameEntry = (a: ChainEntry, b: ChainEntry) =>
  a === CHAT || b === CHAT ? a === b : sameModel(a, b);

/**
 * The models a kind tries, in order, before they're resolved: its model
 * and fallbacks (set to Default: the default model, its own fallbacks,
 * then the default model's), then the default model and its fallbacks,
 * then the chat's model. Each once.
 */
export function kindChain(settings: SubagentSettings, kind: SubagentKindId): ChainEntry[] {
  const k = settings.kinds[kind];
  const defaults: ChainEntry[] = [settings.defaultModel, ...settings.defaultFallbacks];
  // P5.2: set to Default, the default model, then the kind's own fallbacks
  // before the default model's.
  const own: ChainEntry[] =
    k.model === 'default'
      ? [settings.defaultModel, ...k.fallbacks, ...settings.defaultFallbacks]
      : [k.model, ...k.fallbacks];
  const out: ChainEntry[] = [];
  for (const entry of [...own, ...defaults, CHAT]) {
    if (!out.some((o) => sameEntry(o, entry))) out.push(entry);
  }
  return out;
}

/** An enabled model that can call tools: one a kind can use. */
export function usableModel(providers: Provider[], sel: ModelSelection): boolean {
  const model = providers
    .find((p) => p.id === sel.providerId)
    ?.models.find((m) => m.id === sel.modelId);
  return !!model && model.enabled && !model.noTools;
}

/**
 * The settings as the backend takes them. `resolve` turns a selection into
 * a model (lib/aiSettings.ts `resolveProfile`); models turned off, removed
 * or marked "no tools" are left out, so the next one is tried instead.
 */
export function resolveSubagentConfig(
  settings: SubagentSettings,
  providers: Provider[],
  resolve: (providers: Provider[], sel: ModelSelection) => ModelProfile | null
): SubagentConfig {
  const kinds: Partial<Record<SubagentKindId, KindConfig>> = {};
  for (const { id } of SUBAGENT_KINDS) {
    const k = settings.kinds[id];
    const models: ModelChoice[] = [];
    for (const entry of kindChain(settings, id)) {
      if (entry === CHAT) {
        models.push({ chat: true });
        continue;
      }
      if (!usableModel(providers, entry)) continue;
      const profile = resolve(providers, entry);
      if (profile) models.push(profile);
    }
    const first = models[0];
    let options: OpenAIChatOptions | undefined;
    // The kind's effort, in the forms its first model takes; the chat's
    // model with "Default" follows the chat's effort (the backend's rule).
    if (k.effort !== 'default' && first && !('chat' in first)) {
      const formats = formatsToSend(first.reasoning);
      if (formats.length) options = { reasoning_effort: k.effort, reasoning_formats: formats };
    } else if (k.effort !== 'default') {
      options = { reasoning_effort: k.effort };
    }
    kinds[id] = {
      enabled: k.enabled,
      models,
      ...(options ? { options } : {}),
      ...(k.maxToolCalls !== undefined ? { maxToolCalls: k.maxToolCalls } : {}),
      ...(k.maxMinutes !== undefined ? { maxMinutes: k.maxMinutes } : {}),
    };
  }
  return {
    enabled: settings.enabled,
    kinds,
    maxAtOnce: settings.maxAtOnce,
    maxPerRun: settings.maxPerRun,
    reviewMinLines: settings.reviewMinLines,
    workerMinSteps: settings.workerMinSteps,
    parallelWorkers: settings.parallelWorkers,
    redCheck: settings.redCheck,
    parallelRepair: settings.parallelRepair,
    ...(settings.maxRunTokens !== undefined ? { maxRunTokens: settings.maxRunTokens } : {}),
  };
}

/** The provider a selection's model is from (the K18 warning). */
export function providerOf(providers: Provider[], sel: ModelSelection): Provider | undefined {
  return providers.find((p) => p.id === sel.providerId);
}

/**
 * Other providers than the chat's that a kind's models send code to (K18):
 * their names, for "Code will also be sent to …".
 */
export function otherProviders(
  providers: Provider[],
  settings: SubagentSettings,
  kind: SubagentKindId,
  chat: ModelSelection | null
): string[] {
  const names: string[] = [];
  for (const entry of kindChain(settings, kind)) {
    if (entry === CHAT || !usableModel(providers, entry)) continue;
    if (chat && entry.providerId === chat.providerId) continue;
    const provider = providerOf(providers, entry);
    const name = provider?.name || provider?.baseUrl;
    if (name && !names.includes(name)) names.push(name);
  }
  return names;
}

/**
 * What a number field's text saves as: unset when empty or not a number,
 * else within `min`..`max`, and whole when the field's step is.
 */
export function numberFieldValue(
  raw: string,
  min: number,
  max: number,
  step = 1
): number | undefined {
  const text = raw.trim();
  const n = Number(text);
  if (text === '' || !Number.isFinite(n)) return undefined;
  const whole = Number.isInteger(step) ? Math.round(n) : n;
  return Math.min(max, Math.max(min, whole));
}

/** One line for the Settings menu. */
export function subagentSummary(settings: SubagentSettings): string {
  if (!settings.enabled) return 'Off: the agent works alone';
  const on = SUBAGENT_KINDS.filter((k) => settings.kinds[k.id].enabled).map((k) => k.label);
  return on.length ? `On: ${on.join(', ')}` : 'On, but no kind is turned on';
}
