// Component tests: a tool call's block (ToolCallBlock.tsx), its title as
// the chat shows it. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ToolCallBlock from './ToolCallBlock';

/** The title of the block of a finished call of `name` with `args`. */
function titleOf(name: string, args: Record<string, unknown>): string {
  const root = container();
  const dispose = render(
    () => (
      <ToolCallBlock
        toolCall={{ id: 'c1', name, arguments: JSON.stringify(args) }}
        dialogId={null}
        result={{ id: 'r1', role: 'tool', content: 'done' }}
      />
    ),
    root
  );
  const title = root.querySelector('.tool-call-title')?.textContent?.trim() ?? '';
  dispose();
  return title;
}

test('an MCP call is titled by the server’s tool, not by the mcp tool', () => {
  assert.equal(
    titleOf('mcp', { tool: 'docs.read', arguments: { path: 'intro' } }),
    'Called docs.read'
  );
  assert.equal(titleOf('mcp', { tool: 'docs.read', describe: true }), 'Read about docs.read');
  assert.equal(titleOf('mcp', {}), 'Called an MCP tool');
});
