// aiSettings.ts tests (node:test). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadAiSettings } from './aiSettings';

test('settings that could not be read are marked, not passed off as saved', async () => {
  // No app here: reading the file fails.
  const original = console.error;
  console.error = () => {};
  try {
    const settings = await loadAiSettings();
    assert.equal(settings.loadFailed, true);
    assert.equal(settings.subagents.enabled, false, 'the defaults stand in');
  } finally {
    console.error = original;
  }
});
