import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import type { Provider } from '../types';
import ModelProvidersSettings from './ModelProvidersSettings';
import { resolveProfile } from '../lib/aiSettings';

const provider = (): Provider => ({
  id: 'provider',
  name: 'Provider',
  baseUrl: 'https://provider.example/v1',
  apiKey: '',
  stream: true,
  models: [
    { id: 'paid', enabled: true },
    { id: 'free', enabled: true },
  ],
});
const button = (root: HTMLElement, text: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')].find(
    (entry) => entry.textContent === text
  )!;
const input = (root: HTMLElement, label: string) =>
  root.querySelector<HTMLInputElement>(`input[aria-label="${label} per million tokens"]`)!;
function price(root: HTMLElement, label: string, value: string) {
  const element = input(root, label);
  element.value = value;
  element.dispatchEvent(new Event('change', { bubbles: true }));
}
function pick(root: HTMLElement, model: string) {
  const element = root.querySelector<HTMLSelectElement>(
    'select[aria-label="Model for verification pricing"]'
  )!;
  element.value = model;
  element.dispatchEvent(new Event('change', { bubbles: true }));
}

test('provider settings save reusable prices per model and preserve an explicitly free model', () => {
  const [providers, setProviders] = createSignal([provider()]);
  const root = container();
  const dispose = render(
    () => <ModelProvidersSettings providers={providers()} onChange={setProviders} />,
    root
  );
  try {
    button(root, 'Edit').click();
    assert.equal(root.querySelector('details')?.open, false, 'advanced pricing starts collapsed');
    assert.match(root.textContent!, /price per million tokens in its billing currency/);
    assert.doesNotMatch(root.textContent!, /microunits/);
    price(root, 'Input price', '2.50');
    price(root, 'Cached input price', '0.5');
    price(root, 'Output price', '8.000001');
    assert.equal(providers()[0].models[0].harnessRates, undefined, 'the form is still a draft');
    pick(root, 'free');
    assert.equal(input(root, 'Input price').value, '');
    price(root, 'Input price', '0');
    price(root, 'Cached input price', '0');
    price(root, 'Output price', '0');
    button(root, 'Save').click();
    assert.deepEqual(
      providers()[0].models.map((model) => model.harnessRates),
      [
        { input: 2_500_000, cachedInput: 500_000, output: 8_000_001 },
        { input: 0, cachedInput: 0, output: 0 },
      ]
    );
    button(root, 'Edit').click();
    assert.equal(input(root, 'Input price').value, '2.5');
    assert.equal(input(root, 'Output price').value, '8.000001');
    pick(root, 'free');
    assert.equal(input(root, 'Output price').value, '0');
  } finally {
    dispose();
  }
});

test('missing or invalid saved prices remain unknown and cancelling edits preserves prior prices', () => {
  const initial = provider();
  initial.models[0].harnessRates = { input: 1, cachedInput: 2, output: 3 };
  const [providers, setProviders] = createSignal([initial]);
  const root = container();
  const dispose = render(
    () => <ModelProvidersSettings providers={providers()} onChange={setProviders} />,
    root
  );
  try {
    button(root, 'Edit').click();
    assert.equal(input(root, 'Input price').value, '0.000001', 'legacy storage keeps its value');
    price(root, 'Input price', '');
    button(root, 'Cancel').click();
    assert.deepEqual(providers()[0].models[0].harnessRates, {
      input: 1,
      cachedInput: 2,
      output: 3,
    });
    button(root, 'Edit').click();
    price(root, 'Cached input price', '0.0000001');
    assert.equal(input(root, 'Cached input price').value, '', 'excess precision stays unknown');
    price(root, 'Output price', '-1');
    assert.equal(input(root, 'Output price').value, '');
    button(root, 'Save').click();
    assert.deepEqual(providers()[0].models[0].harnessRates, { input: 1 });
    assert.equal(
      resolveProfile(providers(), { providerId: initial.id, modelId: 'paid' })?.harnessRates,
      undefined
    );
  } finally {
    dispose();
  }
});

test('fractional provider prices reach the model profile without changing their units', () => {
  const [providers, setProviders] = createSignal([provider()]);
  const root = container();
  const dispose = render(
    () => <ModelProvidersSettings providers={providers()} onChange={setProviders} />,
    root
  );
  try {
    button(root, 'Edit').click();
    assert.equal(input(root, 'Input price').type, 'text', 'preserve decimal text before parsing');
    price(root, 'Input price', '0.225');
    price(root, 'Cached input price', '0.0045');
    price(root, 'Output price', '0.90');
    button(root, 'Save').click();
    const profile = resolveProfile(providers(), { providerId: 'provider', modelId: 'paid' });
    assert.deepEqual(profile?.harnessRates, {
      input: 225_000,
      cachedInput: 4_500,
      output: 900_000,
    });
    button(root, 'Edit').click();
    assert.equal(input(root, 'Input price').value, '0.225');
    assert.equal(input(root, 'Cached input price').value, '0.0045');
    assert.equal(input(root, 'Output price').value, '0.9');
  } finally {
    dispose();
  }
});
