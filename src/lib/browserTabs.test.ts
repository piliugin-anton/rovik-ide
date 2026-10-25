// What a browser tab shows (lib/browserTabs.ts): the address its address
// bar takes, where its frame loads a forwarded page (through the forward's
// tab port), its name in the tab strip, and the pages the app and "Opened
// ports" ask it to open. Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  addressFrom,
  frameUrl,
  onPageWanted,
  openPage,
  pageLabel,
  setPageOpener,
} from './browserTabs';

const APP = 'http://tauri.localhost';

test('the address bar takes http and https addresses, a bare host getting http', () => {
  assert.equal(addressFrom('localhost:5173', APP), 'http://localhost:5173/');
  assert.equal(addressFrom('  localhost:5173/admin?x=1 ', APP), 'http://localhost:5173/admin?x=1');
  assert.equal(addressFrom('example.com', APP), 'http://example.com/');
  assert.equal(addressFrom('https://example.com/a#b', APP), 'https://example.com/a#b');
  assert.equal(addressFrom('HTTP://127.0.0.1:8000', APP), 'http://127.0.0.1:8000/');
  assert.equal(addressFrom('[::1]:3000', APP), 'http://[::1]:3000/');
});

test('the address bar refuses other schemes, nothing, and the app itself', () => {
  for (const text of [
    '',
    '   ',
    'javascript:alert(1)',
    'file:///etc/passwd',
    'data:text/html,<p>x</p>',
    'mailto:me@example.com',
    'tauri://localhost/',
    'http://',
  ]) {
    assert.equal(addressFrom(text, APP), null, text);
  }
  assert.equal(addressFrom('tauri.localhost', APP), null, "the app's own page");
  assert.equal(addressFrom('localhost:1420', 'http://localhost:1420'), null, 'the app in dev');
});

const PORTS = [
  { port: 5173, localPort: 5174, tabPort: 40001 },
  { port: 8443, localPort: 8443 },
];

test('a forwarded page loads through its forward’s tab port, the rest of its address kept', () => {
  assert.equal(
    frameUrl('http://localhost:5174/admin?x=1#top', PORTS),
    'http://localhost:40001/admin?x=1#top'
  );
  assert.equal(frameUrl('http://127.0.0.1:5174/', PORTS), 'http://127.0.0.1:40001/');
  assert.equal(frameUrl('http://[::1]:5174/', PORTS), 'http://[::1]:40001/');
});

test('any other page loads at its own address', () => {
  for (const address of [
    'http://localhost:5173/',
    'https://localhost:8443/',
    'http://localhost:8443/',
    'https://localhost:5174/',
    'http://example.com:5174/',
    'not an address',
  ]) {
    assert.equal(frameUrl(address, PORTS), address, address);
  }
  assert.equal(frameUrl('http://localhost:5174/', []), 'http://localhost:5174/');
});

test('a browser tab is named after its page’s host and port', () => {
  assert.equal(pageLabel('http://localhost:5173/admin'), 'localhost:5173');
  assert.equal(pageLabel('https://example.com/'), 'example.com');
  assert.equal(pageLabel('nonsense'), 'nonsense');
});

test('a page asked for opens through the opener App sets, and none without one', () => {
  const opened: string[] = [];
  openPage('http://localhost:5173/');
  setPageOpener((address) => opened.push(address));
  openPage('http://localhost:3000/');
  setPageOpener(null);
  openPage('http://localhost:4000/');
  assert.deepEqual(opened, ['http://localhost:3000/']);
});

test('the pages the app asks for come to the handler until it stops listening', async () => {
  const callbacks = new Map<number, (event: unknown) => void>();
  const listeners: { event: string; handler: number }[] = [];
  const unlistened: number[] = [];
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
      if (cmd === 'plugin:event|unlisten') unlistened.push(args.eventId);
      return null;
    },
  };
  (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  try {
    const wanted: string[] = [];
    const stop = onPageWanted((address) => wanted.push(address));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const [listener] = listeners;
    assert.equal(listener.event, 'open-page');
    callbacks.get(listener.handler)!({
      event: 'open-page',
      id: 1,
      payload: 'http://localhost:5173/',
    });
    assert.deepEqual(wanted, ['http://localhost:5173/']);
    stop();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(unlistened, [listener.handler]);
  } finally {
    delete (window as any).__TAURI_INTERNALS__;
    delete (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__;
  }
});
