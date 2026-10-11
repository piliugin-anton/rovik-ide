import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ModelProfile, ModelSelection, Provider } from '../types';
import {
  DEFAULT_SUBAGENT_SETTINGS,
  kindChain,
  normalizeSubagentSettings,
  otherProviders,
  resolveSubagentConfig,
  subagentSummary,
  type SubagentSettings,
} from './subagentSettings';

const providers: Provider[] = [
  {
    id: 'a',
    name: 'Alpha',
    baseUrl: 'http://a',
    apiKey: '',
    stream: true,
    models: [
      { id: 'cheap', enabled: true },
      { id: 'strong', enabled: true },
      { id: 'off', enabled: false },
      { id: 'mute', enabled: true, noTools: true },
    ],
  },
  {
    id: 'b',
    name: 'Beta',
    baseUrl: 'http://b',
    apiKey: '',
    stream: true,
    models: [{ id: 'other', enabled: true, reasoning: { choice: 'reasoning_effort' } }],
  },
] as Provider[];

const sel = (providerId: string, modelId: string): ModelSelection => ({ providerId, modelId });

const resolve = (ps: Provider[], s: ModelSelection): ModelProfile | null => {
  const model = ps.find((p) => p.id === s.providerId)?.models.find((m) => m.id === s.modelId);
  if (!model) return null;
  return {
    id: `${s.providerId}::${s.modelId}`,
    label: s.modelId,
    model: s.modelId,
    baseUrl: '',
    apiKey: '',
    stream: true,
    vision: false,
    noTools: !!model.noTools,
    reasoning: model.reasoning ?? { choice: 'auto' },
  };
};

const ids = (models: ({ id: string } | { chat: true })[]) =>
  models.map((m) => ('chat' in m ? 'chat' : m.id));

test('defaults: everything off, chats work as before', () => {
  const s = normalizeSubagentSettings(undefined);
  assert.deepEqual(s, DEFAULT_SUBAGENT_SETTINGS);
  assert.equal(s.enabled, false);
  assert.equal(s.kinds.scout.enabled, false);
  assert.equal(subagentSummary(s), 'Off: the agent works alone');
  const config = resolveSubagentConfig(s, providers, resolve);
  assert.equal(config.enabled, false);
  assert.deepEqual(ids(config.kinds.scout!.models), ['chat']);
  assert.equal(config.kinds.reviewer!.enabled, false);
  assert.equal(config.reviewMinLines, 10);
  assert.equal(config.workerMinSteps, 6);
});

test('saved settings load back, and garbage is dropped', () => {
  const saved: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    defaultModel: sel('a', 'cheap'),
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: sel('a', 'strong'),
        fallbacks: [sel('a', 'cheap')],
        effort: 'low',
        maxToolCalls: 10,
      },
    },
  };
  assert.deepEqual(normalizeSubagentSettings(JSON.parse(JSON.stringify(saved))), saved);
  const odd = normalizeSubagentSettings({
    enabled: 'yes',
    maxAtOnce: 99,
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: { providerId: 'a', modelId: 'strong' },
        fallbacks: [sel('a', 'strong'), 'x', sel('a', 'cheap'), sel('a', 'cheap')],
        effort: 'extreme',
        maxToolCalls: -3,
      },
    },
  });
  assert.equal(odd.enabled, false);
  assert.equal(odd.maxAtOnce, 8);
  assert.deepEqual(odd.kinds.scout.fallbacks, [sel('a', 'cheap')], 'not itself, once each');
  assert.equal(odd.kinds.scout.effort, 'default');
  assert.equal(odd.kinds.scout.maxToolCalls, 1);
});

test('a kind tries its model, its fallbacks, the default model, then the chat', () => {
  const s: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    defaultModel: sel('a', 'cheap'),
    defaultFallbacks: [sel('b', 'other')],
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: sel('a', 'strong'),
        fallbacks: [sel('a', 'off'), sel('a', 'mute')],
        effort: 'default',
      },
    },
  };
  assert.deepEqual(kindChain(s, 'scout'), [
    sel('a', 'strong'),
    sel('a', 'off'),
    sel('a', 'mute'),
    sel('a', 'cheap'),
    sel('b', 'other'),
    'chat',
  ]);
  const config = resolveSubagentConfig(s, providers, resolve);
  assert.deepEqual(
    ids(config.kinds.scout!.models),
    ['a::strong', 'a::cheap', 'b::other', 'chat'],
    'turned-off and no-tools models are skipped'
  );

  // "Default": the default model's chain, then the chat's.
  const byDefault = {
    ...s,
    kinds: { ...s.kinds, scout: { ...s.kinds.scout, model: 'default' as const, fallbacks: [] } },
  };
  assert.deepEqual(ids(resolveSubagentConfig(byDefault, providers, resolve).kinds.scout!.models), [
    'a::cheap',
    'b::other',
    'chat',
  ]);
  // A removed model falls through to the next.
  const removed = { ...byDefault, defaultModel: sel('gone', 'x') };
  assert.deepEqual(ids(resolveSubagentConfig(removed, providers, resolve).kinds.scout!.models), [
    'b::other',
    'chat',
  ]);
});

test("a kind's effort goes in the forms its first model takes", () => {
  const s: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: sel('b', 'other'),
        fallbacks: [],
        effort: 'high',
        maxMinutes: 2,
      },
    },
  };
  const scout = resolveSubagentConfig(s, providers, resolve).kinds.scout!;
  assert.deepEqual(scout.options, {
    reasoning_effort: 'high',
    reasoning_formats: ['reasoning_effort'],
  });
  assert.equal(scout.maxMinutes, 2);
  const chat = { ...s, kinds: { ...s.kinds, scout: { ...s.kinds.scout, model: 'chat' as const } } };
  assert.deepEqual(resolveSubagentConfig(chat, providers, resolve).kinds.scout!.options, {
    reasoning_effort: 'high',
  });
});

test('models from other providers than the chat’s are named (K18)', () => {
  const s: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: sel('b', 'other'),
        fallbacks: [sel('a', 'cheap')],
        effort: 'default',
      },
    },
  };
  assert.deepEqual(otherProviders(providers, s, 'scout', sel('a', 'strong')), ['Beta']);
  assert.deepEqual(otherProviders(providers, s, 'scout', sel('b', 'other')), ['Alpha']);
  assert.equal(subagentSummary(s), 'On: Scout');
});
