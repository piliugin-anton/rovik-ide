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
  checkConnectorsOnStart,
  CONNECTOR_AUTH_NOTE,
  connectorAuthState,
  resetStartCheck,
  type ConnectorAuth,
  usesRovikTools,
  claudeCodeSettings,
  CLAUDE_CODE_DEFAULTS,
  connectorHeadline,
  connectorKindOf,
  connectorsSummary,
  connectorUsage,
  isConnector,
  newConnector,
  probeModels,
  probeSummary,
  splitSources,
  stepLabel,
  titleProfile,
  CLAUDE_CODE_TITLE_MODEL,
} from './connectors';
import { providersSummary, resolveProfile } from './aiSettings';
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

test('settings take the defaults for what isn’t set', () => {
  assert.deepEqual(claudeCodeSettings({}), CLAUDE_CODE_DEFAULTS);
  const s = claudeCodeSettings({ settings: { program: '  ', permissionMode: 'plan' } });
  assert.equal(s.program, 'claude');
  assert.equal(s.permissionMode, 'plan');
  assert.equal(s.auth, 'login');
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

test('a reply’s record: steps in a few words, and a headline with what it used', () => {
  assert.equal(
    stepLabel({ id: '1', name: 'Read', input: { file_path: 'src/a.rs' } }),
    'Read src/a.rs'
  );
  assert.equal(
    stepLabel({ id: '2', name: 'Bash', input: { command: 'npm test' } }),
    'Bash npm test'
  );
  assert.equal(stepLabel({ id: '3', name: 'TodoWrite', input: { todos: [] } }), 'TodoWrite');
  assert.equal(
    stepLabel({ id: '4', name: 'Grep', input: { pattern: 'x'.repeat(100) } }),
    `Grep ${'x'.repeat(77)}…`
  );
  const meta = {
    kind: 'claude-code',
    model: 'claude-sonnet-5',
    steps: [
      { id: '1', name: 'Read', input: {} },
      { id: '2', name: 'Edit', input: {} },
    ],
    usage: {
      prompt_tokens: 12_000,
      completion_tokens: 300,
      prompt_tokens_details: { cached_tokens: 8_100 },
      cost: 0.04,
      connector: { kind: 'claude-code', numTurns: 3, model: 'claude-sonnet-5' },
    },
  };
  assert.equal(
    connectorHeadline(meta, false),
    'Claude Code · claude-sonnet-5 · 2 steps · 12.3k tokens (8.1k cached) · $0.04 · 3 turns'
  );
  assert.equal(connectorHeadline({ kind: 'claude-code' }, true), 'Claude Code · working…');
  assert.deepEqual(connectorUsage(meta), { inputTokens: 12_000, outputTokens: 300, cost: 0.04 });
  assert.equal(connectorUsage({ kind: 'claude-code' }), null);
});

/*
 * Sign-in at start (checkConnectorsOnStart)
 */

const withModel = (id: string, name: string): Provider => ({
  ...newConnector('claude-code'),
  id,
  name,
  models: [{ id: 'sonnet', enabled: true }],
});

test('at start, each connector a chat can pick is checked once, and one signed out is said', async () => {
  resetStartCheck();
  dismiss(CONNECTOR_AUTH_NOTE);
  const asked: string[] = [];
  let opened = 0;
  const signedIn = withModel('c1', 'Claude Code');
  const signedOut = withModel('c2', 'Claude Code (work)');
  const unused = { ...withModel('c3', 'Spare'), models: [{ id: 'opus', enabled: false }] };
  const check = async (p: Provider): Promise<ConnectorAuth> => {
    asked.push(p.id);
    return p.id === 'c1'
      ? { loggedIn: true, method: 'claude.ai' }
      : { loggedIn: false, method: 'none', detail: 'Not signed in: run `claude` once.' };
  };
  const found = await checkConnectorsOnStart(
    [openai, signedIn, signedOut, unused],
    () => opened++,
    check
  );
  assert.deepEqual(asked.sort(), ['c1', 'c2'], 'only connectors with an enabled model');
  assert.equal(found.c1.state, 'signedIn');
  assert.equal(found.c2.state, 'signedOut');
  assert.equal(connectorAuthState('c2')?.state, 'signedOut', 'kept for the pages');
  const note = notifications().find((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.ok(note);
  assert.equal(note.kind, 'warning');
  assert.equal(note.title, 'Claude Code (work) isn’t signed in');
  assert.match(note.message ?? '', /run `claude` once.*fail until it is/);
  note.actions![0].run();
  assert.equal(opened, 1);
  // Once per window: a second call checks nothing.
  assert.deepEqual(await checkConnectorsOnStart([signedOut], () => opened++, check), {});
  assert.equal(asked.length, 2);
  // The menu line says how many can't run.
  assert.equal(
    connectorsSummary([openai, signedIn, signedOut]),
    '2 connectors, 2 models enabled · 1 can’t run: not signed in or installed'
  );
  dismiss(CONNECTOR_AUTH_NOTE);
});

test('at start, a missing program is said; a check that can’t say is left alone', async () => {
  resetStartCheck();
  dismiss(CONNECTOR_AUTH_NOTE);
  const missing = withModel('m1', 'Claude Code');
  const old = withModel('m2', 'Old Claude');
  const check = async (p: Provider): Promise<ConnectorAuth> => {
    throw new Error(
      p.id === 'm1'
        ? "claude isn't installed, or isn't on the PATH."
        : "This Claude Code can't report its sign-in: update it."
    );
  };
  const found = await checkConnectorsOnStart([missing, old], () => {}, check);
  assert.deepEqual(found.m1, {
    state: 'unknown',
    message: "claude isn't installed, or isn't on the PATH.",
    missing: true,
  });
  assert.equal(found.m2.state, 'unknown');
  assert.ok(!cantRun(found.m2));
  const note = notifications().find((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.equal(note?.title, 'Claude Code isn’t installed');
  dismiss(CONNECTOR_AUTH_NOTE);
  // No connector a chat can pick: nothing is checked, nothing said.
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
  const out = async (): Promise<ConnectorAuth> => ({ loggedIn: false, detail: 'Not signed in.' });
  await checkConnectorsOnStart([withModel('a', 'A'), withModel('b', 'B')], () => {}, out);
  const notes = notifications().filter((n) => n.id === CONNECTOR_AUTH_NOTE);
  assert.equal(notes.length, 1);
  assert.equal(notes[0].title, '2 connectors can’t run');
  assert.equal(notes[0].message, 'A: Not signed in.\nB: Not signed in.');
  dismiss(CONNECTOR_AUTH_NOTE);
  resetStartCheck();
});

test('sign-in lines for the rows', () => {
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
});

test('with Rovik’s tools, a connector’s model is offered the run’s tools', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1' };
  claude.models = [{ id: 'sonnet', enabled: true }];
  assert.equal(claudeCodeSettings(claude).tools, 'own', 'its own tools by default');
  assert.equal(resolveProfile([claude], { providerId: 'c1', modelId: 'sonnet' })?.noTools, true);
  assert.ok(!usesRovikTools(claude));
  claude.settings = { ...CLAUDE_CODE_DEFAULTS, tools: 'rovik', systemPrompt: 'replace' };
  assert.ok(usesRovikTools(claude));
  const profile = resolveProfile([claude], { providerId: 'c1', modelId: 'sonnet' });
  assert.equal(profile?.noTools, false, 'the run offers its tools');
  assert.equal((profile?.connector as { tools?: string }).tools, 'rovik');
  assert.ok(!usesRovikTools(openai), 'a provider is no connector');
});

test('with Rovik’s tools, a reply’s record says what it used, its run’s cost on the last', () => {
  assert.equal(connectorHeadline({ kind: 'claude-code', rovikTools: true }, true), 'Claude Code');
  assert.equal(
    connectorHeadline(
      {
        kind: 'claude-code',
        rovikTools: true,
        usage: {
          prompt_tokens: 2035,
          completion_tokens: 5,
          prompt_tokens_details: { cached_tokens: 1936 },
          cost: 0.00045,
          connector: { model: 'claude-haiku-5-5', costCovers: 'harnessTurn', numTurns: 2 },
        },
      },
      false
    ),
    'Claude Code · claude-haiku-5-5 · 2.0k tokens (1.9k cached) · $0.0004 for its run · 2 turns'
  );
});
