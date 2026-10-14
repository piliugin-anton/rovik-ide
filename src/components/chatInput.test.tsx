// Component tests: the chat input's right-click menu, and the images and
// files attached to a message as the chat shows them. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createStore } from 'solid-js/store';
import ChatInputContextMenu from './ChatInputContextMenu';
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import { pastedSegments } from '../lib/composerClipboard';
import { blockMarkup } from '../lib/textBlocks';
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

/** The chat showing these messages; `check` looks at what it rendered. */
function showChat(dialogId: string, messages: ChatMessage[], check: (root: HTMLElement) => void) {
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
  const noop = async () => 0;
  let dispose = () => {};
  try {
    dispose = render(
      () => (
        <MessageList
          dialogId={dialogId}
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
    check(root);
  } finally {
    dispose();
    api.getMessages = original;
  }
}

test('a user message shows the images pasted into it, even with no text', () => {
  const [one, two] = ['b'.repeat(64), 'c'.repeat(64)];
  const messages = [
    { id: 'u1', role: 'user', content: 'Why is this blue?', attachments: [one] },
    { id: 'a1', role: 'assistant', content: 'The theme.' },
    { id: 'u2', role: 'user', content: '', attachments: [one, two] },
  ] as ChatMessage[];
  showChat('d-images', messages, (root) => {
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
    // A message of images only can be copied too: pasted back, they're chips.
    const wraps = [...root.querySelectorAll('.chat-message-wrap.user')];
    assert.ok(wraps[0].querySelector('.chat-copy-button'));
    assert.ok(wraps[1].querySelector('.chat-copy-button'));

    // A click shows an image whole, another shrinks it back.
    const image = images(bubbles[1])[0];
    image.click();
    assert.ok(image.classList.contains('expanded'));
    image.click();
    assert.ok(!image.classList.contains('expanded'));
  });
});

const pdf = { id: 'd'.repeat(64), name: 'report.pdf', size: 2048, pages: 3 };
const image = 'e'.repeat(64);
const notes = '# Notes\n' + 'a long line of notes\n'.repeat(200);
const withFiles = {
  id: 'u1',
  role: 'user',
  content: `Compare${blockMarkup({ text: notes, name: 'notes.md' })}with the report`,
  attachments: [pdf.id, image],
  meta: { pdfs: [pdf] },
} as ChatMessage;

test("a message's PDFs are listed by name, its text files collapsed under their names", () => {
  showChat('d-files', [withFiles], (root) => {
    const bubble = root.querySelector('.chat-message.user')!;
    const files = [...bubble.querySelectorAll('.chat-message-file')];
    assert.equal(files.length, 1);
    assert.equal(files[0].querySelector('.chat-message-file-name')?.textContent, 'report.pdf');
    assert.equal(files[0].querySelector('.chat-message-file-size')?.textContent, '3 pages');
    // The PDF isn't shown as an image; the image is.
    const images = [...bubble.querySelectorAll<HTMLImageElement>('img.chat-message-image')];
    assert.deepEqual(
      images.map((img) => img.getAttribute('src')!.split('/').pop()),
      [image]
    );
    const block = bubble.querySelector('.chat-pasted-toggle')!;
    assert.match(block.textContent ?? '', /^notes\.md · 202 lines, /);
    assert.ok(!bubble.textContent?.includes('a long line of notes'), 'collapsed');
  });
});

test('copying a message with a file pastes it back as its chips, not as pasted text', () => {
  showChat('d-copy', [withFiles], (root) => {
    // The webview's copy, as execCommand runs it.
    let copied: DataTransfer | null = null;
    const doc = document as Document & { execCommand: (command: string) => boolean };
    doc.execCommand = (command: string) => {
      const event = new window.ClipboardEvent(command, {
        clipboardData: new window.DataTransfer(),
        cancelable: true,
      });
      document.dispatchEvent(event);
      copied = event.clipboardData;
      return event.defaultPrevented;
    };
    try {
      root.querySelector<HTMLButtonElement>('.chat-copy-button')!.click();
    } finally {
      delete (doc as { execCommand?: unknown }).execCommand;
    }
    assert.ok(copied, 'copied');
    const data = copied as unknown as DataTransfer;
    // Other apps get the words, the files by name.
    assert.equal(
      data.getData('text/plain'),
      'Compare [notes.md] with the report\n[report.pdf] [image]'
    );
    // The input gets them back as they were sent.
    assert.deepEqual(pastedSegments(data), [
      { type: 'text', text: 'Compare' },
      { type: 'pasted', text: notes, name: 'notes.md' },
      { type: 'text', text: 'with the report' },
      { type: 'text', text: '\n' },
      { type: 'pdf', pdf },
      { type: 'image', image: { id: image } },
    ]);
  });
});
