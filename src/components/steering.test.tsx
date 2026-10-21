// Component tests: messages sent to the run while it works, as the chat
// shows them (docs/design/steering-plan.md 3.7): after the chat's
// messages, dimmed, with what they wait for and ✕. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createStore } from 'solid-js/store';
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import type { QueuedSteer } from '../lib/chatStore';
import type { ChatMessage } from '../types';

/** The chat showing these messages and queued ones; `check` looks at what it rendered. */
function showChat(
  messages: ChatMessage[],
  queued: QueuedSteer[],
  check: (root: HTMLElement, unsteered: string[]) => void,
  sending = true
) {
  const [chat] = createStore({
    messages,
    hasOlder: false,
    hasNewer: false,
    loadingOlder: false,
    lastUsed: 0,
  });
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () => Promise.resolve({ messages: [], hasOlder: false, hasNewer: false });
  const root = container();
  const unsteered: string[] = [];
  const noop = async () => 0;
  let dispose = () => {};
  try {
    dispose = render(
      () => (
        <MessageList
          dialogId="steered"
          chat={chat}
          sending={sending}
          focusMessageId={null}
          onLoadOlder={noop}
          onLoadNewer={noop}
          onReloadLatest={async () => {}}
          onRetry={() => {}}
          onContinue={() => {}}
          onContinueRun={() => {}}
          onRunChecks={() => {}}
          onOpenProjectChecks={() => {}}
          queued={queued}
          queuedLabel="Queued: the agent reads it after you answer the approval"
          onUnsteer={(id) => unsteered.push(id)}
        />
      ),
      root
    );
    check(root, unsteered);
  } finally {
    dispose();
    api.getMessages = original;
  }
}

const queuedOne = (id: string, content: string, late = false): QueuedSteer => ({
  message: { id, role: 'user', content, finalized: true },
  queuedAt: 1,
  ...(late ? { late } : {}),
});

test('a queued message shows after the chat’s messages, labelled, and ✕ takes it back', () => {
  const messages = [
    { id: 'u1', role: 'user', content: 'Add a cache' },
    { id: 'a1', role: 'assistant', content: 'On it.' },
  ] as ChatMessage[];
  showChat(
    messages,
    [queuedOne('s1', 'Use the existing HTTP client'), queuedOne('s2', 'Then rerun', true)],
    (root, unsteered) => {
      const inner = root.querySelector('.chat-messages-inner')!;
      const items = [...inner.children];
      const queued = [...root.querySelectorAll('.chat-queued')];
      assert.equal(queued.length, 2);
      // After every message of the chat.
      assert.equal(items.indexOf(queued[0]), items.length - 2);
      assert.ok(queued[0].querySelector('.chat-message.user.queued'));
      assert.ok(queued[0].textContent?.includes('Use the existing HTTP client'));
      assert.equal(
        queued[0].querySelector('.chat-queued-label')?.textContent,
        'Queued: the agent reads it after you answer the approval'
      );
      // One the run could no longer take goes as a new message after it.
      assert.equal(
        queued[1].querySelector('.chat-queued-label')?.textContent,
        'Sent as a new message once the agent has finished'
      );
      queued[0].querySelector<HTMLButtonElement>('.chat-queued-unsteer')!.click();
      assert.deepEqual(unsteered, ['s1']);
    }
  );
});

test('a failed message sent to the run doesn’t make its request undelivered: Retry stays', () => {
  const messages = [
    { id: 'u1', role: 'user', content: 'Add a cache' },
    {
      id: 's1',
      role: 'user',
      content: 'Use the client',
      failed: true,
      meta: { steer: { queuedAt: 1 } },
    },
    {
      id: 'a1',
      role: 'assistant',
      content: '',
      error: 'The provider is overloaded',
      finalized: true,
      meta: { failure: { kind: 'server_error', message: 'Overloaded', attempts: 3 } },
    },
  ] as ChatMessage[];
  showChat(
    messages,
    [],
    (root) => {
      const retry = [...root.querySelectorAll('button')].find((b) => b.textContent === 'Retry');
      assert.ok(retry, 'the request itself was delivered');
    },
    false
  );
});
