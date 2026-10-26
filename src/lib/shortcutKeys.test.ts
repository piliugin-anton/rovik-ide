// Shortcuts on keyboard layouts without Latin letters (lib/shortcutKeys.ts):
// a Russian Ctrl+S reaches the page as key "ы", code "KeyS" and, in
// WebKitGTK, keyCode 0, which Monaco's shortcuts never matched. Run with:
// npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fillKeyCode, installShortcutKeys, shortcutKey } from './shortcutKeys';

/** A keydown as the webview reports it: `keyCode` as WebKitGTK gives it. */
function press(key: string, code: string, keyCode: number, init: KeyboardEventInit = {}) {
  const e = new KeyboardEvent('keydown', { key, code, ctrlKey: true, bubbles: true, ...init });
  Object.defineProperty(e, 'keyCode', { value: keyCode, configurable: true });
  return e;
}

test('a letter pressed on a Russian layout gets the US key code of its key', () => {
  const save = press('ы', 'KeyS', 0);
  fillKeyCode(save);
  assert.equal(save.keyCode, 83);
  const digit = press('1', 'Digit1', 0);
  fillKeyCode(digit);
  assert.equal(digit.keyCode, 49);
});

test('a key code the webview gave is kept, on Dvorak too', () => {
  // Dvorak's "o" is where a US keyboard has S: Ctrl+O there is Ctrl+O.
  const dvorak = press('o', 'KeyS', 79);
  fillKeyCode(dvorak);
  assert.equal(dvorak.keyCode, 79);
});

test('a key that is no letter or digit keeps no key code', () => {
  const other = press('ж', 'Semicolon', 0);
  fillKeyCode(other);
  assert.equal(other.keyCode, 0);
});

test('installed, the key code is there before the page reads it', () => {
  installShortcutKeys();
  const target = document.createElement('div');
  document.body.appendChild(target);
  let seen = -1;
  target.addEventListener('keydown', (e) => (seen = (e as KeyboardEvent).keyCode));
  target.dispatchEvent(press('ы', 'KeyS', 0));
  assert.equal(seen, 83);
  target.remove();
});

test("a shortcut's letter: the key typed, or on a non-Latin layout the US key at its place", () => {
  assert.equal(shortcutKey(press('s', 'KeyS', 83)), 's');
  assert.equal(shortcutKey(press('S', 'KeyS', 83, { shiftKey: true })), 's');
  assert.equal(shortcutKey(press('ы', 'KeyS', 0)), 's');
  assert.equal(shortcutKey(press('М', 'KeyV', 0, { shiftKey: true })), 'v');
  assert.equal(shortcutKey(press('o', 'KeyS', 79)), 'o');
  assert.equal(shortcutKey(press('Enter', 'Enter', 13)), 'enter');
});
