// The models a subagent can use (ModelWithFallbacks.tsx `modelOptions`).
// Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { modelOptions } from './ModelWithFallbacks';
import { newConnector } from '../lib/connectors';
import type { Provider } from '../types';

test('a subagent can use a connector’s model, on which it runs as Rovik’s agent', () => {
  const openai: Provider = {
    id: 'p1',
    name: 'OpenAI',
    baseUrl: 'https://api.openai.com/v1',
    apiKey: 'sk',
    stream: true,
    models: [
      { id: 'gpt-5', enabled: true },
      { id: 'mute', enabled: true, noTools: true },
    ],
  };
  const claude: Provider = {
    ...newConnector('claude-code'),
    id: 'c1',
    models: [
      { id: 'sonnet', enabled: true },
      { id: 'opus', enabled: false },
    ],
  };
  const usable = Object.fromEntries(
    modelOptions([openai, claude]).map((o) => [o.selection.modelId, o.usable])
  );
  assert.deepEqual(usable, { 'gpt-5': true, mute: false, sonnet: true, opus: false });
});
