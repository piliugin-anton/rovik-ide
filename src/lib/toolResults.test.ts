// toolResults.ts tests (node:test). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ChatMessage } from '../types';
import { indexToolResults } from './toolResults';

const call = (id: string, name = 'read') => ({ id, name, arguments: '{}' });

test('a call id reused in a later reply gets its own result', () => {
  const first = call('call_0');
  const second = call('call_0', 'task');
  const messages: ChatMessage[] = [
    { id: 'a1', role: 'assistant', content: '', toolCalls: [first] },
    { id: 't1', role: 'tool', content: 'one', tool_call_id: 'call_0' },
    { id: 'a2', role: 'assistant', content: '', toolCalls: [second] },
    { id: 't2', role: 'tool', content: 'two', tool_call_id: 'call_0' },
  ];
  const index = indexToolResults(messages);
  assert.equal(index.results.get(first)?.id, 't1');
  assert.equal(index.results.get(second)?.id, 't2');
  assert.equal(index.latest.get('call_0'), second);

  // The later call still running: no result, and only it is live.
  const running = indexToolResults(messages.slice(0, 3));
  assert.equal(running.results.get(first)?.id, 't1');
  assert.equal(running.results.get(second), undefined);
  assert.equal(running.latest.get('call_0'), second);
});
