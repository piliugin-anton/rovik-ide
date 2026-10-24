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
import { createSignal } from 'solid-js';
import type { ConnectorKind, ModelProfile, Provider, ProviderModel } from '../types';
import { plural } from './textUtils';
import type { RemoteModel } from './aiSettings';
import { notify } from './notifications';

export const CONNECTOR_SCHEME = 'connector://';

/** Claude Code's settings (claude_code.rs `Settings`, the same fields and defaults). */
export type ClaudeCodeSettings = {
  /** The program: `claude`, found on the PATH, or a path to it. */
  program: string;
  /** How it signs in: this computer's login, a `claude setup-token` token, or an API key. */
  auth: 'login' | 'token' | 'apiKey';
  /**
   * `rovik`: Rovik's permissions and approvals decide each call Claude Code
   * would ask about (claude_code.rs `ROVIK_PERMISSIONS`); the others are
   * Claude Code's own modes.
   */
  permissionMode:
    'rovik' | 'acceptEdits' | 'bypassPermissions' | 'default' | 'plan' | 'dontAsk' | 'auto';
  /** `--allowedTools` rules, one per line. */
  allowedTools: string;
  disallowedTools: string;
  /** The most agentic turns one message may take; empty: no limit. */
  maxTurns?: number;
  /** The most one message may cost (estimated, USD); empty: no limit. */
  maxBudgetUsd?: number;
  /** The IDE's system prompt after Claude Code's own, or in its place. */
  systemPrompt: 'own' | 'append' | 'replace';
  /** `CLAUDE_CONFIG_DIR`: another login and settings folder; empty: its own. */
  configDir: string;
  /** More arguments, as typed on a command line. */
  extraArgs: string;
  /** `--bare`: none of the user's hooks, skills, plugins or CLAUDE.md. */
  bare: boolean;
  /**
   * Whose tools a run uses: Claude Code's own, under its permission mode,
   * or Rovik's over MCP, where Claude Code is the model behind Rovik's
   * agent loop and Rovik runs every call under its own permissions.
   */
  tools: 'own' | 'rovik';
};

export const CLAUDE_CODE_DEFAULTS: ClaudeCodeSettings = {
  program: 'claude',
  auth: 'login',
  permissionMode: 'rovik',
  allowedTools: '',
  disallowedTools: '',
  systemPrompt: 'own',
  configDir: '',
  extraArgs: '',
  bare: false,
  tools: 'own',
};

export const TOOLS_MODES: { value: ClaudeCodeSettings['tools']; label: string; hint: string }[] = [
  {
    value: 'own',
    label: 'Claude Code’s own',
    hint: 'Claude Code runs its own tools (Read, Edit, Bash…) under the permission mode below; Rovik shows each step it took.',
  },
  {
    value: 'rovik',
    label: 'Rovik’s (via MCP)',
    hint: 'Claude Code is the model behind Rovik’s agent: it gets Rovik’s tools over MCP, and Rovik runs each call under Permissions and approval, in the chat’s copy of the project, with its checks, checklist, plans and subagents.',
  },
];

/** Whether the entry is a connector that runs Rovik's tools. */
export function usesRovikTools(p: Pick<Provider, 'baseUrl' | 'connector' | 'settings'>): boolean {
  return specOf(p)?.usesRovikTools(p.settings ?? {}) ?? false;
}

/*
 * The hints of the settings' choices are their options' tooltips. On Linux,
 * WebKitGTK shows an open <select>'s tooltips as GTK markup, in which `&`,
 * `<` and `>` break the text ("Entity did not end with a semicolon"), so
 * they are written without them (connectors.test.ts checks).
 */

export const PERMISSION_MODES: {
  value: ClaudeCodeSettings['permissionMode'];
  label: string;
  hint: string;
}[] = [
  {
    value: 'rovik',
    label: 'Follow Rovik’s permissions',
    hint: 'Each call Claude Code would ask about goes to Permissions and approval, as the agent’s own would: commands by the allowed list, edits reviewed in the editor, approvals by you or the AI reviewer',
  },
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

/**
 * What Rovik knows of a kind of connector, the same for every kind: a new
 * harness is one more entry here (and its settings fields, in
 * ConnectorsSettings.tsx), besides its module in the core.
 */
export interface ConnectorSpec {
  kind: ConnectorKind;
  title: string;
  about: string;
  /** The command that installs it, for the user. */
  install: string;
  /** Its settings with the defaults for what isn't set: `{}` gives a new one's. */
  settings: (raw: Record<string, unknown>) => Record<string, unknown>;
  /** Whether its runs get Rovik's tools (over MCP), as its settings say. */
  usesRovikTools: (settings: Record<string, unknown>) => boolean;
  /** For its row in the list: the program and how it runs. */
  summary: (settings: Record<string, unknown>) => string;
  /** The model its chats' titles are asked of; none: the chat's own. */
  titleModel?: string;
}

/**
 * The model a Claude Code chat's title is asked of: Claude Code's small,
 * fast one, as Claude Code titles its own sessions. A title needs no more,
 * and the chat's model may be Opus or Fable.
 */
export const CLAUDE_CODE_TITLE_MODEL = 'haiku';

/** What can be connected: each kind, for the "Add connector" menu. */
export const CONNECTOR_CATALOG: ConnectorSpec[] = [
  {
    kind: 'claude-code',
    title: 'Claude Code',
    about:
      'Anthropic’s coding agent, run headless in the project folder with its own tools. Chats that pick one of its models hand it the task; its steps, tokens and cost show with the reply.',
    install: 'curl -fsSL https://claude.ai/install.sh | bash',
    settings: (raw) => claudeCodeSettings({ settings: raw }),
    usesRovikTools: (raw) => claudeCodeSettings({ settings: raw }).tools === 'rovik',
    summary: (raw) => {
      const s = claudeCodeSettings({ settings: raw });
      const mode = PERMISSION_MODES.find((m) => m.value === s.permissionMode)?.label;
      return `${s.program} · ${s.tools === 'rovik' ? 'Rovik’s tools' : (mode ?? s.permissionMode)}`;
    },
    titleModel: CLAUDE_CODE_TITLE_MODEL,
  },
];

export function connectorSpec(kind: ConnectorKind | undefined): ConnectorSpec | undefined {
  return CONNECTOR_CATALOG.find((c) => c.kind === kind);
}

/** The spec of the connector `p` is; none for a provider. */
export function specOf(p: Pick<Provider, 'baseUrl' | 'connector'>): ConnectorSpec | undefined {
  return connectorSpec(kindOfProvider(p));
}

/** For the user: `Claude Code`. */
export function connectorTitle(kind: ConnectorKind | undefined): string {
  return connectorSpec(kind)?.title ?? kind ?? 'Connector';
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
    settings: connectorSpec(kind)?.settings({}) ?? {},
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
  /** Whether it's signed in, when the program could say. */
  auth?: ConnectorAuth | null;
  /** A newer version than `version` is published: this one. */
  update?: string | null;
}

/** What the core's connector calls are asked with (`ProbeRequestArgs`). */
export interface ConnectorArgs {
  kind: ConnectorKind;
  settings: Record<string, unknown>;
  apiKey: string;
  request: boolean;
}

/** The connector `p` as the core's calls take it; throws for a provider. */
export function connectorArgs(p: Provider, request = false): ConnectorArgs {
  const kind = kindOfProvider(p);
  if (!kind) throw new Error('Not a connector.');
  return { kind, settings: p.settings ?? {}, apiKey: p.apiKey, request };
}

/**
 * Calls one of the core's connector commands on the connector `p`, here or
 * on `host` (a cloud host's id): `connector_probe`, `connector_auth_status`.
 */
export async function connectorCall<T>(
  command: string,
  p: Provider,
  host?: string | null,
  request = false
): Promise<T> {
  return invoke<T>(command, { args: connectorArgs(p, request), host: host ?? null });
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
  return connectorCall<ConnectorProbe>('connector_probe', p, host, request);
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

/**
 * One line for the Settings menu: how many connectors and models there
 * are, and how many of them can't run now (signed out, not installed), as
 * their last sign-in check found.
 */
export function connectorsSummary(
  providers: Provider[],
  states: Record<string, AuthState> = authStates()
): string {
  const connectors = splitSources(providers).connectors;
  if (connectors.length === 0) return 'None yet: connect Claude Code to use it in place of a model';
  const models = connectors.reduce((n, c) => n + c.models.filter((m) => m.enabled).length, 0);
  const line = `${plural(connectors.length, 'connector')}, ${plural(models, 'model')} enabled`;
  const out = connectors.filter((c) => cantRun(states[c.id])).length;
  return out ? `${line} · ${out} can’t run: not signed in or installed` : line;
}

/*
 * Sign-in: whether each connector's program is signed in on this computer
 * (`claude auth status`, which costs nothing), checked once as a window
 * starts for the connectors a chat can pick, again when one is saved or
 * tested, and on demand. The program's own check: a token it has counts
 * until a request says it expired, which the chat then shows as its error.
 */

/** What `connector_auth_status` answers (rovik_protocol::connectors). */
export interface ConnectorAuth {
  loggedIn: boolean;
  method?: string | null;
  configDir?: string | null;
  detail?: string | null;
}

export type AuthState =
  | { state: 'checking' }
  | { state: 'signedIn'; auth: ConnectorAuth }
  | { state: 'signedOut'; auth: ConnectorAuth }
  /** It couldn't say: the program is missing, or too old, or didn't answer. */
  | { state: 'unknown'; message: string; missing: boolean };

const [authStates, setAuthStates] = createSignal<Record<string, AuthState>>({});

/** The last sign-in check of a connector, by its id; none before one. */
export function connectorAuthState(id: string): AuthState | undefined {
  return authStates()[id];
}

export function setConnectorAuthState(id: string, state: AuthState): void {
  setAuthStates((all) => ({ ...all, [id]: state }));
}

/** A state a chat on the connector would fail in. */
export function cantRun(state: AuthState | undefined): boolean {
  return state?.state === 'signedOut' || (state?.state === 'unknown' && state.missing);
}

/** Asks the program whether it's signed in, here or on `host`. */
export function connectorAuthStatus(p: Provider, host?: string | null): Promise<ConnectorAuth> {
  return connectorCall<ConnectorAuth>('connector_auth_status', p, host);
}

/** An answer, or a failure to get one, as a state. */
export function authStateOf(result: { auth: ConnectorAuth } | { error: unknown }): AuthState {
  if ('auth' in result) {
    return result.auth.loggedIn
      ? { state: 'signedIn', auth: result.auth }
      : { state: 'signedOut', auth: result.auth };
  }
  const message = errorText(result.error);
  return { state: 'unknown', message, missing: isMissingProgram(message) };
}

/** An error as the user reads it. */
export function errorText(error: unknown): string {
  return String(error).replace(/^Error: /, '');
}

/** Whether a connector's error says its program isn't there to run. */
export function isMissingProgram(message: string): boolean {
  return /isn't installed|no such program/i.test(message);
}

/** Checks one connector on this computer, and keeps what it found. */
export async function checkConnectorAuth(
  p: Provider,
  check: (p: Provider) => Promise<ConnectorAuth> = connectorAuthStatus
): Promise<AuthState> {
  setConnectorAuthState(p.id, { state: 'checking' });
  const state = await check(p).then(
    (auth) => authStateOf({ auth }),
    (error) => authStateOf({ error })
  );
  setConnectorAuthState(p.id, state);
  return state;
}

const METHODS: Record<string, string> = {
  'claude.ai': 'claude.ai login',
  oauth_token: 'token',
  api_key: 'API key',
  api_key_helper: 'API key helper',
  third_party: 'cloud provider',
};

/** "Signed in (claude.ai login)", "Not signed in: …", for a row or a form. */
export function authLine(state: AuthState | undefined): { text: string; warning: boolean } | null {
  if (!state) return null;
  switch (state.state) {
    case 'checking':
      return { text: 'Checking sign-in…', warning: false };
    case 'signedIn': {
      const how = state.auth.method ? (METHODS[state.auth.method] ?? state.auth.method) : '';
      const text = `Signed in${how ? ` (${how})` : ''}${state.auth.detail ? `. ${state.auth.detail}` : ''}`;
      return { text, warning: false };
    }
    case 'signedOut':
      return { text: state.auth.detail || 'Not signed in.', warning: true };
    case 'unknown':
      return {
        text: state.missing ? state.message : `Sign-in unknown: ${state.message}`,
        warning: state.missing,
      };
  }
}

/** The connectors a chat can pick: those with an enabled model. */
export function usableConnectors(providers: Provider[]): Provider[] {
  return splitSources(providers).connectors.filter((c) => c.models.some((m) => m.enabled));
}

let checkedAtStart = false;

/** Tests: the start's check runs again. */
export function resetStartCheck(): void {
  checkedAtStart = false;
  setAuthStates({});
}

/** The notification the start's check raises: one, updated in place. */
export const CONNECTOR_AUTH_NOTE = 'connector-auth';

/**
 * As a window starts: checks each connector a chat can pick, once, and
 * says which can't run (signed out, or the program missing), with a way
 * to Settings → Connectors. Checks that can't say (an old version) are
 * left for the settings page. Chats on a cloud host run its own
 * installation: the page checks a host when asked.
 */
export async function checkConnectorsOnStart(
  providers: Provider[],
  openConnectors: () => void,
  check: (p: Provider) => Promise<ConnectorAuth> = connectorAuthStatus
): Promise<Record<string, AuthState>> {
  if (checkedAtStart) return {};
  const connectors = usableConnectors(providers);
  if (connectors.length === 0) return {};
  checkedAtStart = true;
  const found: Record<string, AuthState> = {};
  await Promise.all(
    connectors.map(async (c) => {
      found[c.id] = await checkConnectorAuth(c, check);
    })
  );
  const bad = connectors.filter((c) => cantRun(found[c.id]));
  if (bad.length > 0) {
    const reason = (c: Provider) => {
      const state = found[c.id];
      return state.state === 'unknown' ? state.message : (authLine(state)?.text ?? '');
    };
    const one = bad.length === 1 ? bad[0] : null;
    const state = one ? found[one.id] : null;
    notify({
      id: CONNECTOR_AUTH_NOTE,
      kind: 'warning',
      title: one
        ? `${one.name} ${state?.state === 'unknown' ? 'isn’t installed' : 'isn’t signed in'}`
        : `${bad.length} connectors can’t run`,
      message: one
        ? `${reason(one)} Chats using its models will fail until it is.`
        : bad.map((c) => `${c.name}: ${reason(c)}`).join('\n'),
      actions: [{ label: 'Open Connectors', primary: true, run: openConnectors }],
    });
  }
  return found;
}

/**
 * The profile a chat's title is asked with: a connector's on the model its
 * spec names for titles (a Claude Code chat's on Haiku), any other (a
 * provider's, another connector's) as it is.
 */
export function titleProfile(profile: ModelProfile): ModelProfile {
  const model = connectorSpec(connectorKindOf(profile.baseUrl))?.titleModel;
  if (!model) return profile;
  return { ...profile, model, label: profile.label.replace(/[^/]*$/, model) };
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
    // With its own tools, the IDE offers it none; with Rovik's, the run's
    // tools go to it over MCP.
    noTools: !usesRovikTools(provider),
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
    /** `harnessTurn`: the cost is the whole run's, over several replies. */
    costCovers?: string;
    permissionDenials?: number;
    /** The harness's own context after the reply, as it reported it. */
    context?: ConnectorContext;
  };
}

/**
 * A connector's context after a reply (claude_code.rs `context_of`): the
 * tokens its session holds, of its model's window, and where it compacts
 * the conversation on its own. The harness keeps the history, not Rovik.
 */
export interface ConnectorContext {
  used: number;
  window: number;
  compactsAt?: number;
}

export interface ConnectorMeta {
  kind?: string;
  session?: string;
  model?: string | null;
  version?: string | null;
  steps?: HarnessStep[];
  usage?: ConnectorUsage;
  /** It ran Rovik's tools: its calls are the reply's own tool calls. */
  rovikTools?: boolean;
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

/**
 * "Claude Code · claude-sonnet-5 · 7 steps · 12.3k tokens (8.1k cached) · $0.04 · 3 turns";
 * while it runs, `state` (`Stopping…`, `waiting for your approval`), else "working…" until a step.
 */
export function connectorHeadline(meta: ConnectorMeta, running: boolean, state?: string): string {
  const parts: string[] = [];
  const kind = meta.kind ? connectorTitle(meta.kind as ConnectorKind) : 'Connector';
  parts.push(kind);
  const model = meta.usage?.connector?.model ?? meta.model;
  if (model) parts.push(model);
  const steps = meta.steps?.length ?? 0;
  if (steps) parts.push(plural(steps, 'step'));
  if (running && state) parts.push(state);
  else if (running && !steps && !meta.rovikTools) parts.push('working…');
  const u = meta.usage;
  if (u) {
    const tokens = (u.prompt_tokens ?? 0) + (u.completion_tokens ?? 0);
    const cached = u.prompt_tokens_details?.cached_tokens ?? 0;
    if (tokens)
      parts.push(`${compact(tokens)} tokens${cached ? ` (${compact(cached)} cached)` : ''}`);
    // A command run without a request to the model (`/context`) cost nothing to say.
    if (u.cost !== undefined && (u.cost > 0 || tokens > 0)) {
      const cost = `$${u.cost < 0.01 ? u.cost.toFixed(4) : u.cost.toFixed(2)}`;
      // With Rovik's tools, a run of Claude Code's spans several replies:
      // its cost comes with the last.
      parts.push(u.connector?.costCovers === 'harnessTurn' ? `${cost} for its run` : cost);
    }
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
  // Nothing used (a command run without a request): nothing to count.
  if (!u.prompt_tokens && !u.completion_tokens && !u.cost) return null;
  const used = { inputTokens: u.prompt_tokens ?? 0, outputTokens: u.completion_tokens ?? 0 } as {
    inputTokens: number;
    outputTokens: number;
    cost?: number;
  };
  if (u.cost !== undefined) used.cost = u.cost;
  return used;
}

/**
 * The context the latest connector reply in `messages` reported, and the
 * connector's kind; none before one has.
 */
export function latestConnectorContext(
  messages: { role: string; meta?: { connector?: ConnectorMeta } | null }[]
): { kind?: string; context: ConnectorContext } | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const meta = messages[i].role === 'assistant' ? messages[i].meta?.connector : undefined;
    const context = meta?.usage?.connector?.context;
    if (context && context.window > 0) return { kind: meta?.kind, context };
  }
  return null;
}
