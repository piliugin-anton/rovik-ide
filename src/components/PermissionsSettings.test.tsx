// Component tests: Settings → Permissions & approval, its Network section:
// the hosts the agent may reach, listed only while the list is on, each
// entry kept as the backend reads it, once; and the MCP tool calls' rule.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import PermissionsSettings from './PermissionsSettings';
import { DEFAULT_PERMISSIONS, type AgentPermissions } from '../lib/permissions';

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string) => {
    throw new Error(`no app: ${cmd}`);
  },
};

function page(permissions: Partial<AgentPermissions> = {}) {
  const root = container();
  const [current, setCurrent] = createSignal<AgentPermissions>({
    ...DEFAULT_PERMISSIONS,
    ...permissions,
  });
  render(
    () => (
      <PermissionsSettings
        permissions={current()}
        providers={[]}
        onChange={setCurrent}
        onOpenPage={() => {}}
      />
    ),
    root
  );
  return { root, current };
}

const hostInput = (root: HTMLElement) =>
  root.querySelector('input[aria-label="Host to allow"]') as HTMLInputElement | null;

/** Types `text` into the host field and presses Enter. */
function addHost(root: HTMLElement, text: string) {
  const input = hostInput(root)!;
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
}

const listed = (root: HTMLElement) =>
  [...root.querySelectorAll('.command-row-text')].map((row) => row.textContent);

const choose = (root: HTMLElement, group: string, option: string) =>
  [...root.querySelectorAll(`[aria-label="${group}"] button`)]
    .find((button) => button.textContent === option)!
    .dispatchEvent(new MouseEvent('click', { bubbles: true }));

test('the hosts are listed only while the list is on', () => {
  const { root, current } = page({ allowAnyCommand: true });
  assert.equal(hostInput(root), null, 'any host: no list');
  choose(root, 'Which hosts can be reached', 'Listed only');
  assert.equal(current().allowAnyHost, false);
  assert.ok(hostInput(root));
  assert.match(root.textContent!, /No hosts listed: only localhost is reached\./);
  choose(root, 'Which hosts can be reached', 'Any host');
  assert.equal(current().allowAnyHost, true);
  assert.equal(hostInput(root), null);
});

test('a host typed is kept as the backend reads it, once, and can be removed', () => {
  const { root, current } = page({ allowAnyCommand: true, allowAnyHost: false });
  addHost(root, 'https://Docs.RS/tokio');
  addHost(root, 'docs.rs');
  addHost(root, '*.GitHub.com');
  assert.deepEqual(current().allowedHosts, ['docs.rs', '*.github.com']);
  assert.deepEqual(listed(root), ['docs.rs', '*.github.com']);
  assert.equal(hostInput(root)!.value, '', 'the field is cleared');
  (root.querySelector('button[aria-label="Remove docs.rs"]') as HTMLElement).click();
  assert.deepEqual(current().allowedHosts, ['*.github.com']);
});

test('what is not a host is refused with why, until the entry changes', () => {
  const { root, current } = page({ allowAnyCommand: true, allowAnyHost: false });
  addHost(root, 'exa mple.com');
  assert.deepEqual(current().allowedHosts, []);
  const alert = root.querySelector('[role="alert"]');
  assert.match(alert!.textContent!, /“exa mple\.com” isn’t a host/);
  assert.equal(hostInput(root)!.value, 'exa mple.com', 'kept to correct');
  const input = hostInput(root)!;
  input.value = 'example.com';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(root.querySelector('[role="alert"]'), null);
});

test('MCP tool calls have their own rule, asking you by default', () => {
  const { root, current } = page();
  const group = 'Approval for each MCP tool call';
  const selected = () =>
    root.querySelector(`[aria-label="${group}"] button[aria-checked="true"]`)?.textContent;
  assert.equal(selected(), 'By you');
  assert.match(root.textContent!, /Every MCP tool call waits for you to allow or deny it\./);
  choose(root, group, 'Off');
  assert.deepEqual(current().mcpApproval, { required: false, by: 'user' });
  assert.match(root.textContent!, /MCP tool calls run right away\./);
  assert.deepEqual(current().toolApproval, DEFAULT_PERMISSIONS.toolApproval, 'the others stay');
});
