// Component test: Claude Code's part of a connector's form, its
// `ConnectorFields` (ClaudeCodeFields.tsx). How it signs in is in the
// Connection section; its permissions (for its own tools, in Agent mode)
// come first among its options. Whose tools it runs is the chat's mode's,
// not a setting.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal, type Component } from 'solid-js';
import { render } from 'solid-js/web';
import { CONNECTOR_FIELDS, type ConnectorFieldsProps } from './connectorFields';
import { newConnector } from '../lib/connectors';
import { PERMISSION_MODES } from '../lib/claudeCode';
import type { Provider } from '../types';

const { Connection, Options } = CONNECTOR_FIELDS['claude-code'];

/**
 * `Part` of the form for a new connector with `settings`, kept up to date
 * as the form keeps its draft: the connector as it is now, and the root.
 */
function mount(Part: Component<ConnectorFieldsProps>, settings: Record<string, unknown> = {}) {
  const fresh = newConnector('claude-code');
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

/** The titles of the sections shown. */
const sections = (root: Element) =>
  [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);

/** The fields shown, by their labels: a row's, or a checkbox's first line. */
const fields = (root: Element) =>
  [...root.querySelectorAll('.setting-row-label, .setting-checkbox > span')].map((l) =>
    [...l.childNodes]
      .filter((n) => n.nodeType === 3)
      .map((n) => n.textContent)
      .join('')
      .trim()
  );

test('its permissions come first, and whose tools it runs is no setting', () => {
  // Saved by an earlier version: Rovik's tools, which hid its permissions.
  const { root, dispose } = mount(Options, { tools: 'rovik' });
  assert.deepEqual(sections(root), ['Permissions', 'Limits', 'Advanced']);
  assert.equal(select(root, 'Runs as'), undefined);
  assert.deepEqual(
    [...select(root, 'Mode')!.options].map((o) => o.value),
    PERMISSION_MODES.map((m) => m.value)
  );
  const hint = root.querySelector('.settings-section .settings-hint')!;
  assert.match(hint.textContent ?? '', /in Agent mode\. In Plan mode and Autopilot/);
  dispose();
});

test('allowed-tools rules show only when Claude Code’s own permission mode decides', () => {
  const { root, dispose } = mount(Options);
  const permissions = () => root.querySelectorAll('.settings-section')[0];
  assert.deepEqual(
    fields(permissions()),
    ['Mode', 'Denied tools'],
    'Rovik’s permissions would pass them by'
  );
  choose(select(root, 'Mode')!, 'acceptEdits');
  assert.deepEqual(fields(permissions()), ['Mode', 'Allowed tools', 'Denied tools']);
  dispose();
});

test('the program is with the other advanced settings, closed until opened', () => {
  const { root, dispose } = mount(Options);
  const advanced = root.querySelector('details.connector-advanced')!;
  assert.equal(advanced.querySelector('summary .settings-section-title')?.textContent, 'Advanced');
  assert.ok(!advanced.hasAttribute('open'));
  assert.deepEqual(fields(advanced), ['Program', 'Config folder', 'Arguments', 'Bare mode']);
  dispose();
});

test('its limits keep whole turns and a budget in dollars, and empty is no limit', () => {
  const { root, connector, dispose } = mount(Options);
  const [turns, budget] = [
    ...root.querySelectorAll('.connector-limits input'),
  ] as HTMLInputElement[];
  const type = (el: HTMLInputElement, value: string) => {
    el.value = value;
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  type(turns, '12.6');
  type(budget, '2.75');
  assert.equal(connector().settings?.maxTurns, 13);
  assert.equal(turns.value, '13', 'it shows what it keeps');
  assert.equal(connector().settings?.maxBudgetUsd, 2.75);
  type(turns, '');
  assert.equal(connector().settings?.maxTurns, undefined);
  dispose();
});

test('under a choice is its line, with all of it as the line’s tooltip', () => {
  const { root, dispose } = mount(Options);
  const mode = select(root, 'Mode')!;
  const line = mode.closest('.setting-row')!.nextElementSibling!;
  assert.deepEqual(
    [...mode.options].map((o) => o.title),
    PERMISSION_MODES.map((m) => m.hint),
    'each option’s line is its tooltip'
  );
  assert.equal(line.textContent, PERMISSION_MODES[0].hint);
  assert.equal(line.getAttribute('title'), PERMISSION_MODES[0].detail);
  choose(mode, PERMISSION_MODES[1].value);
  assert.equal(line.textContent, PERMISSION_MODES[1].hint);
  assert.equal(line.getAttribute('title'), PERMISSION_MODES[1].detail);
  dispose();
});

test('a change saves its settings without the choices it no longer has', () => {
  const { root, connector, dispose } = mount(Options, { systemPrompt: 'append', tools: 'rovik' });
  choose(select(root, 'Mode')!, 'acceptEdits');
  const saved = connector().settings ?? {};
  assert.equal(saved.permissionMode, 'acceptEdits');
  assert.ok(!('systemPrompt' in saved), 'the prompt is the tools’ now');
  assert.ok(!('tools' in saved), 'whose tools it runs is the chat’s mode’s');
  dispose();
});

test('it asks for a token or a key only when it signs in with one, and keeps it as the key', () => {
  const { root, connector, dispose } = mount(Connection);
  const signIn = select(root, 'Signs in with')!;
  assert.equal(signIn.value, 'login');
  assert.deepEqual(fields(root), ['Signs in with'], 'this computer’s login needs nothing more');
  choose(signIn, 'token');
  assert.deepEqual(fields(root), ['Signs in with', 'Token']);
  const token = root.querySelector('input[type="password"]') as HTMLInputElement;
  token.value = 'sk-ant-oat01-x';
  token.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(connector().apiKey, 'sk-ant-oat01-x', 'the key goes to the keychain on save');
  choose(signIn, 'apiKey');
  assert.deepEqual(fields(root), ['Signs in with', 'API key']);
  dispose();
});
