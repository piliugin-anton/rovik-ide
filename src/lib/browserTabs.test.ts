// What a browser tab shows (lib/browserTabs.ts): the address its address
// bar takes, where its frame loads a forwarded page (through the forward's
// tab port), where the page says it is as it moves (the tab port's script,
// run here as the page runs it), its name in the tab strip, and the pages
// the app and "Opened ports" ask it to open. Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  addressFrom,
  frameUrl,
  onPageWanted,
  openPage,
  pageAddress,
  pageLabel,
  reportedAddress,
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

test('a page at its frame’s origin is shown at the port forwarded here, path and all', () => {
  const frame = 'http://localhost:40001/';
  const address = 'http://localhost:5174/';
  assert.equal(
    pageAddress('http://localhost:40001/a/b?c=1#d', frame, address),
    'http://localhost:5174/a/b?c=1#d'
  );
  assert.equal(pageAddress('http://localhost:40002/a', frame, address), null, 'another origin');
  assert.equal(pageAddress('nonsense', frame, address), null);
  // A page framed at its own address (not forwarded) stays where it is.
  assert.equal(
    pageAddress('https://example.com/x', 'https://example.com/', 'https://example.com/'),
    'https://example.com/x'
  );
});

test('only the tab port script’s message says where a page is', () => {
  const href = 'http://localhost:40001/a';
  assert.equal(reportedAddress({ type: 'rovik-tab-location', href }), href);
  assert.equal(reportedAddress({ type: 'other', href }), null);
  assert.equal(reportedAddress({ type: 'rovik-tab-location', href: 1 }), null);
  assert.equal(reportedAddress(href), null);
  assert.equal(reportedAddress(null), null);
});

/** The tab port's script (rovik-client's remote/tab.js), as it's served. */
const TAB_SCRIPT = readFileSync('src-tauri/crates/rovik-client/src/remote/tab.js', 'utf8');

/**
 * Runs the tab port's script in a page: in the app's tab's frame, or `in` a
 * window of its own or a frame of the page's. What it posts, and how to
 * move the page and let time pass.
 */
function pageWithScript(where: 'tab' | 'window' | 'nested' = 'tab', win: any = {}) {
  const posted: { data: unknown; target: string }[] = [];
  const parent = { postMessage: (data: unknown, target: string) => posted.push({ data, target }) };
  win.parent ??= where === 'window' ? win : parent;
  win.top ??= where === 'nested' ? {} : win.parent;
  const location = { href: 'http://localhost:40001/' };
  const listeners = new Map<string, () => void>();
  const ticks: (() => void)[] = [];
  let removed = false;
  const document = { currentScript: { remove: () => (removed = true) } };
  new Function('window', 'document', 'location', 'addEventListener', 'setInterval', TAB_SCRIPT)(
    win,
    document,
    location,
    (event: string, listener: () => void) => listeners.set(event, listener),
    (tick: () => void) => ticks.push(tick)
  );
  const tick = () => ticks.forEach((t) => t());
  return { win, posted, location, listeners, tick, removed: () => removed };
}

const at = (href: string) => ({ data: { type: 'rovik-tab-location', href }, target: '*' });

test('the tab port’s script tells the tab where its page is, as the page moves', () => {
  const page = pageWithScript();
  assert.ok(page.removed(), 'its tag is gone from the page');
  assert.deepEqual(page.posted, [at('http://localhost:40001/')]);
  page.tick();
  assert.equal(page.posted.length, 1, 'told once while it stays');
  // history.pushState: seen as time passes.
  page.location.href = 'http://localhost:40001/about';
  page.tick();
  // Back, and a hash: told at once.
  page.location.href = 'http://localhost:40001/';
  page.listeners.get('popstate')!();
  page.location.href = 'http://localhost:40001/#top';
  page.listeners.get('hashchange')!();
  assert.deepEqual(page.posted, [
    at('http://localhost:40001/'),
    at('http://localhost:40001/about'),
    at('http://localhost:40001/'),
    at('http://localhost:40001/#top'),
  ]);
  // Run again in the same page (its tag in a page fetched by a script).
  assert.deepEqual(pageWithScript('tab', page.win).posted, [], 'once a page');
});

test('the tab port’s script tells nothing in a window of its own or a frame of the page’s', () => {
  assert.deepEqual(pageWithScript('window').posted, []);
  assert.deepEqual(pageWithScript('nested').posted, []);
});
