import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ModelProfile, ModelSelection, Provider } from '../types';
import {
  DEFAULT_SUBAGENT_SETTINGS,
  kindChain,
  normalizeSubagentSettings,
  JUDGING_KINDS,
  numberFieldValue,
  otherProviderModel,
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
    pdf: false,
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
  // P5.2: "Default" with fallbacks of its own: the default model, its
  // own fallbacks, then the default model's.
  const ownFallbacks = {
    ...s,
    kinds: {
      ...s.kinds,
      scout: { ...s.kinds.scout, model: 'default' as const, fallbacks: [sel('a', 'strong')] },
    },
  };
  assert.deepEqual(kindChain(ownFallbacks, 'scout'), [
    sel('a', 'cheap'),
    sel('a', 'strong'),
    sel('b', 'other'),
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

test('parallel groups, the red check and parallel repair are off until turned on', () => {
  const s = normalizeSubagentSettings(undefined);
  assert.equal(s.parallelWorkers, false);
  assert.equal(s.redCheck, false);
  assert.equal(s.parallelRepair, false);
  const on = normalizeSubagentSettings({
    parallelWorkers: true,
    redCheck: true,
    parallelRepair: true,
    workerMinSteps: 4,
  });
  assert.equal(on.parallelWorkers, true);
  assert.equal(on.redCheck, true);
  assert.equal(on.parallelRepair, true);
  const config = resolveSubagentConfig(on, providers, resolve);
  assert.equal(config.parallelWorkers, true);
  assert.equal(config.redCheck, true);
  assert.equal(config.parallelRepair, true);
  assert.equal(config.workerMinSteps, 4);
  assert.equal(normalizeSubagentSettings({ parallelWorkers: 'yes' }).parallelWorkers, false);
});

test('a number field saves what it shows: unset, clamped, whole', () => {
  assert.equal(numberFieldValue('', 1, 10), undefined);
  assert.equal(numberFieldValue('abc', 1, 10), undefined);
  assert.equal(numberFieldValue(' 999 ', 1, 10), 10);
  assert.equal(numberFieldValue('-3', 1, 10), 1);
  assert.equal(numberFieldValue('2.6', 1, 10), 3);
  assert.equal(numberFieldValue('2.6', 0, 10, 0.5), 2.6);
});

test('default_with_own_fallbacks_and_disabled_model', () => {
  const s: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    defaultModel: sel('a', 'cheap'),
    defaultFallbacks: [sel('b', 'other'), sel('a', 'strong')],
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: {
        enabled: true,
        model: 'default',
        // Its own, repeating the default model and one of its fallbacks.
        fallbacks: [sel('a', 'strong'), sel('a', 'cheap'), sel('a', 'off')],
        effort: 'default',
      },
    },
  };
  assert.deepEqual(kindChain(s, 'scout'), [
    sel('a', 'cheap'),
    sel('a', 'strong'),
    sel('a', 'off'),
    sel('b', 'other'),
    'chat',
  ]);
  assert.deepEqual(
    ids(resolveSubagentConfig(s, providers, resolve).kinds.scout!.models),
    ['a::cheap', 'a::strong', 'b::other', 'chat'],
    'deduplicated; the turned-off model is left out when resolved'
  );
});

test('default_with_no_own_fallbacks', () => {
  const s: SubagentSettings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    enabled: true,
    defaultModel: sel('a', 'strong'),
    defaultFallbacks: [sel('b', 'other')],
    kinds: {
      ...DEFAULT_SUBAGENT_SETTINGS.kinds,
      scout: { enabled: true, model: 'default', fallbacks: [], effort: 'default' },
    },
  };
  assert.deepEqual(kindChain(s, 'scout'), [sel('a', 'strong'), sel('b', 'other'), 'chat']);
  // The default model set to the chat's: just the chat.
  const chat = { ...s, defaultModel: 'chat' as const, defaultFallbacks: [] };
  assert.deepEqual(kindChain(chat, 'scout'), ['chat']);
});

test('limits_clamp_table: each limit is kept whole and within its range', () => {
  const table: [string, unknown, number | undefined][] = [
    ['maxPerRun', 0, 1],
    ['maxPerRun', 99, 50],
    ['maxPerRun', 7.4, 7],
    ['maxPerRun', 'x', 12],
    ['maxPerRun', undefined, 12],
    ['reviewMinLines', -5, 1],
    ['reviewMinLines', 20_000, 10_000],
    ['reviewMinLines', 2.5, 3],
    ['reviewMinLines', undefined, 10],
    ['workerMinSteps', 0.2, 1],
    ['workerMinSteps', 500, 100],
    ['workerMinSteps', NaN, 6],
    ['workerMinSteps', undefined, 6],
    ['maxRunTokens', 10, 1000],
    ['maxRunTokens', 1e9, 1e8],
    ['maxRunTokens', 2500.6, 2501],
    ['maxRunTokens', '5000', undefined],
    ['maxRunTokens', undefined, undefined],
  ];
  for (const [field, raw, expected] of table) {
    const s = normalizeSubagentSettings({ [field]: raw }) as unknown as Record<string, unknown>;
    assert.equal(s[field], expected, `${field}: ${String(raw)}`);
    assert.equal(field in s, expected !== undefined, `${field} present: ${String(raw)}`);
  }
  // A kind's budget: tool calls whole, minutes as given (a quarter minute is fine).
  const kind = (k: Record<string, unknown>) =>
    normalizeSubagentSettings({ kinds: { scout: k } }).kinds.scout;
  assert.equal(kind({ maxMinutes: 0.1 }).maxMinutes, 0.25);
  assert.equal(kind({ maxMinutes: 2.5 }).maxMinutes, 2.5);
  assert.equal(kind({ maxMinutes: 999 }).maxMinutes, 120);
  assert.equal('maxMinutes' in kind({}), false);
  assert.equal(kind({ maxToolCalls: 2.6 }).maxToolCalls, 3);
  assert.equal(kind({ maxToolCalls: 999 }).maxToolCalls, 200);
  assert.equal('maxToolCalls' in kind({ maxToolCalls: 'many' }), false);
});

test('number_field_value_table', () => {
  const table: [Parameters<typeof numberFieldValue>, number | undefined][] = [
    [['500', 1000, 1e8], 1000],
    [['', 1000, 1e8], undefined],
    [['abc', 1000, 1e8], undefined],
    [['1e9', 1000, 1e8], 1e8],
    [['2.5', 1, 8], 3],
    [['2.5', 1, 8, 0.5], 2.5],
    [['  7 ', 1, 8], 7],
    [['Infinity', 1, 8], undefined],
  ];
  for (const [args, expected] of table)
    assert.equal(numberFieldValue(...args), expected, JSON.stringify(args));
});

test('judging rows left on Default start on another provider than the chat’s (T09)', () => {
  const chat = sel('a', 'cheap');
  const s: SubagentSettings = { ...DEFAULT_SUBAGENT_SETTINGS, enabled: true };
  assert.deepEqual(otherProviderModel(providers, chat), sel('b', 'other'));
  assert.equal(otherProviderModel(providers, null), null, 'no chat model known');
  // Two providers: the judging rows resolve to the other one first.
  const config = resolveSubagentConfig(s, providers, resolve, chat);
  for (const kind of JUDGING_KINDS) {
    assert.deepEqual(ids(config.kinds[kind]!.models), ['b::other', 'chat'], kind);
  }
  // The others keep their chain.
  assert.deepEqual(ids(config.kinds.reviewer!.models), ['chat']);
  assert.deepEqual(otherProviders(providers, s, 'judge', chat), ['Beta']);
  assert.deepEqual(otherProviders(providers, s, 'reviewer', chat), []);
  // One provider: the default.
  const one = providers.slice(0, 1);
  assert.deepEqual(ids(resolveSubagentConfig(s, one, resolve, chat).kinds.judge!.models), ['chat']);
  // The chat on b: a's first usable model.
  assert.deepEqual(
    ids(resolveSubagentConfig(s, providers, resolve, sel('b', 'other')).kinds.planner!.models),
    ['a::cheap', 'chat']
  );
  // A default model of another provider already holds; one of the chat's
  // provider comes after the other provider's.
  const otherDefault = { ...s, defaultModel: sel('b', 'other') };
  assert.deepEqual(
    ids(resolveSubagentConfig(otherDefault, providers, resolve, chat).kinds.acceptance!.models),
    ['b::other', 'chat']
  );
  const sameDefault = { ...s, defaultModel: sel('a', 'strong') };
  assert.deepEqual(
    ids(resolveSubagentConfig(sameDefault, providers, resolve, chat).kinds.breaker!.models),
    ['b::other', 'a::strong', 'chat']
  );
  // A row set to a model of its own keeps it.
  const own: SubagentSettings = {
    ...s,
    kinds: {
      ...s.kinds,
      judge: { ...s.kinds.judge, model: sel('a', 'strong') },
    },
  };
  assert.deepEqual(ids(resolveSubagentConfig(own, providers, resolve, chat).kinds.judge!.models), [
    'a::strong',
    'chat',
  ]);
  // Without the chat's model, nothing changes.
  assert.deepEqual(ids(resolveSubagentConfig(s, providers, resolve).kinds.judge!.models), ['chat']);
});
