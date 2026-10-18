// Autopilot's settings (lib/autopilotSettings.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  autopilotConfig,
  budgetOf,
  changedBudget,
  DEFAULT_AUTOPILOT_DENYLIST,
  DEFAULT_BUDGETS,
  DEFAULT_AUTOPILOT_SETTINGS,
  denylistEntry,
  denylistOf,
  normalizeAutopilotSettings,
} from './autopilotSettings';
import { DEFAULT_SUBAGENT_SETTINGS, normalizeSubagentSettings } from './subagentSettings';

test('autopilot settings: Balanced by default, and a saved effort loads back', () => {
  assert.deepEqual(DEFAULT_AUTOPILOT_SETTINGS, { effort: 'balanced' });
  assert.deepEqual(normalizeAutopilotSettings(undefined), DEFAULT_AUTOPILOT_SETTINGS);
  assert.deepEqual(normalizeAutopilotSettings({ effort: 'thorough' }), { effort: 'thorough' });
  // Garbage, or an effort this version doesn't know: the default.
  assert.deepEqual(normalizeAutopilotSettings({ effort: 'reckless' }), DEFAULT_AUTOPILOT_SETTINGS);
  assert.deepEqual(normalizeAutopilotSettings('thorough'), DEFAULT_AUTOPILOT_SETTINGS);
});

test('autopilot settings: saved with the Subagents page’s, a file without them loads', () => {
  // Written before Autopilot existed.
  const old = { ...DEFAULT_SUBAGENT_SETTINGS, enabled: true } as Record<string, unknown>;
  delete old.autopilot;
  assert.deepEqual(normalizeSubagentSettings(old).autopilot, DEFAULT_AUTOPILOT_SETTINGS);
  const saved = { ...DEFAULT_SUBAGENT_SETTINGS, autopilot: { effort: 'thorough' as const } };
  assert.deepEqual(normalizeSubagentSettings(JSON.parse(JSON.stringify(saved))), saved);
});

test('autopilot settings: the backend gets the effort (AgentConfig.autopilot)', () => {
  assert.deepEqual(autopilotConfig({ effort: 'thorough' }), {
    effort: 'thorough',
    budget: { maxHours: 6, maxCost: null },
    prepare: true,
  });
  assert.deepEqual(autopilotConfig(DEFAULT_AUTOPILOT_SETTINGS), {
    effort: 'balanced',
    budget: { maxHours: 2, maxCost: 5 },
    prepare: true,
  });
});

test('autopilot settings: Prepare is on until it’s turned off (S03)', () => {
  assert.equal(autopilotConfig(DEFAULT_AUTOPILOT_SETTINGS).prepare, true);
  const off = normalizeAutopilotSettings({ effort: 'balanced', prepare: false });
  assert.deepEqual(off, { effort: 'balanced', prepare: false });
  assert.equal(autopilotConfig(off).prepare, false);
  // On, or garbage: the default, kept as nothing.
  for (const on of [true, 'false', 0, null]) {
    assert.deepEqual(normalizeAutopilotSettings({ prepare: on }), DEFAULT_AUTOPILOT_SETTINGS);
  }
  // The backend's field, and its default.
  const rust = readFileSync('src-tauri/crates/rovik-protocol/src/autopilot.rs', 'utf8');
  assert.match(rust, /pub prepare: bool,/);
  assert.match(rust, /prepare: true,/);
});

test('autopilot settings: the default denylist is the backend’s', () => {
  // Read from the project's folder, as npm test runs.
  const rust = readFileSync('src-tauri/crates/rovik-protocol/src/autopilot.rs', 'utf8');
  const list = /DEFAULT_AUTOPILOT_DENYLIST: &\[&str\] = &\[([\s\S]*?)\];/.exec(rust);
  assert.ok(list, 'the constant is there');
  const entries = [...list[1].matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  assert.deepEqual(DEFAULT_AUTOPILOT_DENYLIST, entries);
});

test('autopilot settings: a denylist entry is kept as its words, a pipe’s sides joined', () => {
  assert.equal(denylistEntry('  git   push  '), 'git push');
  assert.equal(denylistEntry('curl|sh'), 'curl | sh');
  assert.equal(denylistEntry(' wget -qO-  |  bash '), 'wget -qO- | bash');
  for (const nothing of ['', '   ', 'curl |', '| sh', 'a || b']) {
    assert.equal(denylistEntry(nothing), '', JSON.stringify(nothing));
  }
});

test('autopilot settings: the denylist is the defaults until it’s changed, then the user’s', () => {
  assert.deepEqual(denylistOf(DEFAULT_AUTOPILOT_SETTINGS), DEFAULT_AUTOPILOT_DENYLIST);
  assert.equal(autopilotConfig(DEFAULT_AUTOPILOT_SETTINGS).deniedCommands, undefined);
  const saved = normalizeAutopilotSettings({
    effort: 'balanced',
    deniedCommands: ['npm  publish', 'curl|sh', 'npm publish', 7, '', 'x |'],
  });
  assert.deepEqual(saved.deniedCommands, ['npm publish', 'curl | sh']);
  assert.deepEqual(denylistOf(saved), ['npm publish', 'curl | sh']);
  assert.deepEqual(autopilotConfig(saved).deniedCommands, ['npm publish', 'curl | sh']);
  // Every entry taken off: none, which isn't the defaults.
  const none = normalizeAutopilotSettings({ deniedCommands: [] });
  assert.deepEqual(denylistOf(none), []);
  assert.deepEqual(autopilotConfig(none).deniedCommands, []);
  assert.equal(normalizeAutopilotSettings({ deniedCommands: 'sudo' }).deniedCommands, undefined);
});

test('autopilot settings: a task’s budget, its effort’s until it’s set (U04)', () => {
  assert.deepEqual(budgetOf(DEFAULT_AUTOPILOT_SETTINGS), { maxHours: 2, maxCost: 5 });
  // Thorough's: 6 hours, a limit kept whatever the provider reports.
  assert.deepEqual(budgetOf({ effort: 'thorough' }), { maxHours: 6, maxCost: null });
  // Set: it stays whatever the effort.
  const mine = { effort: 'thorough' as const, budget: { maxHours: 0.5, maxCost: null } };
  assert.deepEqual(budgetOf(mine), { maxHours: 0.5, maxCost: null });
  assert.deepEqual(autopilotConfig(mine).budget, { maxHours: 0.5, maxCost: null });
  // Saved, garbage aside.
  const saved = normalizeAutopilotSettings({
    effort: 'balanced',
    budget: { maxHours: 3, maxCost: -1 },
  });
  assert.deepEqual(saved.budget, { maxHours: 3, maxCost: null });
  assert.deepEqual(
    normalizeAutopilotSettings({ budget: { maxHours: 'x', maxCost: Infinity } }).budget,
    { maxHours: null, maxCost: null }
  );
  assert.equal(normalizeAutopilotSettings({ budget: 7 }).budget, undefined);
  // A token limit: kept in whole tokens, only when it's one.
  assert.deepEqual(
    normalizeAutopilotSettings({ budget: { maxHours: null, maxCost: null, maxTokens: 2.5e6 } })
      .budget,
    { maxHours: null, maxCost: null, maxTokens: 2_500_000 }
  );
  assert.deepEqual(
    normalizeAutopilotSettings({ budget: { maxHours: 1, maxCost: null, maxTokens: 0 } }).budget,
    { maxHours: 1, maxCost: null }
  );
});

test('autopilot settings: a budget changed back to its effort’s own is left unset', () => {
  const balanced = DEFAULT_BUDGETS.balanced;
  assert.deepEqual(changedBudget(balanced, { maxTokens: 3_000_000 }, balanced), {
    maxHours: 2,
    maxCost: 5,
    maxTokens: 3_000_000,
  });
  // Emptied: no token limit kept, and the rest is the effort's own.
  const set = { ...balanced, maxTokens: 3_000_000 };
  assert.equal(changedBudget(set, { maxTokens: null }, balanced), undefined);
  assert.deepEqual(changedBudget(balanced, { maxHours: 1 }, balanced), {
    maxHours: 1,
    maxCost: 5,
  });
});
