// Check the shared layout and action colors with the real stylesheet.
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { render } from 'solid-js/web';
import { SettingRow } from './SettingsControls';

function withStyles(run: () => void) {
  const style = document.createElement('style');
  style.textContent = readFileSync('src/styles.css', 'utf8');
  document.head.append(style);
  try {
    run();
  } finally {
    style.remove();
  }
}

test('help text in a setting row aligns with the control rather than adding a second indent', () => {
  withStyles(() => {
    const root = container();
    const dispose = render(
      () => (
        <section class="settings-section">
          <SettingRow label="Slots">
            <p class="setting-explain">Auto gives one slot.</p>
          </SettingRow>
        </section>
      ),
      root
    );
    try {
      assert.equal(getComputedStyle(root.querySelector('p')!).marginLeft, '0px');
    } finally {
      dispose();
      root.remove();
    }
  });
});

test('settings button defaults preserve primary action colors', () => {
  withStyles(() => {
    const root = container();
    const dispose = render(
      () => (
        <div class="modal model-settings">
          <button class="chat-action-button primary">Install</button>
          <div class="profile-form-actions">
            <button class="primary">Save</button>
          </div>
        </div>
      ),
      root
    );
    try {
      const accent = getComputedStyle(document.documentElement).getPropertyValue('--accent').trim();
      for (const button of root.querySelectorAll('button')) {
        assert.equal(getComputedStyle(button).backgroundColor, accent, button.textContent!);
      }
    } finally {
      dispose();
      root.remove();
    }
  });
});
