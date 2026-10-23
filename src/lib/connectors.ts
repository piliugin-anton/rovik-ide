// connectors.ts
//
// Connectors: coding harnesses a chat can use in place of a model
// provider (Settings → Connectors). A connector is a program on the
// computer the chat runs on, Claude Code first: it brings its own model
// loop and tools, so the IDE hands it the system prompt, the model, the
// message and the project folder, follows what it does, and shows its
// steps, its tokens and its cost with the reply
// (src-tauri/crates/rovik-core/src/connectors).
//
// A connector is kept as a `Provider` entry in the AI settings whose
// `baseUrl` is `connector://<kind>` (its token or key is `apiKey`, so it
// goes to the keychain as a provider's key does), with the harness's
// settings in `settings`. So the model picker, a chat's saved model, the
// AI reviewer and the settings file work as they do for providers; the
// pages and the picker tell the two apart by `isConnector`.
import { invoke } from '@tauri-apps/api/core';
import type { ConnectorKind, ModelProfile, Provider, ProviderModel } from '../types';
import { plural } from './textUtils';
import type { RemoteModel } from './aiSettings';

export const CONNECTOR_SCHEME = 'connector://';

/** Claude Code's settings (claude_code.rs `Settings`, the same fields and defaults). */
export interface ClaudeCodeSettings {
  /** The program: `claude`, found on the PATH, or a path to it. */
  program: string;
  /** How it signs in: this computer's login, a `claude setup-token` token, or an API key. */
  auth: 'login' | 'token' | 'apiKey';
  permissionMode: 'acceptEdits' | 'bypassPermissions' | 'default' | 'plan' | 'dontAsk' | 'auto';
  /** `--allowedTools` rules, one per line. */
  allowedTools: string;
  disallowedTools: string;
  /** The most agentic turns one message may take; empty: no limit. */
  maxTurns?: number;
  /** The most one message may cost (estimated, USD); empty: no limit. */
  maxBudgetUsd?: number;
  /** The IDE's system prompt after Claude Code's own, or in its place. */
  systemPrompt: 'append' | 'replace';
  /** `CLAUDE_CONFIG_DIR`: another login and settings folder; empty: its own. */
  configDir: string;
  /** More arguments, as typed on a command line. */
  extraArgs: string;
  /** `--bare`: none of the user's hooks, skills, plugins or CLAUDE.md. */
  bare: boolean;
}

export const CLAUDE_CODE_DEFAULTS: ClaudeCodeSettings = {
  program: 'claude',
  auth: 'login',
  permissionMode: 'acceptEdits',
  allowedTools: '',
  disallowedTools: '',
  systemPrompt: 'append',
  configDir: '',
  extraArgs: '',
  bare: false,
};

export const PERMISSION_MODES: {
  value: ClaudeCodeSettings['permissionMode'];
  label: string;
  hint: string;
}[] = [
  {
    value: 'acceptEdits',
    label: 'Accept edits',
    hint: 'Edits files and runs common file commands without asking; other commands need an allowed-tools rule',
  },
  {
    value: 'bypassPermissions',
    label: 'Everything',
    hint: 'Runs every tool without asking, like the IDE’s own agent with approval off',
  },
  { value: 'auto', label: 'Auto', hint: 'A classifier reviews each action instead of a person' },
  {
    value: 'default',
    label: 'Manual',
    hint: 'Only the allowed-tools rules run; anything else is denied, since nobody is there to answer',
  },
  { value: 'plan', label: 'Plan', hint: 'Reads and plans, changes nothing' },
];

export const AUTH_KINDS: { value: ClaudeCodeSettings['auth']; label: string; hint: string }[] = [
  {
    value: 'login',
    label: 'This computer’s login',
    hint: 'The account `claude` is logged in to here (run `claude` once to log in). On a cloud host, the host’s login.',
  },
  {
    value: 'token',
    label: 'Token',
    hint: 'A long-lived token from `claude setup-token`: works unattended and on cloud hosts, kept in the keychain',
  },
  {
    value: 'apiKey',
    label: 'API key',
    hint: 'An Anthropic API key, billed to the Console account',
  },
];

/** What can be connected: each kind, for the "Add connector" menu. */
export const CONNECTOR_CATALOG: {
  kind: ConnectorKind;
  title: string;
  about: string;
  install: string;
}[] = [
  {
    kind: 'claude-code',
    title: 'Claude Code',
    about:
      'Anthropic’s coding agent, run headless in the project folder with its own tools. Chats that pick one of its models hand it the task; its steps, tokens and cost show with the reply.',
    install: 'curl -fsSL https://claude.ai/install.sh | bash',
  },
];

export function connectorTitle(kind: ConnectorKind): string {
  return CONNECTOR_CATALOG.find((c) => c.kind === kind)?.title ?? kind;
}

export function connectorBaseUrl(kind: ConnectorKind): string {
  return `${CONNECTOR_SCHEME}${kind}`;
}

/** The connector a base URL names, if it names one. */
export function connectorKindOf(baseUrl: string): ConnectorKind | undefined {
  const rest = baseUrl.trim();
  if (!rest.startsWith(CONNECTOR_SCHEME)) return undefined;
  const id = rest.slice(CONNECTOR_SCHEME.length).replace(/\/+$/, '');
  return CONNECTOR_CATALOG.some((c) => c.kind === id) ? (id as ConnectorKind) : undefined;
}

/** Whether the entry is a connector rather than a model provider. */
export function isConnector(p: Pick<Provider, 'baseUrl' | 'connector'>): boolean {
  return !!p.connector || connectorKindOf(p.baseUrl) !== undefined;
}

export function kindOfProvider(
  p: Pick<Provider, 'baseUrl' | 'connector'>
): ConnectorKind | undefined {
  return p.connector ?? connectorKindOf(p.baseUrl);
}

/** The entries that are model providers, and those that are connectors. */
export function splitSources(providers: Provider[]): {
  providers: Provider[];
  connectors: Provider[];
} {
  return {
    providers: providers.filter((p) => !isConnector(p)),
    connectors: providers.filter(isConnector),
  };
}

/** A new connector entry of `kind`, with the kind's defaults. */
export function newConnector(kind: ConnectorKind): Provider {
  return {
    id: crypto.randomUUID(),
    name: connectorTitle(kind),
    baseUrl: connectorBaseUrl(kind),
    apiKey: '',
    stream: true,
    models: [],
    connector: kind,
    settings: { ...CLAUDE_CODE_DEFAULTS },
  };
}

/** A connector's settings with the kind's defaults for what isn't set. */
export function claudeCodeSettings(p: Pick<Provider, 'settings'>): ClaudeCodeSettings {
  const s = (p.settings ?? {}) as Partial<ClaudeCodeSettings>;
  return {
    ...CLAUDE_CODE_DEFAULTS,
    ...s,
    program: typeof s.program === 'string' && s.program.trim() ? s.program : 'claude',
  };
}

/** What a probe (Test & refresh models) reports (rovik_protocol::connectors). */
export interface ConnectorProbe {
  program: string;
  version: string;
  models: {
    id: string;
    ownedBy?: string | null;
    contextLength?: number | null;
    description?: string | null;
  }[];
  request?: {
    model: string;
    inputTokens: number;
    outputTokens: number;
    costUsd?: number | null;
    ms: number;
  } | null;
  notes?: string[];
}

/**
 * Tests the connector on this computer, or on `host` (a cloud host's id),
 * and lists its models; with `request`, makes one small request too, to
 * see that it's signed in (a few tokens).
 */
export function probeConnector(
  p: Provider,
  request: boolean,
  host?: string | null
): Promise<ConnectorProbe> {
  const kind = kindOfProvider(p);
  if (!kind) return Promise.reject(new Error('Not a connector.'));
  return invoke<ConnectorProbe>('connector_probe', {
    args: { kind, settings: p.settings ?? {}, apiKey: p.apiKey, request },
    host: host ?? null,
  });
}

/** A probe's models as `mergeRemoteModels` takes them. */
export function probeModels(probe: ConnectorProbe): RemoteModel[] {
  return probe.models.map((m) => ({
    id: m.id,
    owned_by: m.ownedBy ?? null,
    context_length: m.contextLength ?? null,
  }));
}

/** "Connected: Claude Code 2.1.309 · answered with claude-sonnet-5 ($0.0006)" */
export function probeSummary(probe: ConnectorProbe): string {
  let text = `Connected: ${probe.version || probe.program}`;
  const r = probe.request;
  if (r) {
    const cost =
      r.costUsd != null
        ? ` · $${r.costUsd < 0.01 ? r.costUsd.toFixed(4) : r.costUsd.toFixed(2)}`
        : '';
    text += ` · answered${r.model ? ` with ${r.model}` : ''} in ${(r.ms / 1000).toFixed(1)} s${cost}`;
  }
  return text;
}

/** One line for the Settings menu: how many connectors and models there are. */
export function connectorsSummary(providers: Provider[]): string {
  const connectors = splitSources(providers).connectors;
  if (connectors.length === 0) return 'None yet: connect Claude Code to use it in place of a model';
  const models = connectors.reduce((n, c) => n + c.models.filter((m) => m.enabled).length, 0);
  return `${plural(connectors.length, 'connector')}, ${plural(models, 'model')} enabled`;
}

/** A connector's model, resolved for a request (aiSettings.ts resolveProfile). */
export function connectorProfile(
  provider: Provider,
  model: ProviderModel,
  kind: ConnectorKind
): ModelProfile {
  return {
    id: `${provider.id}::${model.id}`,
    label: `${provider.name || connectorTitle(kind)}/${model.id}`,
    model: model.id,
    baseUrl: connectorBaseUrl(kind),
    apiKey: provider.apiKey,
    stream: true,
    // Attachments stay with the IDE: the harness reads the project itself.
    vision: false,
    pdf: false,
    contextTokens: model.contextOverride ?? model.contextLength,
    // The harness runs its own tools: the IDE offers it none.
    noTools: true,
    firstByteTimeoutMs: model.firstTokenTimeoutSec ? model.firstTokenTimeoutSec * 1000 : undefined,
    reasoning: model.reasoning ?? { choice: 'auto' },
    connector: provider.settings ?? {},
  };
}

/*
 * What a connector's reply records (`meta.connector`, agent_run/run.rs
 * `harness_meta`): the harness's session, model and version, its steps,
 * and what it used.
 */

export interface HarnessStep {
  id: string;
  name: string;
  input: unknown;
  result?: string | null;
  ok?: boolean | null;
  /** A subagent's step: the call that started the subagent. */
  parent?: string | null;
}

export interface ConnectorUsage {
  prompt_tokens?: number;
  completion_tokens?: number;
  prompt_tokens_details?: { cached_tokens?: number };
  cost?: number;
  connector?: {
    kind?: string;
    numTurns?: number | null;
    durationMs?: number | null;
    model?: string | null;
    version?: string | null;
    subtype?: string;
    permissionDenials?: number;
  };
}

export interface ConnectorMeta {
  kind?: string;
  session?: string;
  model?: string | null;
  version?: string | null;
  steps?: HarnessStep[];
  usage?: ConnectorUsage;
}

/** "Read src/a.rs", "Bash npm test", "Edit foo.ts": a step in a few words. */
export function stepLabel(step: HarnessStep): string {
  const input = (step.input ?? {}) as Record<string, unknown>;
  const str = (k: string) => (typeof input[k] === 'string' ? (input[k] as string) : '');
  const what =
    str('file_path') ||
    str('path') ||
    str('command') ||
    str('pattern') ||
    str('query') ||
    str('url') ||
    str('description') ||
    str('prompt') ||
    str('skill') ||
    '';
  const short = what.length > 80 ? `${what.slice(0, 77)}…` : what;
  return short ? `${step.name} ${short}` : step.name;
}

function compact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** "Claude Code · claude-sonnet-5 · 7 steps · 12.3k tokens (8.1k cached) · $0.04 · 3 turns" */
export function connectorHeadline(meta: ConnectorMeta, running: boolean): string {
  const parts: string[] = [];
  const kind = meta.kind ? connectorTitle(meta.kind as ConnectorKind) : 'Connector';
  parts.push(kind);
  const model = meta.usage?.connector?.model ?? meta.model;
  if (model) parts.push(model);
  const steps = meta.steps?.length ?? 0;
  if (steps || running) parts.push(running && !steps ? 'working…' : plural(steps, 'step'));
  const u = meta.usage;
  if (u) {
    const tokens = (u.prompt_tokens ?? 0) + (u.completion_tokens ?? 0);
    const cached = u.prompt_tokens_details?.cached_tokens ?? 0;
    if (tokens)
      parts.push(`${compact(tokens)} tokens${cached ? ` (${compact(cached)} cached)` : ''}`);
    if (u.cost !== undefined)
      parts.push(`$${u.cost < 0.01 ? u.cost.toFixed(4) : u.cost.toFixed(2)}`);
    const turns = u.connector?.numTurns;
    if (turns && turns > 1) parts.push(plural(turns, 'turn'));
  }
  return parts.join(' · ');
}

/** What the reply used, as the subagents' totals count it (subagents.ts). */
export function connectorUsage(
  meta: ConnectorMeta | undefined
): { inputTokens: number; outputTokens: number; cost?: number } | null {
  const u = meta?.usage;
  if (!u) return null;
  const used = { inputTokens: u.prompt_tokens ?? 0, outputTokens: u.completion_tokens ?? 0 } as {
    inputTokens: number;
    outputTokens: number;
    cost?: number;
  };
  if (u.cost !== undefined) used.cost = u.cost;
  return used;
}
