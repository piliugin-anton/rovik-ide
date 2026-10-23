// Component tests: the chat's model picker with models from providers and
// connectors: the source switch, the search, the icons and where "Manage…"
// goes. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ModelPicker, { filterItems, pickerItems } from './ModelPicker';
import { newConnector } from '../lib/connectors';
import type { ModelSelection, Provider } from '../types';

const openai: Provider = {
  id: 'p1',
  name: 'OpenAI',
  baseUrl: 'https://api.openai.com/v1',
  apiKey: '',
  stream: true,
  models: [
    { id: 'gpt-5', enabled: true },
    { id: 'gpt-4.1', enabled: false },
  ],
};
const claude: Provider = {
  ...newConnector('claude-code'),
  id: 'c1',
  models: [
    { id: 'sonnet', enabled: true },
    { id: 'opus', enabled: true },
  ],
};

const options = (root: HTMLElement) =>
  [...root.querySelectorAll('[role="option"]')].map((o) => o.textContent?.trim());

test('the picker lists enabled models, providers first, each with its source', () => {
  const items = pickerItems([claude, openai]);
  assert.deepEqual(
    items.map((i) => [i.label, i.source]),
    [
      ['OpenAI/openai/gpt-5', 'provider'],
      ['Claude Code/sonnet', 'connector'],
      ['Claude Code/opus', 'connector'],
    ]
  );
  assert.deepEqual(
    filterItems(items, '', 'connector').map((i) => i.modelId),
    ['sonnet', 'opus']
  );
  assert.deepEqual(
    filterItems(items, 'gpt', 'all').map((i) => i.modelId),
    ['gpt-5']
  );
  assert.deepEqual(
    filterItems(items, 'claude son', 'all').map((i) => i.modelId),
    ['sonnet']
  );
  assert.deepEqual(filterItems(items, 'opus', 'provider'), []);
});

test('the popover: a source switch when both kinds exist, icons, and Manage… for what’s shown', () => {
  let chosen: ModelSelection | null = null;
  let opened: string | null = null;
  const root = container();
  render(
    () => (
      <ModelPicker
        providers={[openai, claude]}
        selection={{ providerId: 'c1', modelId: 'sonnet' }}
        onSelect={(sel) => (chosen = sel)}
        onOpenSettings={(page) => (opened = page)}
      />
    ),
    root
  );
  const trigger = root.querySelector<HTMLButtonElement>('.model-picker-trigger')!;
  assert.equal(trigger.textContent?.trim(), 'Claude Code/sonnet');
  assert.ok(
    trigger.querySelector('.model-source-icon.connector'),
    'the current model wears its icon'
  );
  trigger.click();
  assert.deepEqual(options(root), [
    'OpenAI/openai/gpt-5',
    'Claude Code/sonnet',
    'Claude Code/opus',
  ]);
  assert.equal(root.querySelectorAll('[role="option"] .model-source-icon.connector').length, 2);
  const radios = [
    ...root.querySelectorAll<HTMLButtonElement>('.model-picker-sources [role="radio"]'),
  ];
  assert.deepEqual(
    radios.map((r) => r.textContent?.replace(/\s+/g, '')),
    ['All3', 'Modelproviders1', 'Connectors2']
  );
  radios[2].click();
  assert.deepEqual(options(root), ['Claude Code/sonnet', 'Claude Code/opus']);
  assert.equal(root.querySelector('.model-picker-manage')?.textContent, 'Manage connectors…');
  radios[1].click();
  assert.deepEqual(options(root), ['OpenAI/openai/gpt-5']);
  assert.equal(root.querySelector('.model-picker-manage')?.textContent, 'Manage providers…');
  (root.querySelector('[role="option"]') as HTMLElement).click();
  assert.deepEqual(chosen, { providerId: 'p1', modelId: 'gpt-5' });
  assert.equal(root.querySelector('.model-picker-popover'), null, 'closed on choice');
  // Manage… opens the page for what the list shows.
  trigger.click();
  [...root.querySelectorAll<HTMLButtonElement>('.model-picker-sources [role="radio"]')][2].click();
  root.querySelector<HTMLButtonElement>('.model-picker-manage')!.click();
  assert.equal(opened, 'connectors');
});

test('with one kind of source there is no switch', () => {
  const root = container();
  render(
    () => (
      <ModelPicker
        providers={[openai]}
        selection={null}
        onSelect={() => {}}
        onOpenSettings={() => {}}
      />
    ),
    root
  );
  root.querySelector<HTMLButtonElement>('.model-picker-trigger')!.click();
  assert.equal(root.querySelector('.model-picker-sources'), null);
  assert.deepEqual(options(root), ['OpenAI/openai/gpt-5']);
});
