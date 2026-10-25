// Component test: the chat's lines for a command the agent left running
// (bash's `background`) and for the background tool that reads and stops
// one: a start says it runs in the background, and as which task, not that
// it was killed; one that ended at once says how. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ToolCallBlock, { toolResultMeta } from './ToolCallBlock';
import type { ChatMessage, ChatToolCall } from '../types';

const call = (name: string, args: Record<string, unknown>): ChatToolCall => ({
  id: `${name}-call`,
  name,
  arguments: JSON.stringify(args),
});

const result = (content: string): ChatMessage => ({
  id: 'result',
  role: 'tool',
  content,
  tool_call_id: 'call',
});

/** The block's header: its title and its muted detail. */
function header(toolCall: ChatToolCall, content: string, running = false) {
  const root = container();
  const dispose = render(
    () => (
      <ToolCallBlock
        toolCall={toolCall}
        dialogId="chat"
        result={running ? undefined : result(content)}
        running={running}
      />
    ),
    root
  );
  const read = [
    root.querySelector('.tool-call-title')?.textContent,
    root.querySelector('.tool-call-meta')?.textContent,
  ];
  dispose();
  return read;
}

const started =
  '$ npm run dev\nrunning in the background as bg2, 3.0 s\nVITE ready\n(It keeps running. …)';

test('a start in the background reads as running, as its task, not as killed', () => {
  assert.deepEqual(toolResultMeta('bash', started), { exitCode: null, background: 'bg2' });
  const dev = call('bash', { command: 'npm run dev', background: true });
  assert.deepEqual(header(dev, started), [
    'Started `npm run dev` in the background',
    'running as bg2',
  ]);
  assert.deepEqual(header(dev, '', true)[0], 'Starting `npm run dev` in the background');
  // It failed at once: its exit code, as any command's.
  const failed = '$ npm run dev\nexit code 1, 0.4 s\nport 5173 is taken';
  assert.deepEqual(toolResultMeta('bash', failed), { exitCode: 1 });
  assert.deepEqual(header(dev, failed), ['Started `npm run dev` in the background', 'exit 1']);
  // A command run the usual way reads as before.
  assert.deepEqual(
    header(call('bash', { command: 'npm test' }), '$ npm test\nexit code 0, 2.0 s'),
    ['Ran `npm test`', 'ok']
  );
});

test('the background tool says which task it read or stopped, and how it is', () => {
  const read = call('background', { action: 'output', id: 'bg2' });
  assert.deepEqual(header(read, started), ['Read what bg2 printed', 'running']);
  const crashed = '$ npm run dev\nexit code 1, 65.0 s\nError: boom';
  assert.deepEqual(header(read, crashed), ['Read what bg2 printed', 'ended, exit 1']);
  const stop = call('background', { action: 'stop', id: 'bg2' });
  assert.deepEqual(header(stop, '$ npm run dev\nstopped (killed), 70.0 s\nbye'), [
    'Stopped bg2',
    undefined,
  ]);
  const list = call('background', { action: 'list' });
  assert.deepEqual(header(list, 'bg2: `npm run dev`, running for 70 s'), [
    'Listed the background tasks',
    undefined,
  ]);
  assert.equal(toolResultMeta('background', 'No command runs in the background.'), undefined);
});
