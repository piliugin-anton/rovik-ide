// Component tests: Settings → Autopilot (autopilot-plan 3.5): the effort,
// a task's budget, the Prepare stage and the commands a task never runs.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import AutopilotSettings from './AutopilotSettings';
import {
  DEFAULT_AUTOPILOT_DENYLIST,
  DEFAULT_AUTOPILOT_SETTINGS,
  type AutopilotSettings as Settings,
} from '../lib/autopilotSettings';
import type { SettingsPage } from '../lib/appSettings';

/** The page, keeping what it changes; `opened` is the pages its links opened. */
function page(initial: Settings = DEFAULT_AUTOPILOT_SETTINGS) {
  const root = container();
  const [settings, setSettings] = createSignal<Settings>(initial);
  const opened: SettingsPage[] = [];
  render(
    () => (
      <AutopilotSettings
        settings={settings()}
        onChange={setSettings}
        onOpenPage={(p) => opened.push(p)}
      />
    ),
    root
  );
  return { root, settings, opened };
}

/** The Effort radios, by label. */
const effortRadio = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.startsWith(label))!
    .querySelector('input[type="radio"]') as HTMLInputElement;

const checkbox = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.includes(label))!
    .querySelector('input') as HTMLInputElement;

test('autopilot settings: what the page is, and where the rest is set', () => {
  const { root, opened } = page();
  const titles = [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);
  assert.deepEqual(titles, [
    'Effort',
    'Budget per task',
    'Before the first check',
    'Commands it never runs',
    'What else a task follows',
  ]);
  // Says when these apply, and what a task takes from the other pages.
  assert.match(root.textContent!, /A chat is in Autopilot when you choose that mode/);
  assert.match(root.textContent!, /Planner \(Thorough only\)/);
  const link = (label: string) =>
    [...root.querySelectorAll('button.link-button')].find((b) => b.textContent === label)!;
  link('Subagents').click();
  link('Cloud hosts').click();
  link('Permissions & approval').click();
  assert.deepEqual(opened, ['subagents', 'hosts', 'permissions']);
});

test('autopilot settings: the effort, Balanced by default', () => {
  const { root, settings } = page();
  const balanced = effortRadio(root, 'Balanced');
  const thorough = effortRadio(root, 'Thorough');
  assert.equal(balanced.checked, true);
  assert.equal(thorough.checked, false);
  assert.match(
    balanced.closest('label')!.textContent!,
    /Each stage once, with the subagents below\./
  );
  assert.match(
    thorough.closest('label')!.textContent!,
    /Several candidates, a second model for every judgment, an adversarial tester; takes longer and costs more\./
  );
  thorough.click();
  assert.deepEqual(settings(), { effort: 'thorough' });
  assert.equal(effortRadio(root, 'Thorough').checked, true);
});

test('autopilot settings: the commands a task may never run, the defaults until changed (U01)', () => {
  const { root, settings } = page();
  const shown = () =>
    [...root.querySelectorAll('.command-row-text')].map((c) => c.textContent ?? '');
  const reset = () =>
    [...root.querySelectorAll('button')].find((b) => b.textContent === 'Reset list');
  assert.deepEqual(shown(), DEFAULT_AUTOPILOT_DENYLIST);
  assert.equal(reset(), undefined, 'nothing to reset');

  const input = root.querySelector(
    'input[aria-label="A command Autopilot may never run"]'
  ) as HTMLInputElement;
  const add = input.parentElement!.querySelector('button') as HTMLButtonElement;
  input.value = '  npm   publish ';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  add.click();
  assert.deepEqual(settings().deniedCommands, [...DEFAULT_AUTOPILOT_DENYLIST, 'npm publish']);
  assert.deepEqual(shown().slice(-1), ['npm publish']);
  assert.equal(input.value, '', 'the input is cleared');

  const remove = root.querySelector('button[aria-label="Remove git push"]') as HTMLButtonElement;
  remove.click();
  assert.ok(!settings().deniedCommands!.includes('git push'));
  assert.ok(reset(), 'a changed list can be reset');
  reset()!.click();
  assert.equal(settings().deniedCommands, undefined);
  assert.deepEqual(shown(), DEFAULT_AUTOPILOT_DENYLIST);
});

test('autopilot settings: a task’s budget, its effort’s defaults until set (U04)', () => {
  const { root, settings } = page();
  const field = (label: string) =>
    [...root.querySelectorAll('label.setting-number-field')]
      .find((l) => l.textContent?.startsWith(label))!
      .querySelector('input') as HTMLInputElement;
  const hours = () => field('Stop a task after (hours)');
  const cost = () => field('or once it cost');
  const tokens = () => field('or used (million tokens)');
  // Balanced: 2 hours and 5, no token limit.
  assert.equal(hours().value, '2');
  assert.equal(cost().value, '5');
  assert.equal(tokens().value, '');
  // Thorough: 6 hours, while it's not set.
  effortRadio(root, 'Thorough').click();
  assert.equal(hours().value, '6');
  assert.equal(cost().placeholder, 'no limit');
  assert.equal(settings().budget, undefined);
  // Set: kept, whatever the effort.
  hours().value = '1.5';
  hours().dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(settings().budget, { maxHours: 1.5, maxCost: null });
  effortRadio(root, 'Balanced').click();
  assert.equal(hours().value, '1.5');
  assert.equal(cost().value, '');
  // Emptied back to the effort's own: it follows the effort again.
  hours().value = '2';
  hours().dispatchEvent(new Event('change', { bubbles: true }));
  cost().value = '5';
  cost().dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(settings().budget, undefined);
  // Tokens, in millions on the page, kept in tokens.
  tokens().value = '2.5';
  tokens().dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(settings().budget, {
    maxHours: 2,
    maxCost: 5,
    maxTokens: 2_500_000,
  });
  assert.equal(tokens().value, '2.5');
  tokens().value = '';
  tokens().dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(settings().budget, undefined);
});

test('autopilot settings: Prepare before the first check, on until turned off (S03)', () => {
  const { root, settings } = page();
  const prepare = () => checkbox(root, 'Prepare dependencies before the first check');
  assert.equal(prepare().checked, true);
  prepare().click();
  assert.equal(settings().prepare, false);
  assert.equal(prepare().checked, false);
  // Back on: the default again, kept as nothing.
  prepare().click();
  assert.equal(settings().prepare, undefined);
  assert.equal(prepare().checked, true);
});
