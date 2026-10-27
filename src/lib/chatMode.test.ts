// Which chat modes a model can run (lib/chatMode.ts `modeUnavailable`): the
// same rules the core refuses a start by (agent_run `mode_refusal`).
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { modeUnavailable } from './chatMode';

const plainModel = { baseUrl: 'https://api.example.com/v1', noTools: true };
const toolModel = { baseUrl: 'https://api.example.com/v1', noTools: false };

test('every mode runs on a model that uses Rovik’s tools, and on any connector', () => {
  // A connector runs as Rovik's agent in Plan mode and Autopilot, whatever
  // its profile says of tools.
  const connectors = ['connector://claude-code', 'connector://codex'].flatMap((baseUrl) => [
    { baseUrl, noTools: false },
    { baseUrl, noTools: true },
  ]);
  for (const profile of [toolModel, ...connectors, null]) {
    for (const mode of ['agent', 'plan', 'autopilot'] as const) {
      assert.equal(modeUnavailable(mode, profile), null, `${mode} on ${profile?.baseUrl}`);
    }
  }
});

test('Autopilot needs Rovik’s tools, which a model marked “no tools” doesn’t use', () => {
  assert.match(modeUnavailable('autopilot', plainModel) ?? '', /this model doesn’t use/);
  // It changes nothing: it can plan, and answer.
  assert.equal(modeUnavailable('plan', plainModel), null);
  assert.equal(modeUnavailable('agent', plainModel), null);
});
