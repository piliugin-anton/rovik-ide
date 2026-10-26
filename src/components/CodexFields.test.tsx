// Component test: Codex's part of a connector's form, its `ConnectorFields`
// (CodexFields.tsx). How it signs in is in the Connection section; its
// permission mode (for its own tools, in Agent mode) comes first among its
// options. Whose tools it runs is the chat's mode's, not a setting.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal, type Component } from 'solid-js';
import { render } from 'solid-js/web';
import { CONNECTOR_FIELDS, type ConnectorFieldsProps } from './connectorFields';
import { newConnector } from '../lib/connectors';
import { CODEX_DEFAULTS, CODEX_PERMISSION_MODES } from '../lib/codex';
import type { Provider } from '../types';

const { Connection, Options } = CONNECTOR_FIELDS.codex;

/**
 * `Part` of the form for a new Codex connector with `settings`, kept up to
 * date as the form keeps its draft: the connector as it is now, and the root.
 */
function mount(Part: Component<ConnectorFieldsProps>, settings: Record<string, unknown> = {}) {
  const fresh = newConnector('codex');
  const [connector, setConnector] = createSignal<Provider>({
    ...fresh,
    settings: { ...fresh.settings, ...settings },
  });
  const root = container();
  const dispose = render(
    () => (
      <Part
        connector={connector()}
        update={(changes) => setConnector({ ...connector(), ...changes })}
      />
    ),
    root
  );
  return { root, connector, dispose };
}

/** The select of the row labelled `label`, if it's shown. */
function select(root: Element, label: string): HTMLSelectElement | undefined {
  const row = [...root.querySelectorAll('.setting-row')].find(
    (r) => r.querySelector('.setting-row-label')?.textContent === label
  );
  return (row?.querySelector('select') as HTMLSelectElement | null) ?? undefined;
}

function choose(el: HTMLSelectElement, value: string) {
  el.value = value;
  el.dispatchEvent(new Event('change', { bubbles: true }));
}

const sections = (root: Element) =>
  [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);

const fields = (root: Element) =>
  [...root.querySelectorAll('.setting-row-label')].map((l) => l.textContent);

test('a new Codex connector follows Rovik’s permissions with its own tools', () => {
  const codex = newConnector('codex');
  assert.equal(codex.baseUrl, 'connector://codex');
  assert.equal(codex.name, 'Codex');
  assert.deepEqual(codex.settings, CODEX_DEFAULTS);
  const { root, dispose } = mount(Options);
  assert.equal(select(root, 'Mode')!.value, 'rovik');
  dispose();
});

test('its permission mode comes first, and a change saves it without the tools choice it had', () => {
  // Saved by an earlier version: Rovik's tools, which hid its permission mode.
  const { root, connector, dispose } = mount(Options, { tools: 'rovik' });
  assert.deepEqual(sections(root), ['Permissions', 'Advanced']);
  assert.equal(select(root, 'Runs as'), undefined);
  assert.deepEqual(
    [...select(root, 'Mode')!.options].map((o) => o.value),
    CODEX_PERMISSION_MODES.map((m) => m.value)
  );
  choose(select(root, 'Mode')!, 'workspace');
  const saved = connector().settings ?? {};
  assert.equal(saved.permissionMode, 'workspace');
  assert.ok(!('tools' in saved), 'whose tools it runs is the chat’s mode’s');
  dispose();
});

test('the program, its settings folder and its arguments are the advanced ones', () => {
  const { root, connector, dispose } = mount(Options);
  const advanced = root.querySelector('details.connector-advanced')!;
  assert.ok(!advanced.hasAttribute('open'));
  assert.deepEqual(fields(advanced), ['Program', 'Settings folder', 'Arguments']);
  const folder = advanced.querySelectorAll('input')[1] as HTMLInputElement;
  folder.value = '/srv/codex';
  folder.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(connector().settings?.configDir, '/srv/codex');
  dispose();
});

test('it asks for a token or a key only when it signs in with one, and keeps it as the key', () => {
  const { root, connector, dispose } = mount(Connection);
  const signIn = select(root, 'Signs in with')!;
  assert.equal(signIn.value, 'login');
  assert.deepEqual(fields(root), ['Signs in with'], 'this computer’s login needs nothing more');
  choose(signIn, 'token');
  assert.deepEqual(fields(root), ['Signs in with', 'Access token']);
  choose(signIn, 'apiKey');
  assert.deepEqual(fields(root), ['Signs in with', 'API key']);
  const key = root.querySelector('input[type="password"]') as HTMLInputElement;
  key.value = 'sk-proj-x';
  key.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(connector().apiKey, 'sk-proj-x', 'the key goes to the keychain on save');
  dispose();
});
