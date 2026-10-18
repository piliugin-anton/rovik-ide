// appSettings.ts tests (node:test): another window may have saved the AI
// settings; this one reads them again when it comes to the front, but only
// once their file was written since it last looked (reading them reads the
// keychain).
import { test } from 'node:test';
import assert from 'node:assert/strict';

(globalThis as any).window = new EventTarget();
const { aiSettingsRevision, rereadAiSettingsOnFocus } = await import('./appSettings');

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const focus = async () => {
  window.dispatchEvent(new Event('focus'));
  await settle();
};

test('the settings are read again on focus once another window saved them', async () => {
  const stamps = [100, 100, 250, 250, 300];
  const stop = rereadAiSettingsOnFocus(async () => stamps.shift()!);
  await settle();
  const at = aiSettingsRevision();
  await focus();
  assert.equal(aiSettingsRevision(), at, 'not written since: not read again');
  await focus();
  assert.equal(aiSettingsRevision(), at + 1, 'written since: read again');
  await focus();
  assert.equal(aiSettingsRevision(), at + 1);
  stop();
  await focus();
  assert.equal(aiSettingsRevision(), at + 1, 'not looked at once stopped');
});

test('a stamp that can’t be read reads nothing again', async () => {
  const stop = rereadAiSettingsOnFocus(async () => {
    throw new Error('no app');
  });
  await settle();
  const at = aiSettingsRevision();
  await focus();
  assert.equal(aiSettingsRevision(), at);
  stop();
});
