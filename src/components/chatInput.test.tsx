// Component tests: the chat input's right-click menu, and the images pasted
// into a message as the chat shows them. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createStore } from 'solid-js/store';
import ChatInputContextMenu from './ChatInputContextMenu';
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import type { ChatMessage } from '../types';

function menu(hasSelection: boolean, hasContent: boolean) {
  const root = container();
  const calls: string[] = [];
  const dispose = render(
    () => (
      <ChatInputContextMenu
        ref={() => {}}
        x={10}
        y={20}
        hasSelection={hasSelection}
        hasContent={hasContent}
        onCut={() => calls.push('cut')}
        onCopy={() => calls.push('copy')}
        onPaste={() => calls.push('paste')}
        onSelectAll={() => calls.push('select all')}
      />
    ),
    root
  );
  const items = [...root.querySelectorAll<HTMLElement>('[role="menuitem"]')];
  const item = (label: string) =>
    items.find((i) => i.querySelector('.context-menu-label')?.textContent === label)!;
  return { root, calls, items, item, dispose };
}

test('the input menu: Cut and Copy need a selection, Paste is always there', () => {
  const empty = menu(false, false);
  assert.deepEqual(
    empty.items.map((i) => i.querySelector('.context-menu-label')?.textContent),
    ['Cut', 'Copy', 'Paste', 'Select all']
  );
  for (const label of ['Cut', 'Copy', 'Select all']) {
    assert.ok(empty.item(label).classList.contains('disabled'), `${label} is disabled`);
    empty.item(label).click();
  }
  empty.item('Paste').click();
  assert.deepEqual(empty.calls, ['paste'], 'disabled items do nothing');
  empty.dispose();

  const selected = menu(true, true);
  for (const label of ['Cut', 'Copy', 'Paste', 'Select all']) {
    assert.ok(!selected.item(label).classList.contains('disabled'), `${label} is enabled`);
    selected.item(label).click();
  }
  assert.deepEqual(selected.calls, ['cut', 'copy', 'paste', 'select all']);
  selected.dispose();
});

test('pressing the input menu leaves the focus and selection in the input', () => {
  const m = menu(true, true);
  const press = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
  m.item('Paste').dispatchEvent(press);
  assert.equal(press.defaultPrevented, true);
  const rightClick = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
  m.item('Copy').dispatchEvent(rightClick);
  assert.equal(rightClick.defaultPrevented, true, 'no webview menu over this one');
  m.dispose();
});

test('a user message shows the images pasted into it, even with no text', () => {
  const [one, two] = ['b'.repeat(64), 'c'.repeat(64)];
  const [chat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Why is this blue?', attachments: [one] },
      { id: 'a1', role: 'assistant', content: 'The theme.' },
      { id: 'u2', role: 'user', content: '', attachments: [one, two] },
    ] as ChatMessage[],
    hasOlder: false,
    hasNewer: false,
    loadingOlder: false,
    lastUsed: 0,
  });
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () => Promise.resolve({ messages: [], hasOlder: false, hasNewer: false });
  const root = container();
  const noop = async () => 0;
  let dispose = () => {};
  try {
    dispose = render(
      () => (
        <MessageList
          dialogId="d-images"
          chat={chat}
          sending={false}
          focusMessageId={null}
          onLoadOlder={noop}
          onLoadNewer={noop}
          onReloadLatest={async () => {}}
          onRetry={() => {}}
          onContinue={() => {}}
          onContinueRun={() => {}}
          onRunChecks={() => {}}
          onOpenProjectChecks={() => {}}
        />
      ),
      root
    );
    const bubbles = [...root.querySelectorAll('.chat-message.user')];
    assert.equal(bubbles.length, 2, 'the images-only message has its bubble');
    const images = (bubble: Element) => [
      ...bubble.querySelectorAll<HTMLImageElement>('img.chat-message-image'),
    ];
    assert.equal(images(bubbles[0]).length, 1);
    assert.ok(images(bubbles[0])[0].getAttribute('src')!.endsWith(`/${one}`));
    assert.ok(bubbles[0].textContent?.includes('Why is this blue?'));
    assert.deepEqual(
      images(bubbles[1]).map((img) => img.getAttribute('src')!.split('/').pop()),
      [one, two]
    );
    // Nothing to copy from a message of images only.
    const wraps = [...root.querySelectorAll('.chat-message-wrap.user')];
    assert.ok(wraps[0].querySelector('.chat-copy-button'));
    assert.equal(wraps[1].querySelector('.chat-copy-button'), null);

    // A click shows an image whole, another shrinks it back.
    const image = images(bubbles[1])[0];
    image.click();
    assert.ok(image.classList.contains('expanded'));
    image.click();
    assert.ok(!image.classList.contains('expanded'));
  } finally {
    dispose();
    api.getMessages = original;
  }
});
