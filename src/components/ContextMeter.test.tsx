// Component test: the context bar above the chat input. A model's chat
// shows the backend's estimate; a connector's, the context its harness
// reported after its last reply, used of total. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ContextMeter, { ConnectorContextMeter } from './ContextMeter';

test('a connector’s chat shows its context used of its window, and where it compacts', () => {
  const root = container();
  const dispose = render(
    () => (
      <ConnectorContextMeter
        kind="claude-code"
        context={{ used: 35_197, window: 1_000_000, compactsAt: 784_000 }}
      />
    ),
    root
  );
  const bar = root.querySelector('.chat-context-bar')!;
  assert.equal(bar.textContent, 'Context used: 35k / 1M (4%)');
  assert.ok(bar.classList.contains('low'));
  const title = bar.getAttribute('title') ?? '';
  assert.match(title, /Claude Code’s context after its last reply: 35k of 1M tokens/);
  assert.match(title, /compacts it at about 784k tokens/);
  dispose();
});

test('a connector’s nearly full context is marked high', () => {
  const root = container();
  const dispose = render(
    () => <ConnectorContextMeter kind="claude-code" context={{ used: 180_000, window: 200_000 }} />,
    root
  );
  const bar = root.querySelector('.chat-context-bar')!;
  assert.equal(bar.textContent, 'Context used: 180k / 200k (90%)');
  assert.ok(bar.classList.contains('high'));
  assert.doesNotMatch(bar.getAttribute('title') ?? '', /compacts/);
  dispose();
});

test('a model’s chat shows the estimate of its next request', () => {
  const root = container();
  const dispose = render(
    () => (
      <ContextMeter
        usage={{
          promptTokens: 64_000,
          windowTokens: 128_000,
          windowGuessed: false,
          fullHistoryTokens: 60_000,
          historyTokens: 60_000,
          replyReserveTokens: 8_000,
          shortened: false,
        }}
      />
    ),
    root
  );
  const bar = root.querySelector('.chat-context-bar')!;
  assert.equal(bar.textContent, 'Context used: 50%');
  assert.match(bar.getAttribute('title') ?? '', /About 64k of 128k tokens in the next request/);
  dispose();
});
