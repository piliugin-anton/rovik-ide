// Which chat modes a model can run (lib/chatMode.ts `modeUnavailable`): the
// same rules the core refuses a start by (agent_run `mode_refusal`).
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { modeUnavailable } from './chatMode';
import { CONNECTOR_CATALOG } from './connectors';

const claudeOwn = { baseUrl: 'connector://claude-code', noTools: true };
const claudeRovik = { baseUrl: 'connector://claude-code', noTools: false };
const plainModel = { baseUrl: 'https://api.example.com/v1', noTools: true };
const toolModel = { baseUrl: 'https://api.example.com/v1', noTools: false };

test('every mode runs on a model that uses Rovik’s tools', () => {
  for (const profile of [toolModel, claudeRovik, null]) {
    for (const mode of ['agent', 'plan', 'autopilot'] as const) {
      assert.equal(modeUnavailable(mode, profile), null, `${mode}`);
    }
  }
});

test('Autopilot needs Rovik’s tools, and says where a connector gets them', () => {
  const connector = modeUnavailable('autopilot', claudeOwn);
  assert.match(connector ?? '', /set Claude Code to use them \(Settings → Connectors\)/);
  const model = modeUnavailable('autopilot', plainModel);
  assert.match(model ?? '', /this model doesn’t use/);
  assert.equal(modeUnavailable('agent', claudeOwn), null);
});

test('Plan mode on a connector running its own tools needs its own plan mode', () => {
  // Claude Code has one.
  assert.equal(modeUnavailable('plan', claudeOwn), null);
  const spec = CONNECTOR_CATALOG[0];
  const had = spec.planMode;
  spec.planMode = false;
  try {
    assert.match(modeUnavailable('plan', claudeOwn) ?? '', /has no plan mode of its own/);
    // A model without tools at all can't change anything: it can plan.
    assert.equal(modeUnavailable('plan', plainModel), null);
  } finally {
    spec.planMode = had;
  }
});
