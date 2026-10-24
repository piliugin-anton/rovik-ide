// Component test: Claude Code's system prompt setting. Its own prompt,
// unchanged, is the default; with Rovik's tools it isn't offered, since
// Claude Code then has no tools of its own for it to describe.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ClaudeCodeFields from './ClaudeCodeFields';
import { newConnector } from '../lib/connectors';
import type { Provider } from '../types';

/** The system prompt selector's options and its value, for `connector`. */
function promptSelect(connector: Provider): { options: string[]; value: string } {
  const root = container();
  const dispose = render(() => <ClaudeCodeFields connector={connector} update={() => {}} />, root);
  const select = [...root.querySelectorAll('select')].find((s) =>
    [...s.options].some((o) => o.value === 'replace')
  )!;
  const result = { options: [...select.options].map((o) => o.value), value: select.value };
  dispose();
  return result;
}

test('Claude Code’s own prompt is the default, offered with its own tools', () => {
  const connector = newConnector('claude-code');
  assert.deepEqual(promptSelect(connector), {
    options: ['own', 'append', 'replace'],
    value: 'own',
  });
});

test('with Rovik’s tools, Claude Code’s own prompt isn’t offered', () => {
  const connector = newConnector('claude-code');
  connector.settings = { ...connector.settings, tools: 'rovik', systemPrompt: 'replace' };
  assert.deepEqual(promptSelect(connector), { options: ['append', 'replace'], value: 'replace' });
});
