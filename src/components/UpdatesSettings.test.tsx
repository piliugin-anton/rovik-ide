// Component tests: Settings → Updates. The version this is; "Check now" and
// what it found, with the new version's Update and What's new; the switch
// for the looks every 30 minutes, kept for every window. The app is
// answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import UpdatesSettings from './UpdatesSettings';
import { checkingForUpdates, startUpdates } from '../lib/updates';
import type { SettingsPage } from '../lib/appSettings';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') return 0;
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
after(() => dispose());

const opened: SettingsPage[] = [];
async function page() {
  dispose();
  const root = container();
  dispose = render(() => <UpdatesSettings onOpenPage={(p) => opened.push(p)} />, root);
  await flush();
  return root;
}

const button = (root: HTMLElement, text: string | RegExp) =>
  [...root.querySelectorAll('button')].find((b) =>
    typeof text === 'string' ? b.textContent?.trim() === text : text.test(b.textContent ?? '')
  ) as HTMLButtonElement | undefined;

test('the page says the version, and Check now what it found', async () => {
  backend = {
    update_info: () => ({ version: '1.0.0', installable: true, periodic: true }),
    update_check: () => ({
      version: '1.1.0',
      current: '1.0.0',
      page: 'https://github.com/o/r/releases/tag/v1.1.0',
      installable: true,
    }),
    open_url: () => null,
  };
  await startUpdates(
    { folder: () => null, settle: async () => true, confirm: async () => true },
    false
  );
  const root = await page();
  assert.match(root.querySelector('.updates-version')!.textContent!, /Rovik 1\.0\.0/);
  assert.equal(root.querySelector('.settings-status'), null, 'nothing looked at yet');
  button(root, 'Check now')!.click();
  await flush();
  assert.ok(calls.some((c) => c.cmd === 'update_check'));
  assert.equal(root.querySelector('.settings-status')!.textContent, 'Rovik 1.1.0 is available.');
  assert.ok(button(root, 'Update to 1.1.0'));
  button(root, 'What’s new')!.click();
  await flush();
  assert.deepEqual(calls[calls.length - 1], {
    cmd: 'open_url',
    args: { url: 'https://github.com/o/r/releases/tag/v1.1.0' },
  });
  // The page's links go to the pages they name.
  button(root, 'Cloud hosts')!.click();
  assert.deepEqual(opened, ['hosts']);
});

test('the switch for the looks is on by default, and off is kept for every window', async () => {
  const root = await page();
  const box = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
  assert.equal(box.checked, true);
  box.checked = false;
  box.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal(checkingForUpdates(), false);
  assert.equal(JSON.parse(localStorage.getItem('rovik.appUpdates') ?? '{}').checkForUpdates, false);
  box.checked = true;
  box.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal(checkingForUpdates(), true);
  assert.equal(
    JSON.parse(localStorage.getItem('rovik.appUpdates') ?? '{}').checkForUpdates,
    undefined
  );
});
