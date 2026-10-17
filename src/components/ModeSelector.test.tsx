// Component tests: the chat mode switch (Agent, Plan, Autopilot) and the
// mode it remembers. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ModeSelector from './ModeSelector';
import { chatMode, savedMode, setChatMode, setOwnModeChat } from '../lib/chatMode';

const radios = (root: HTMLElement) =>
  [...root.querySelectorAll('[role="radio"]')] as HTMLButtonElement[];

const checked = (root: HTMLElement) =>
  radios(root)
    .filter((r) => r.getAttribute('aria-checked') === 'true')
    .map((r) => r.textContent?.trim());

test('mode selector: Agent, Plan and Autopilot, one of them checked', () => {
  setChatMode('agent');
  const root = container();
  render(() => <ModeSelector />, root);
  assert.deepEqual(
    radios(root).map((r) => r.textContent?.trim()),
    ['Agent', 'Plan', 'Autopilot']
  );
  assert.deepEqual(checked(root), ['Agent']);
  const autopilot = radios(root)[2];
  assert.match(autopilot.title, /without asking/);
});

test('mode selector: choosing Autopilot is the next message’s mode, and remembered', () => {
  setChatMode('agent');
  const root = container();
  render(() => <ModeSelector />, root);
  radios(root)[2].click();
  assert.equal(chatMode(), 'autopilot');
  assert.deepEqual(checked(root), ['Autopilot']);
  assert.ok(radios(root)[2].classList.contains('autopilot'));
  assert.equal(localStorage.getItem('rovik.chatMode'), 'autopilot');
  radios(root)[1].click();
  assert.equal(chatMode(), 'plan');
  assert.equal(localStorage.getItem('rovik.chatMode'), 'plan');
});

test('chat mode: a saved mode reads back, anything else is Agent', () => {
  assert.equal(savedMode('autopilot'), 'autopilot');
  assert.equal(savedMode('plan'), 'plan');
  assert.equal(savedMode('agent'), 'agent');
  assert.equal(savedMode(null), 'agent');
  assert.equal(savedMode('turbo'), 'agent');
});

test('a chat on a host that works on its own: Autopilot until the user picks another for it', () => {
  setChatMode('agent');
  const root = container();
  render(() => <ModeSelector />, root);
  setOwnModeChat('remote-1');
  assert.equal(chatMode(), 'autopilot');
  assert.deepEqual(checked(root), ['Autopilot']);
  // Plan for this chat; the other chats keep theirs, which isn't saved over.
  radios(root)[1].click();
  assert.equal(chatMode(), 'plan');
  assert.equal(localStorage.getItem('rovik.chatMode'), 'agent');
  setOwnModeChat('remote-2');
  assert.equal(chatMode(), 'autopilot', 'another such chat starts in Autopilot');
  setOwnModeChat(null);
  assert.equal(chatMode(), 'agent');
  assert.deepEqual(checked(root), ['Agent']);
  setOwnModeChat('remote-1');
  assert.equal(chatMode(), 'plan', 'what the user picked for it');
  setOwnModeChat(null);
});
