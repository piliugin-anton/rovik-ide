// Component test: a browser tab's page (BrowserView.tsx), in a frame that
// can't navigate the app away, under an address bar that opens what it
// takes and says what it refuses, with Reload and "Open in your browser".
// The bar follows the page as the page says it moves (the tab port's
// script), and Reload and "Open in your browser" take it where it is. A
// tab not shown keeps its frame. And the app's CSP lets the frames load
// http and https pages. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import BrowserView, { FRAME_SANDBOX } from './BrowserView';
import type { BrowserTab } from '../lib/editorTabs';

const invoked: { cmd: string; args: unknown }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    invoked.push({ cmd, args });
    return null;
  },
};

const TAB: BrowserTab = {
  kind: 'browser',
  path: 'browser:1',
  address: 'http://localhost:5174/admin',
  loads: 0,
};
const FRAME = 'http://localhost:40001/admin';

/** The tab rendered, shown or not, and what it asked for. */
function rendered(active = true) {
  const root = container();
  const navigations: string[] = [];
  const asked = { reloads: [] as string[] };
  const dispose = render(
    () => (
      <BrowserView
        tab={TAB}
        active={active}
        frame={FRAME}
        onNavigate={(address) => navigations.push(address)}
        onReload={(address) => asked.reloads.push(address)}
      />
    ),
    root
  );
  const input = root.querySelector<HTMLInputElement>('.browser-address')!;
  const form = root.querySelector<HTMLFormElement>('.browser-bar')!;
  /** Types `text` in the address bar and presses Enter. */
  const go = (text: string) => {
    input.value = text;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  };
  return { root, input, go, navigations, asked, dispose };
}

test('a browser tab frames its page sandboxed, unable to navigate the app away', () => {
  const { root, input, dispose } = rendered();
  const frame = root.querySelector('iframe')!;
  assert.equal(frame.getAttribute('src'), FRAME, 'through its tab port');
  assert.equal(frame.getAttribute('title'), TAB.address);
  const allowed = frame.getAttribute('sandbox')!.split(/\s+/);
  assert.equal(frame.getAttribute('sandbox'), FRAME_SANDBOX);
  for (const needed of ['allow-scripts', 'allow-same-origin', 'allow-forms', 'allow-popups']) {
    assert.ok(allowed.includes(needed), needed);
  }
  assert.ok(!allowed.some((a) => a.startsWith('allow-top-navigation')), FRAME_SANDBOX);
  assert.equal(frame.getAttribute('allow'), 'clipboard-write');
  assert.equal(input.value, TAB.address, 'the address bar shows the page’s own address');
  assert.ok(root.querySelector('.browser-view')!.classList.contains('active'));
  dispose();
});

test('the address bar opens the address it takes, and says what it refuses', () => {
  const { root, input, go, navigations, dispose } = rendered();
  go('localhost:3000/login');
  assert.deepEqual(navigations, ['http://localhost:3000/login']);

  go('javascript:alert(1)');
  assert.deepEqual(navigations, ['http://localhost:3000/login'], 'not opened');
  assert.equal(
    root.querySelector('.browser-bar-note')?.textContent,
    'Only http and https pages open here'
  );
  assert.ok(input.classList.contains('refused'));
  assert.equal(input.getAttribute('aria-invalid'), 'true');

  input.value = 'localhost';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(root.querySelector('.browser-bar-note'), null, 'typing again clears it');

  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(input.value, TAB.address, 'Escape puts the page’s address back');
  dispose();
});

test('Reload reloads, and Open in your browser opens the page’s own address, not its frame’s', async () => {
  const { root, asked, dispose } = rendered();
  root.querySelector<HTMLButtonElement>('[aria-label="Reload"]')!.click();
  assert.deepEqual(asked.reloads, [TAB.address], 'where it loaded, until it says otherwise');

  invoked.length = 0;
  root.querySelector<HTMLButtonElement>('[aria-label="Open in your browser"]')!.click();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(invoked, [{ cmd: 'open_url', args: { url: TAB.address } }]);
  dispose();
});

/**
 * The page in the tab's frame, posting as the tab port's script does: the
 * frame's window is a stand-in, as no page loads in the test's DOM.
 */
function framedPage(root: HTMLElement) {
  const page = {};
  const frame = root.querySelector('iframe')!;
  Object.defineProperty(frame, 'contentWindow', { value: page, configurable: true });
  /** A message to the app, `from` a window at `origin`. */
  const post = (data: unknown, origin = 'http://localhost:40001', from: unknown = page) =>
    window.dispatchEvent(new window.MessageEvent('message', { data, origin, source: from as any }));
  /** The page says it's at `href`. */
  const says = (href: string, origin?: string, from?: unknown) =>
    post({ type: 'rovik-tab-location', href }, origin, from);
  return { frame, post, says };
}

test('the address bar follows the page as it moves, and Reload takes it where it is', async () => {
  const { root, input, asked, dispose } = rendered();
  const { frame, says } = framedPage(root);
  says('http://localhost:40001/admin/users?page=2#top');
  const there = 'http://localhost:5174/admin/users?page=2#top';
  assert.equal(input.value, there, 'at the port forwarded here, not the tab port');
  assert.equal(frame.getAttribute('title'), there);
  assert.equal(frame.getAttribute('src'), FRAME, 'the frame isn’t loaded again');

  root.querySelector<HTMLButtonElement>('[aria-label="Reload"]')!.click();
  assert.deepEqual(asked.reloads, [there]);
  invoked.length = 0;
  root.querySelector<HTMLButtonElement>('[aria-label="Open in your browser"]')!.click();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(invoked, [{ cmd: 'open_url', args: { url: there } }]);
  dispose();
});

test('only its own frame is heard, telling of its own origin', () => {
  const { root, input, dispose } = rendered();
  const { says, post } = framedPage(root);
  says('http://localhost:40001/other', 'http://localhost:40001', {});
  says('http://localhost:40002/other', 'http://localhost:40002');
  says('http://elsewhere.test/other', 'http://localhost:40001');
  says('not a url');
  post({ type: 'something-else', href: 'http://localhost:40001/other' });
  post('http://localhost:40001/other');
  assert.equal(input.value, TAB.address);
  dispose();
});

test('what the user is typing stays as the page moves, and Escape takes it where it is', () => {
  const { root, input, dispose } = rendered();
  const { says } = framedPage(root);
  input.value = 'localhost:3000';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  says('http://localhost:40001/settings');
  assert.equal(input.value, 'localhost:3000');
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(input.value, 'http://localhost:5174/settings');
  dispose();
});

test('a tab not shown keeps its frame, hidden', () => {
  const { root, dispose } = rendered(false);
  assert.ok(!root.querySelector('.browser-view')!.classList.contains('active'));
  assert.equal(root.querySelector('iframe')?.getAttribute('src'), FRAME);
  dispose();
});

test('the frame isn’t loaded again when its address is worked out again the same', () => {
  const root = container();
  const [ports, setPorts] = createSignal(1);
  const [tabPort, setTabPort] = createSignal(40001);
  const dispose = render(
    () => (
      <BrowserView
        tab={TAB}
        active={true}
        // Worked out again each time the ports change, as App does it.
        frame={(ports(), `http://localhost:${tabPort()}/admin`)}
        onNavigate={() => {}}
        onReload={() => {}}
      />
    ),
    root
  );
  const frame = root.querySelector('iframe')!;
  const observer = new MutationObserver(() => {});
  observer.observe(frame, { attributes: true, attributeFilter: ['src'] });
  setPorts(2);
  assert.equal(observer.takeRecords().length, 0, 'the same address: not set again');
  setTabPort(40002);
  assert.equal(observer.takeRecords().length, 1);
  assert.equal(frame.getAttribute('src'), 'http://localhost:40002/admin');
  observer.disconnect();
  dispose();
});

test('the app’s content security policy lets browser tabs frame http and https pages', () => {
  const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  const { csp, devCsp } = config.app.security;
  assert.equal(csp['frame-src'], 'http: https:');
  assert.equal(devCsp['frame-src'], 'http: https:');
});
