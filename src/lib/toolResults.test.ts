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

test('three_replies_reuse_function_call_1', () => {
  // Gemini numbers calls per reply: each reply's first call is function-call-1.
  const calls = [call('function-call-1'), call('function-call-1'), call('function-call-1')];
  const messages: ChatMessage[] = [
    // A tool message with no call before it (a page cut there): dropped.
    { id: 'orphan', role: 'tool', content: 'lost', tool_call_id: 'function-call-1' },
    ...calls.flatMap((c, i): ChatMessage[] => [
      { id: `a${i}`, role: 'assistant', content: '', toolCalls: [c] },
      { id: `t${i}`, role: 'tool', content: `result ${i}`, tool_call_id: 'function-call-1' },
    ]),
  ];
  const index = indexToolResults(messages);
  assert.deepEqual(
    calls.map((c) => index.results.get(c)?.id),
    ['t0', 't1', 't2'],
    'by position'
  );
  assert.equal(index.results.size, 3, 'the orphan answers no call');
  assert.equal(index.latest.get('function-call-1'), calls[2]);
});
