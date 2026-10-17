// Autopilot's settings (lib/autopilotSettings.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  autopilotConfig,
  DEFAULT_AUTOPILOT_SETTINGS,
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
  assert.deepEqual(autopilotConfig({ effort: 'thorough' }), { effort: 'thorough' });
  assert.deepEqual(autopilotConfig(DEFAULT_AUTOPILOT_SETTINGS), { effort: 'balanced' });
});
