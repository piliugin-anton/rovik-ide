// shortcutKeys.ts
//
// Shortcuts pressed on a keyboard layout without Latin letters (Russian,
// Greek, Hebrew…). There Ctrl+S types "ы": the key event has key "ы", code
// "KeyS" (the key's place) and, in WebKitGTK (the Linux webview), keyCode
// 0. Monaco matches its shortcuts by keyCode, so while such a layout was
// on, Ctrl+S, Ctrl+Z, Ctrl+F… did nothing in the editor. Chromium (WebView2
// on Windows) gives such a press the US key code of the key's place, which
// Monaco's shortcuts then match: installShortcutKeys does the same for
// WebKitGTK. The app's own shortcuts, which read `key`, use shortcutKey.

/** The US key code of the letter or digit key at `code` ("KeyS": 83); null for other keys. */
function usKeyCode(code: string): number | null {
  const match = /^(?:Key([A-Z])|Digit([0-9]))$/.exec(code);
  return match ? (match[1] ?? match[2]).charCodeAt(0) : null;
}

/**
 * A key press the webview gave no key code (a letter on a non-Latin
 * layout, in WebKitGTK) takes the US key code of its key, as in Chromium.
 */
export function fillKeyCode(e: KeyboardEvent): void {
  if (e.keyCode !== 0) return;
  const keyCode = usKeyCode(e.code);
  if (keyCode !== null) Object.defineProperty(e, 'keyCode', { value: keyCode, configurable: true });
}

/** Fills the key codes of key presses in the window, before anything else reads them. */
export function installShortcutKeys(): void {
  window.addEventListener('keydown', fillKeyCode, true);
}

/**
 * The letter or digit a shortcut was pressed with, lowercase: the key
 * typed, or on a layout without Latin letters the US key at its place.
 * Any other key: its `key`, lowercase.
 */
export function shortcutKey(e: KeyboardEvent): string {
  if (/^[a-z0-9]$/i.test(e.key)) return e.key.toLowerCase();
  const keyCode = usKeyCode(e.code);
  return keyCode === null ? e.key.toLowerCase() : String.fromCharCode(keyCode).toLowerCase();
}
