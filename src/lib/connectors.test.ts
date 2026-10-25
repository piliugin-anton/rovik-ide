// Connectors (lib/connectors.ts): how a connector is told from a provider,
// resolved for a request, summarized, and how its reply's record reads.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Provider } from '../types';
import {
  authLine,
  authStateOf,
  cantRun,
  checkConnector,
  checkConnectorsOnStart,
  CONNECTOR_AUTH_NOTE,
  connectorAuthState,
  resetStartCheck,
  setConnectorAuthState,
  type ConnectorAuth,
  type ConnectorProbe,
  usesRovikTools,
  connectorSegments,
  type ConnectorMeta,
  type ConnectorSegment,
  connectorKindOf,
  connectorsSummary,
  connectorUsage,
  isConnector,
  latestConnectorContext,
  newConnector,
  probeModels,
  probeSummary,
  splitSources,
  titleProfile,
  specOf,
  versionLine,
} from './connectors';
import {
  AUTH_KINDS,
  claudeCodeSettings,
  CLAUDE_CODE_DEFAULTS,
  CLAUDE_CODE_TITLE_MODEL,
  PERMISSION_MODES,
  USE_MODES,
} from './claudeCode';
import { providersSummary, resolveProfile } from './aiSettings';
import { EFFORTS } from './reasoning';
import { dismiss, notifications } from './notifications';

// The notifications' timers are the window's.
(globalThis as any).window ??= { setTimeout, clearTimeout };

const openai: Provider = {
  id: 'p1',
  name: 'OpenAI',
  baseUrl: 'https://api.openai.com/v1',
  apiKey: 'sk',
  stream: true,
  models: [{ id: 'gpt-5', enabled: true }],
};

test('a connector is a provider entry with a connector:// base URL', () => {
  const claude = newConnector('claude-code');
  assert.equal(claude.baseUrl, 'connector://claude-code');
  assert.equal(claude.name, 'Claude Code');
  assert.equal(claude.connector, 'claude-code');
  assert.deepEqual(claude.settings, CLAUDE_CODE_DEFAULTS);
  assert.ok(isConnector(claude));
  assert.ok(!isConnector(openai));
  // An entry saved by the base URL alone still counts.
  assert.ok(isConnector({ baseUrl: 'connector://claude-code/' }));
  assert.equal(connectorKindOf('connector://codex'), undefined);
  assert.equal(connectorKindOf('http://localhost:11434/v1'), undefined);
  const { providers, connectors } = splitSources([openai, claude]);
  assert.deepEqual(
    providers.map((p) => p.id),
    ['p1']
  );
  assert.deepEqual(
    connectors.map((p) => p.id),
    [claude.id]
  );
});

test('a connector’s model resolves to a profile the harness runs, with no IDE tools', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1', apiKey: 'tok' };
  claude.models = [{ id: 'sonnet', enabled: true, contextLength: 1_000_000 }];
  claude.settings = { ...CLAUDE_CODE_DEFAULTS, auth: 'token', maxBudgetUsd: 2 };
  const profile = resolveProfile([openai, claude], { providerId: 'c1', modelId: 'sonnet' });
  assert.ok(profile);
  assert.equal(profile.id, 'c1::sonnet');
  assert.equal(profile.label, 'Claude Code/sonnet');
  assert.equal(profile.baseUrl, 'connector://claude-code');
  assert.equal(profile.apiKey, 'tok');
  assert.equal(profile.noTools, true, 'the harness runs its own tools');
  assert.equal(profile.vision, false);
  assert.equal(profile.contextTokens, 1_000_000);
  assert.deepEqual(profile.connector, claude.settings);
  // A provider's model is as before.
  const plain = resolveProfile([openai, claude], { providerId: 'p1', modelId: 'gpt-5' });
  assert.equal(plain?.connector, undefined);
  assert.equal(plain?.noTools, false);
});

test('a Claude Code chat is titled by Haiku; any other by its own model', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1' };
  claude.models = [{ id: 'opus', enabled: true }];
  const profile = resolveProfile([openai, claude], { providerId: 'c1', modelId: 'opus' });
  assert.ok(profile);
  const asked = titleProfile(profile);
  assert.equal(CLAUDE_CODE_TITLE_MODEL, 'haiku');
  assert.equal(asked.model, 'haiku');
  assert.equal(asked.label, 'Claude Code/haiku');
  assert.equal(asked.baseUrl, profile.baseUrl);
  assert.deepEqual(asked.connector, profile.connector);
  assert.equal(profile.model, 'opus', 'the chat’s profile is left as it is');
  const plain = resolveProfile([openai, claude], { providerId: 'p1', modelId: 'gpt-5' });
  assert.ok(plain);
  assert.equal(titleProfile(plain), plain);
  // Another connector kind keeps its model.
  const other = { ...profile, baseUrl: 'connector://codex' };
  assert.equal(titleProfile(other), other);
});

test('a kind’s spec gives a new connector its settings and its row its summary', () => {
  const claude = newConnector('claude-code');
  const spec = specOf(claude);
  assert.ok(spec);
  assert.deepEqual(claude.settings, CLAUDE_CODE_DEFAULTS);
  assert.equal(spec.summary(claude.settings ?? {}), 'claude · Follow Rovik’s permissions');
  assert.equal(
    spec.summary({ tools: 'rovik', program: '/opt/claude' }),
    '/opt/claude · Rovik’s agent'
  );
  assert.equal(specOf(openai), undefined);
});

test('settings take the defaults for what isn’t set', () => {
  assert.deepEqual(claudeCodeSettings({}), CLAUDE_CODE_DEFAULTS);
  const s = claudeCodeSettings({ settings: { program: '  ', permissionMode: 'plan' } });
  assert.equal(s.program, 'claude');
  assert.equal(s.permissionMode, 'plan');
  assert.equal(s.auth, 'login');
});

test('a system prompt choice saved before is dropped: the prompt is the tools’', () => {
  const s = claudeCodeSettings({ settings: { tools: 'rovik', systemPrompt: 'replace' } });
  assert.equal(s.tools, 'rovik');
  assert.ok(!('systemPrompt' in s), 'nothing sends it to the core');
});

test('the menu lines count providers and connectors apart', () => {
  const claude = newConnector('claude-code');
  claude.models = [
    { id: 'sonnet', enabled: true },
    { id: 'opus', enabled: false },
  ];
  assert.equal(
    connectorsSummary([openai]),
    'None yet: connect Claude Code to use it in place of a model'
  );
  assert.equal(connectorsSummary([openai, claude]), '1 connector, 1 model enabled');
  assert.equal(providersSummary([openai, claude]), '1 provider, 1 model enabled');
  assert.equal(providersSummary([claude]), 'No providers: chats use connectors');
  assert.equal(providersSummary([]), 'No providers yet: add one to chat');
});

test('a probe’s models merge as a provider’s do, and its summary says what answered', () => {
  const probe = {
    program: 'claude',
    version: '2.1.309 (Claude Code)',
    models: [
      { id: 'sonnet', ownedBy: 'anthropic', contextLength: 1_000_000, description: 'Sonnet' },
    ],
    request: {
      model: 'claude-sonnet-5',
      inputTokens: 3000,
      outputTokens: 4,
      costUsd: 0.0006,
      ms: 1234,
    },
  };
  assert.deepEqual(probeModels(probe), [
    { id: 'sonnet', owned_by: 'anthropic', context_length: 1_000_000 },
  ]);
  assert.equal(
    probeSummary(probe),
    'Connected: 2.1.309 (Claude Code) · answered with claude-sonnet-5 in 1.2 s · $0.0006'
  );
  assert.equal(probeSummary({ ...probe, request: null }), 'Connected: 2.1.309 (Claude Code)');
});

test('what a reply used counts in the subagents’ totals; a command that asked nothing, none', () => {
  const meta = {
    kind: 'claude-code',
    usage: {
      prompt_tokens: 12_000,
      completion_tokens: 300,
      prompt_tokens_details: { cached_tokens: 8_100 },
      cost: 0.04,
      connector: { kind: 'claude-code', numTurns: 3, model: 'claude-sonnet-5' },
    },
  };
  assert.deepEqual(connectorUsage(meta), { inputTokens: 12_000, outputTokens: 300, cost: 0.04 });
  assert.equal(connectorUsage({ kind: 'claude-code' }), null);
  // A command run without a request to the model (`/context`): no cost, nothing counted.
  const command = {
    kind: 'claude-code',
    usage: { prompt_tokens: 0, completion_tokens: 0, cost: 0, connector: { numTurns: 0 } },
  };
  assert.equal(connectorUsage(command), null);
});

test('a reply’s steps show as Rovik’s work does: its thoughts apart, its calls as Rovik’s', () => {
  const meta: ConnectorMeta = {
    kind: 'claude-code',
    steps: [
      { id: 'thinking-1', name: '', input: null, thinking: 'Read it first.', ms: 1_200 },
      { id: 't1', name: 'Read', input: { file_path: 'src/a.rs' }, result: 'fn a() {}', ok: true },
      { id: 't2', name: 'Bash', input: { command: 'npm test' }, result: 'boom', ok: false },
      // A subagent's step isn't the reply's: the call that started it has its report.
      { id: 't9', name: 'Grep', input: { pattern: 'x' }, parent: 't3' },
      { id: 't3', name: 'Task', input: { description: 'Look around' } },
      { id: 'thinking-2', name: '', input: null, thinking: 'Done.' },
    ],
  };
  const segments = connectorSegments(meta);
  assert.deepEqual(
    segments.map((s) => ('thought' in s ? `thought:${s.thought.id}` : 'calls')),
    ['thought:thinking-1', 'calls', 'thought:thinking-2']
  );
  const calls = 'calls' in segments[1] ? segments[1].calls : [];
  assert.deepEqual(
    calls.map((c) => [c.call.id, c.call.name, JSON.parse(c.call.arguments)]),
    [
      ['t1', 'read', { path: 'src/a.rs' }],
      ['t2', 'bash', { command: 'npm test' }],
      // Rovik has no tool like it: its own name and input.
      ['t3', 'Task', { description: 'Look around' }],
    ]
  );
  assert.equal(calls[0].result?.content, 'fn a() {}');
  assert.equal(calls[0].result?.tool_call_id, 't1');
  assert.equal(calls[1].result?.content, 'Error: boom', 'a failed step reads as a failed call');
  assert.equal(calls[2].result, undefined, 'still running');
});

test('a step that hasn’t changed is the same call, so its block keeps its state', () => {
  const kept = new Map();
  const step = { id: 't1', name: 'Read', input: { file_path: 'a.rs' } };
  const first = connectorSegments({ kind: 'claude-code', steps: [step] }, kept);
  const done = { ...step, result: 'x', ok: true };
  const later = connectorSegments({ kind: 'claude-code', steps: [done] }, kept);
  const only = (s: ConnectorSegment[]) => ('calls' in s[0] ? s[0].calls[0] : undefined);
  assert.equal(only(later)?.call, only(first)?.call, 'the same call, now with its result');
  assert.equal(only(later)?.result?.content, 'x');
  const again = connectorSegments({ kind: 'claude-code', steps: [done] }, kept);
  assert.equal(only(again)?.result, only(later)?.result);
});

/*
 * Status at start (checkConnectorsOnStart), and as the pages check again
 */

const withModel = (id: string, name: string): Provider => ({
  ...newConnector('claude-code'),
  id,
  name,
  models: [{ id: 'sonnet', enabled: true }],
});

/** What a test that makes no request answers: the program, signed in or not as `auth` says. */
const probeOf = (auth?: ConnectorAuth, more: Partial<ConnectorProbe> = {}): ConnectorProbe => ({
  program: '/home/me/.local/bin/claude',
  version: '2.1.310 (Claude Code)',
  models: [],
  auth,
  ...more,
});

test('at start, each connector is checked once, and one a chat can pick that’s signed out is said', async () => {
  resetStartCheck();
  dismiss(CONNECTOR_AUTH_NOTE);
  const asked: string[] = [];
  let opened = 0;
  const signedIn = withModel('c1', 'Claude Code');
  const signedOut = withModel('c2', 'Claude Code (work)');
  const unused = { ...withModel('c3', 'Spare'), models: [{ id: 'opus', enabled: false }] };
  const check = async (p: Provider): Promise<ConnectorProbe> => {
    asked.push(p.id);
    return p.id === 'c1'
      ? probeOf({ loggedIn: true, method: 'claude.ai' })
      : probeOf({ loggedIn: false, method: 'none', detail: 'Not signed in: run `claude` once.' });
  };
  const found = await checkConnectorsOnStart(
    [openai, signedIn, signedOut, unused],
    () => opened++,
    check
  );
  // Every connector, so its row shows what was found as Settings opens.
  assert.deepEqual(asked.sort(), ['c1', 'c2', 'c3']);
  assert.equal(found.c1.state, 'signedIn');
  assert.equal(found.c2.state, 'signedOut');
  assert.equal(found.c3.state, 'signedOut');
  assert.equal(connectorAuthState('c2')?.state, 'signedOut', 'kept for the pages');
  assert.equal(versionLine(connectorAuthState('c3')), 'Installed: 2.1.310 (Claude Code)');
  // Only one a chat can pick is said: the spare one has no enabled model.
  const note = notifications().find((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.ok(note);
  assert.equal(note.kind, 'warning');
  assert.equal(note.title, 'Claude Code (work) isn’t signed in');
  assert.match(note.message ?? '', /run `claude` once.*fail until it is/);
  note.actions![0].run();
  assert.equal(opened, 1);
  // Once per window: a second call checks nothing.
  assert.deepEqual(await checkConnectorsOnStart([signedOut], () => opened++, check), {});
  assert.equal(asked.length, 3);
  // The menu line says how many can't run.
  assert.equal(
    connectorsSummary([openai, signedIn, signedOut]),
    '2 connectors, 2 models enabled · 1 can’t run: not signed in or installed'
  );
  dismiss(CONNECTOR_AUTH_NOTE);
});

test('at start, a missing program is said; a program that can’t say it’s signed in is left alone', async () => {
  resetStartCheck();
  dismiss(CONNECTOR_AUTH_NOTE);
  const missing = withModel('m1', 'Claude Code');
  const old = withModel('m2', 'Old Claude');
  const check = async (p: Provider): Promise<ConnectorProbe> => {
    if (p.id === 'm1') throw new Error("claude isn't installed, or isn't on the PATH.");
    return probeOf(undefined, { version: '2.0.1 (Claude Code)' });
  };
  const found = await checkConnectorsOnStart([missing, old], () => {}, check);
  assert.deepEqual(found.m1, {
    state: 'unknown',
    message: "claude isn't installed, or isn't on the PATH.",
    missing: true,
  });
  assert.equal(found.m2.state, 'unknown');
  assert.ok(!cantRun(found.m2));
  assert.deepEqual(authLine(found.m2), {
    text: "Sign-in unknown: it didn't say whether it's signed in.",
    warning: false,
  });
  const note = notifications().find((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.equal(note?.title, 'Claude Code isn’t installed');
  dismiss(CONNECTOR_AUTH_NOTE);
  // No connector: nothing is checked, nothing said.
  resetStartCheck();
  assert.deepEqual(await checkConnectorsOnStart([openai], () => {}, check), {});
  assert.equal(
    notifications().find((n) => n.id === CONNECTOR_AUTH_NOTE),
    undefined
  );
});

test('several connectors that can’t run share one notification', async () => {
  resetStartCheck();
  dismiss(CONNECTOR_AUTH_NOTE);
  const out = async () => probeOf({ loggedIn: false, detail: 'Not signed in.' });
  await checkConnectorsOnStart([withModel('a', 'A'), withModel('b', 'B')], () => {}, out);
  const notes = notifications().filter((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.equal(notes.length, 1);
  assert.equal(notes[0].title, '2 connectors can’t run');
  assert.equal(notes[0].message, 'A: Not signed in.\nB: Not signed in.');
  dismiss(CONNECTOR_AUTH_NOTE);
  resetStartCheck();
});

test('a check shows what the last one found until it answers', async () => {
  resetStartCheck();
  const claude = withModel('k1', 'Claude Code');
  setConnectorAuthState('k1', authStateOf({ probe: probeOf({ loggedIn: false }) }));
  let answer = (_: ConnectorProbe) => {};
  const checking = checkConnector(claude, () => new Promise((resolve) => (answer = resolve)));
  // Checking again: the last check's lines stay, and so does what can't run.
  assert.equal(connectorAuthState('k1')?.state, 'checking');
  assert.deepEqual(authLine(connectorAuthState('k1')), { text: 'Not signed in.', warning: true });
  assert.ok(cantRun(connectorAuthState('k1')));
  assert.equal(versionLine(connectorAuthState('k1')), 'Installed: 2.1.310 (Claude Code)');

  answer(probeOf({ loggedIn: true }, { version: '2.1.320 (Claude Code)', update: '2.2.0' }));
  assert.equal((await checking).state, 'signedIn');
  assert.ok(!cantRun(connectorAuthState('k1')));
  assert.equal(
    versionLine(connectorAuthState('k1')),
    'Installed: 2.1.320 (Claude Code) · 2.2.0 is available'
  );
  resetStartCheck();
});

test('a test’s state is what’s installed and signed in, not the request it made', () => {
  const state = authStateOf({
    probe: probeOf(
      { loggedIn: true, method: 'claude.ai' },
      { request: { model: 'haiku', inputTokens: 9, outputTokens: 1, ms: 900 } }
    ),
  });
  assert.equal(state.state, 'signedIn');
  assert.equal(state.probe?.request, null);
  assert.equal(state.probe?.version, '2.1.310 (Claude Code)');
});

test('status lines for the rows', () => {
  assert.equal(authLine(undefined), null);
  assert.deepEqual(authLine({ state: 'checking' }), { text: 'Checking sign-in…', warning: false });
  assert.deepEqual(authLine({ state: 'signedIn', auth: { loggedIn: true, method: 'claude.ai' } }), {
    text: 'Signed in (claude.ai login)',
    warning: false,
  });
  assert.deepEqual(
    authLine({
      state: 'signedIn',
      auth: { loggedIn: true, method: 'oauth_token', detail: 'The token is taken as good.' },
    }),
    { text: 'Signed in (token). The token is taken as good.', warning: false }
  );
  assert.deepEqual(authLine({ state: 'signedOut', auth: { loggedIn: false } }), {
    text: 'Not signed in.',
    warning: true,
  });
  assert.deepEqual(authLine(authStateOf({ error: new Error('timed out') })), {
    text: 'Sign-in unknown: timed out',
    warning: false,
  });
  // The version: none before a test answered.
  assert.equal(versionLine(undefined), null);
  assert.equal(versionLine({ state: 'checking' }), null);
  assert.equal(versionLine(authStateOf({ error: new Error('timed out') })), null);
  assert.equal(
    versionLine(authStateOf({ probe: probeOf({ loggedIn: true }, { version: '' }) })),
    'Installed: /home/me/.local/bin/claude'
  );
});

test('with Rovik’s tools, a connector’s model is offered the run’s tools', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1' };
  claude.models = [{ id: 'sonnet', enabled: true }];
  assert.equal(claudeCodeSettings(claude).tools, 'own', 'its own tools by default');
  assert.equal(resolveProfile([claude], { providerId: 'c1', modelId: 'sonnet' })?.noTools, true);
  assert.ok(!usesRovikTools(claude));
  claude.settings = { ...CLAUDE_CODE_DEFAULTS, tools: 'rovik' };
  assert.ok(usesRovikTools(claude));
  const profile = resolveProfile([claude], { providerId: 'c1', modelId: 'sonnet' });
  assert.equal(profile?.noTools, false, 'the run offers its tools');
  assert.equal((profile?.connector as { tools?: string }).tools, 'rovik');
  assert.ok(!usesRovikTools(openai), 'a provider is no connector');
});

test('the choices’ tooltips have no markup characters, which WebKitGTK can’t show', () => {
  const choices = [...AUTH_KINDS, ...PERMISSION_MODES, ...USE_MODES];
  const tips = [...choices.flatMap((c) => [c.hint, c.detail ?? '']), ...EFFORTS.map((c) => c.hint)];
  for (const tip of tips) assert.doesNotMatch(tip, /[&<>]/, tip);
});

test('the context shown is the one the latest connector reply reported', () => {
  const reply = (context?: { used: number; window: number }) => ({
    role: 'assistant',
    meta: { connector: { kind: 'claude-code', usage: { connector: context ? { context } : {} } } },
  });
  assert.equal(latestConnectorContext([]), null);
  assert.equal(latestConnectorContext([{ role: 'user' }, reply()]), null);
  // A later reply that didn't report one (a stopped run) leaves the last one shown.
  assert.deepEqual(
    latestConnectorContext([
      reply({ used: 10, window: 100 }),
      reply({ used: 40, window: 100 }),
      { role: 'user' },
      reply(),
    ]),
    { kind: 'claude-code', context: { used: 40, window: 100 } }
  );
  assert.equal(latestConnectorContext([reply({ used: 1, window: 0 })]), null);
});
