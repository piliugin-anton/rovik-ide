// Connectors (lib/connectors.ts): how a connector is told from a provider,
// resolved for a request, summarized, and how its reply's record reads.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Provider } from '../types';
import {
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
} from './connectors';
import { providersSummary, resolveProfile } from './aiSettings';

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
