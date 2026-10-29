// connectors.ts
//
// Connectors: coding harnesses a chat can use in place of a model
// provider (Settings → Connectors). A connector is a program on the
// computer the chat runs on, Claude Code or Codex: it brings its own model
// loop, tools and prompt, so the IDE hands it the model, the message and
// the project folder, follows what it does, and shows its steps, its
// tokens and its cost with the reply
// (src-tauri/crates/rovik-core/src/connectors). That's Agent mode; in Plan
// mode and Autopilot it's the model behind Rovik's agent instead, with
// Rovik's prompt and Rovik's tools (over MCP).
//
// A connector is kept as a `Provider` entry in the AI settings whose
// `baseUrl` is `connector://<kind>` (its token or key is `apiKey`, so it
// goes to the keychain as a provider's key does), with the harness's
// settings in `settings`. So the model picker, a chat's saved model, the
// AI reviewer and the settings file work as they do for providers; the
// pages and the picker tell the two apart by `isConnector`.
//
// This module is the same for every kind. A kind of connector brings its
// own: in the core, its `Connector` (connectors/mod.rs); here, its
// `ConnectorSpec` in `CONNECTOR_CATALOG`, in a module of its own
// (claudeCode.ts, codex.ts); and its form's `ConnectorFields`
// (components/connectorFields.ts).
import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';
import type {
  ChatMessage,
  ChatToolCall,
  ConnectorKind,
  ModelProfile,
  Provider,
  ProviderModel,
} from '../types';
import { plural } from './textUtils';
import { isErrorResult } from './toolArgs';
import type { RemoteModel } from './aiSettings';
import { notify } from './notifications';
import { CLAUDE_CODE } from './claudeCode';
import { CODEX } from './codex';

export const CONNECTOR_SCHEME = 'connector://';

/**
 * What Rovik knows of a kind of connector, the same for every kind: each
 * kind's is in a module of its own (claudeCode.ts, codex.ts), one entry of
 * `CONNECTOR_CATALOG`.
 */
export interface ConnectorSpec {
  kind: ConnectorKind;
  title: string;
  about: string;
  /** The command that installs it, for the user. */
  install: string;
  /** Its settings with the defaults for what isn't set: `{}` gives a new one's. */
  settings: (raw: Record<string, unknown>) => Record<string, unknown>;
  /** For its row in the list: the program and how it runs. */
  summary: (settings: Record<string, unknown>) => string;
  /** The model its chats' titles are asked of; none: the chat's own. */
  titleModel?: string;
  /** Under its models' table while it has none: how to get them. */
  modelsHint: string;
  /**
   * One of its own tool calls as the chat shows it: as the call of Rovik's
   * tool of the same kind, its arguments as that one takes them, so its
   * line and block read as Rovik's do (`connectorSegments`). A skill it
   * uses is a `skill` call (`{ skill, args }`), which Rovik's agent has
   * none of. None for a tool Rovik has none like: shown by its own name
   * and input.
   */
  chatCall: (name: string, input: Record<string, unknown>) => ChatCall | undefined;
}

/** A call of one of Rovik's tools: its name and its arguments. */
export interface ChatCall {
  name: string;
  arguments: Record<string, unknown>;
}

/**
 * One of a setting's choices in a connector's form: the line shown under
 * its control (`hint`, also its option's tooltip) and, where there's more
 * to say, all of it as that line's tooltip (`detail`). On Linux, WebKitGTK
 * shows an open <select>'s tooltips as GTK markup, in which `&`, `<` and
 * `>` break the text ("Entity did not end with a semicolon"), so hints are
 * written without them (connectors.test.ts checks).
 */
export interface Choice<T extends string> {
  value: T;
  label: string;
  hint: string;
  detail?: string;
}

/** What can be connected: each kind, for the "Add connector" menu. */
export const CONNECTOR_CATALOG: ConnectorSpec[] = [CLAUDE_CODE, CODEX];

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
 * on `host` (a cloud host's id): `connector_probe`.
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
 * their last check found.
 */
export function connectorsSummary(
  providers: Provider[],
  states: Record<string, AuthState> = authStates()
): string {
  const connectors = splitSources(providers).connectors;
  if (connectors.length === 0)
    return 'None yet: connect Claude Code or Codex to use one in place of a model';
  const models = connectors.reduce((n, c) => n + c.models.filter((m) => m.enabled).length, 0);
  const line = `${plural(connectors.length, 'connector')}, ${plural(models, 'model')} enabled`;
  const out = connectors.filter((c) => cantRun(states[c.id])).length;
  return out ? `${line} · ${out} can’t run: not signed in or installed` : line;
}

/*
 * Status: whether each connector's program is installed on this computer
 * (its version), signed in (`claude auth status`) and the newest version
 * its makers publish, as a test that makes no request finds them, so for
 * no tokens. Checked once as a window starts, again as Settings →
 * Connectors opens, when one is saved or tested here, and on demand. The
 * program's own sign-in check: a token it has counts until a request says
 * it expired, which the chat then shows as its error.
 */

/** Whether the program is signed in, as it says (rovik_protocol::connectors). */
export interface ConnectorAuth {
  loggedIn: boolean;
  method?: string | null;
  configDir?: string | null;
  detail?: string | null;
}

/** What a check found: signed in or out, or that it couldn't say (and why). */
export type CheckedState = (
  | { state: 'signedIn'; auth: ConnectorAuth }
  | { state: 'signedOut'; auth: ConnectorAuth }
  /** It couldn't say: the program is missing, or too old, or didn't answer. */
  | { state: 'unknown'; message: string; missing: boolean }
) & {
  /** The test that found it, when the program answered: its version, and a newer one published. */
  probe?: ConnectorProbe;
};

/** What the last check found, or that one runs (what the last found stays shown meanwhile). */
export type AuthState = { state: 'checking'; last?: CheckedState } | CheckedState;

const [authStates, setAuthStates] = createSignal<Record<string, AuthState>>({});

/** The last check of a connector on this computer, by its id; none before one. */
export function connectorAuthState(id: string): AuthState | undefined {
  return authStates()[id];
}

export function setConnectorAuthState(id: string, state: AuthState): void {
  setAuthStates((all) => ({ ...all, [id]: state }));
}

/** What the last check found, also while another runs; none before one has. */
export function lastFound(state: AuthState | undefined): CheckedState | undefined {
  return state?.state === 'checking' ? state.last : state;
}

/** A state a chat on the connector would fail in. */
export function cantRun(state: AuthState | undefined): boolean {
  const found = lastFound(state);
  return found?.state === 'signedOut' || (found?.state === 'unknown' && found.missing);
}

/** The test that makes no request, so costs no tokens, on this computer. */
export const probeHere = (p: Provider): Promise<ConnectorProbe> => probeConnector(p, false);

/** A test's answer, or a failure to get one, as a state. */
export function authStateOf(result: { probe: ConnectorProbe } | { error: unknown }): CheckedState {
  if ('probe' in result) {
    // The state is what's installed and signed in: a request the test made
    // isn't part of it.
    const probe = { ...result.probe, request: null };
    const { auth } = probe;
    if (!auth) {
      return {
        state: 'unknown',
        message: "it didn't say whether it's signed in.",
        missing: false,
        probe,
      };
    }
    return auth.loggedIn ? { state: 'signedIn', auth, probe } : { state: 'signedOut', auth, probe };
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

/**
 * Checks one connector on this computer with a test that makes no
 * request, and keeps what it found; the last check's stays shown meanwhile.
 */
export async function checkConnector(
  p: Provider,
  probe: (p: Provider) => Promise<ConnectorProbe> = probeHere
): Promise<CheckedState> {
  setConnectorAuthState(p.id, { state: 'checking', last: lastFound(connectorAuthState(p.id)) });
  const state = await probe(p).then(
    (probe) => authStateOf({ probe }),
    (error) => authStateOf({ error })
  );
  setConnectorAuthState(p.id, state);
  return state;
}

/** Each program's sign-in methods, as its sign-in check names them. */
const METHODS: Record<string, string> = {
  'claude.ai': 'claude.ai login',
  oauth_token: 'token',
  api_key: 'API key',
  api_key_helper: 'API key helper',
  third_party: 'cloud provider',
  chatgpt: 'ChatGPT login',
  access_token: 'access token',
  personal_access_token: 'personal access token',
};

/** "Signed in (claude.ai login)", "Not signed in: …", for a row or a form. */
export function authLine(state: AuthState | undefined): { text: string; warning: boolean } | null {
  const found = lastFound(state);
  if (!found) return state ? { text: 'Checking sign-in…', warning: false } : null;
  switch (found.state) {
    case 'signedIn': {
      const how = found.auth.method ? (METHODS[found.auth.method] ?? found.auth.method) : '';
      const text = `Signed in${how ? ` (${how})` : ''}${found.auth.detail ? `. ${found.auth.detail}` : ''}`;
      return { text, warning: false };
    }
    case 'signedOut':
      return { text: found.auth.detail || 'Not signed in.', warning: true };
    case 'unknown':
      return {
        text: found.missing ? found.message : `Sign-in unknown: ${found.message}`,
        warning: found.missing,
      };
  }
}

/** "Installed: 2.1.310 (Claude Code) · 2.2.0 is available", for a row; none before a test answered. */
export function versionLine(state: AuthState | undefined): string | null {
  const probe = lastFound(state)?.probe;
  if (!probe) return null;
  const installed = `Installed: ${probe.version || probe.program}`;
  return probe.update ? `${installed} · ${probe.update} is available` : installed;
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
 * As a window starts: checks each connector, once, so Settings shows what
 * it found as it opens, and says which a chat can pick can't run (signed
 * out, or the program missing), with a way to Settings → Connectors.
 * Checks that can't say (an old version) are left for the settings page.
 * Chats on a cloud host run its own installation: the page checks a host
 * when asked.
 */
export async function checkConnectorsOnStart(
  providers: Provider[],
  openConnectors: () => void,
  probe: (p: Provider) => Promise<ConnectorProbe> = probeHere
): Promise<Record<string, CheckedState>> {
  if (checkedAtStart) return {};
  const connectors = splitSources(providers).connectors;
  if (connectors.length === 0) return {};
  checkedAtStart = true;
  const found: Record<string, CheckedState> = {};
  await Promise.all(
    connectors.map(async (c) => {
      found[c.id] = await checkConnector(c, probe);
    })
  );
  const bad = usableConnectors(providers).filter((c) => cantRun(found[c.id]));
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
    // It takes Rovik's tools (over MCP): the run offers them in Plan mode,
    // Autopilot and to a subagent, and none in Agent mode, where it runs
    // its own (the core's `run::lead_profile`).
    noTools: false,
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
  /** A stretch of thinking rather than a tool call: its text so far (a summary). */
  thinking?: string | null;
  /** How long that thinking took, once it's over (ms). */
  ms?: number | null;
  /** Where in the reply's text it came: the text's length then (UTF-16 code units). */
  at?: number | null;
}

/** A stretch of the harness's thinking, among its steps but not one it ran. */
export function isThought(step: HarnessStep): boolean {
  return typeof step.thinking === 'string';
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

/** One of a connector's tool steps as the chat shows it: Rovik's call, and its result as a tool's. */
export interface ChatStep {
  call: ChatToolCall;
  /** Once the step has one. */
  result?: ChatMessage;
}

/**
 * What a reply's harness did, in order: a stretch of its thinking, the
 * tool calls between two, or the text it wrote before a step.
 */
export type ConnectorSegment = { thought: HarnessStep } | { calls: ChatStep[] } | { text: string };

/** A step's chat form as last made, to give it back while the step doesn't change. */
export interface KeptStep {
  key: string;
  call: ChatToolCall;
  resultKey?: string;
  result?: ChatMessage;
}

/**
 * A reply's steps as the chat shows them, in order: its thoughts apart,
 * the tool calls between them as Rovik's calls of the same kind (the
 * kind's `chatCall`), each with its result as a tool's. Its subagents'
 * steps aren't the reply's: the call that started one has its report.
 * With `kept`, a step that hasn't changed gives the same objects as
 * before, so the chat's blocks keep their state while the reply goes on.
 * With the reply's `text`, what it wrote before a step that says where it
 * came (`at`) is in its place before that step; what it wrote after its
 * last step is `textAfterSteps`.
 */
export function connectorSegments(
  meta: ConnectorMeta,
  kept: Map<string, KeptStep> = new Map(),
  text = ''
): ConnectorSegment[] {
  const spec = connectorSpec(meta.kind as ConnectorKind);
  const segments: ConnectorSegment[] = [];
  let shown = 0;
  for (const step of meta.steps ?? []) {
    if (step.parent) continue;
    if (typeof step.at === 'number' && step.at > shown) {
      const before = text.slice(shown, step.at);
      shown = step.at;
      if (before.trim()) segments.push({ text: before });
    }
    if (isThought(step)) {
      segments.push({ thought: step });
      continue;
    }
    const chat = chatStep(step, spec, kept);
    const last = segments[segments.length - 1];
    if (last && 'calls' in last) last.calls.push(chat);
    else segments.push({ calls: [chat] });
  }
  return segments;
}

/**
 * The reply's text after its last step, which shows as its own; what came
 * before shows among its steps (`connectorSegments`). All of it when its
 * steps don't say where they came (a reply stored before they did).
 */
export function textAfterSteps(meta: ConnectorMeta, text: string): string {
  let at = 0;
  for (const step of meta.steps ?? []) {
    if (!step.parent && typeof step.at === 'number') at = Math.max(at, step.at);
  }
  return text.slice(at);
}

/**
 * Rovik's `mcp` tool as a harness running its own tools calls it, served by
 * Rovik (docs/design/mcp-plan.md, Q09): every kind's step of it is Rovik's
 * `mcp` call.
 */
const ROVIK_MCP_STEP = 'mcp__rovik__mcp';

/** A tool step as Rovik's call, and its result once it has one ("Error: …" when it failed). */
function chatStep(
  step: HarnessStep,
  spec: ConnectorSpec | undefined,
  kept: Map<string, KeptStep>
): ChatStep {
  const input =
    step.input && typeof step.input === 'object' ? (step.input as Record<string, unknown>) : {};
  const like =
    step.name === ROVIK_MCP_STEP
      ? { name: 'mcp', arguments: input }
      : spec?.chatCall(step.name, input);
  const name = like?.name ?? step.name;
  const args = JSON.stringify(like?.arguments ?? input);
  const key = `${name}\n${args}`;
  let entry = kept.get(step.id);
  if (!entry || entry.key !== key) {
    entry = { key, call: { id: step.id, name, arguments: args } };
    kept.set(step.id, entry);
  }
  if (step.ok === undefined || step.ok === null) return { call: entry.call };
  const text = step.result ?? '';
  const content = step.ok || isErrorResult(text) ? text : `Error: ${text}`;
  if (entry.resultKey !== content) {
    entry.resultKey = content;
    // The harness's own output, cut short: nothing in it is read as Rovik's
    // tools' results are (an exit code, a count), which it doesn't say.
    const meta = {};
    entry.result = { id: `${step.id}:result`, role: 'tool', content, tool_call_id: step.id, meta };
  }
  return { call: entry.call, result: entry.result };
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
