// Component test: the ports a remote chat's servers opened show under its
// title row (D20): the server's own port each, with a button that copies
// the address on this computer that leads to it, the port forwarded here.
// Hidden while the chat has none. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import OpenedPorts from './OpenedPorts';
import { initCloud } from '../lib/cloud';

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, handler: args.handler });
      return args.handler;
    }
    if (cmd === 'hosts_summaries') return [];
    // Forwarded before this window listened.
    if (cmd === 'cloud_forwarded_ports') return { earlier: [{ port: 8000, localPort: 8000 }] };
    return null;
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

function emit(payload: unknown) {
  for (const l of listeners.filter((l) => l.event === 'cloud-event')) {
    callbacks.get(l.handler)!({ event: 'cloud-event', id: 1, payload });
  }
}

const numbers = (root: HTMLElement) =>
  [...root.querySelectorAll('.opened-port-number')].map((n) => n.textContent);

test('a chat shows the ports its servers opened, each copying the address here that leads to it', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="c1" />, root);
  assert.equal(root.querySelector('.opened-ports'), null, 'hidden without ports');

  emit({
    type: 'ports',
    dialogId: 'c1',
    ports: [
      { port: 3000, localPort: 3000 },
      { port: 5173, localPort: 5174 },
    ],
  });
  emit({ type: 'ports', dialogId: 'other', ports: [{ port: 8080, localPort: 8080 }] });
  const block = root.querySelector('.opened-ports');
  assert.equal(block?.querySelector('.opened-ports-label')?.textContent, 'Opened ports:');
  assert.deepEqual(numbers(root), ['3000', '5173'], 'the server’s own ports, this chat’s only');

  let copied = '';
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: {
      writeText: async (text: string) => {
        copied = text;
      },
    },
  });
  try {
    const buttons = root.querySelectorAll<HTMLButtonElement>('.opened-port-copy');
    assert.equal(buttons[1].getAttribute('aria-label'), 'Copy localhost:5174');
    buttons[1].click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(copied, 'localhost:5174', 'the port forwarded here, its own being taken');
    assert.equal(buttons[1].classList.contains('copied'), true);
    buttons[0].click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(copied, 'localhost:3000');
  } finally {
    delete (navigator as any).clipboard;
  }

  emit({ type: 'ports', dialogId: 'c1', ports: [{ port: 5173, localPort: 5174 }] });
  assert.deepEqual(numbers(root), ['5173'], 'a closed port goes');
  emit({ type: 'ports', dialogId: 'c1', ports: [] });
  assert.equal(root.querySelector('.opened-ports'), null, 'hidden once none is left');
  dispose();
});

test('ports forwarded before the window listened show too', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="earlier" />, root);
  assert.deepEqual(numbers(root), ['8000']);
  dispose();
});
