// Component test: a tool call line keeps its elements while the chat
// streams on. Its label is worked out again on every token; drawn anew
// each time, the file name under the pointer was replaced between press
// and release, so clicking it (or the line) did nothing while the model
// was thinking or writing. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import ToolCallGroup from './ToolCallGroup';
import { onOpenFileRequest, type OpenFileRequest } from '../lib/editorBridge';
import { sameGroupLabel } from '../lib/toolGroups';
import type { ChatMessage, ChatToolCall } from '../types';

const calls: ChatToolCall[] = ['src/a.ts', 'src/b.ts'].map((path, i) => ({
  id: `read-${i}`,
  name: 'read',
  arguments: JSON.stringify({ path }),
}));

test('streaming elsewhere in the chat leaves the line’s file names in place, clickable', () => {
  const [tokens, setTokens] = createSignal(0);
  const root = container();
  const dispose = render(
    () => (
      <ToolCallGroup
        name="read"
        calls={calls}
        dialogId="chat"
        // As the chat's lookup does: the results are read from messages
        // that change on every token, and come back as new objects.
        result={(call): ChatMessage => {
          tokens();
          return { id: `r-${call.id}`, role: 'tool', content: 'file text', tool_call_id: call.id };
        }}
        running
        renderCall={() => <div />}
      />
    ),
    root
  );
  const files = () => [...root.querySelectorAll('.tool-group-file')];
  const before = files();
  assert.deepEqual(
    before.map((b) => b.textContent),
    ['a.ts', 'b.ts']
  );
  for (let i = 1; i <= 20; i++) setTokens(i);
  const after = files();
  assert.equal(after.length, 2);
  assert.ok(
    after.every((b, i) => b === before[i]),
    'the same buttons, not new ones'
  );
  const opened: OpenFileRequest[] = [];
  const stop = onOpenFileRequest((r) => opened.push(r));
  (after[1] as HTMLButtonElement).click();
  assert.deepEqual(opened, [{ path: 'src/b.ts', line: 1 }]);
  stop();
  dispose();
});

test('labels that read the same are the same', () => {
  const a = { verb: 'Read', items: [{ text: 'a.ts', path: 'src/a.ts' }], more: 0 };
  assert.ok(
    sameGroupLabel(a, { verb: 'Read', items: [{ text: 'a.ts', path: 'src/a.ts' }], more: 0 })
  );
  assert.ok(!sameGroupLabel(a, { ...a, verb: 'Reading' }));
  assert.ok(!sameGroupLabel(a, { ...a, items: [{ text: 'a.ts', path: 'src/a.ts', line: 3 }] }));
  assert.ok(!sameGroupLabel(a, { ...a, more: 1 }));
});
