// On Linux, releasing the middle button pastes the PRIMARY selection into
// whatever element has focus, even when the click landed somewhere else. So
// middle-clicking an editor tab while the chat input is focused would paste
// the last selected text into the chat. Cancel that paste unless the click
// itself was on something editable, where pasting is the expected behavior.

// Monaco and xterm handle middle clicks themselves.
const EDITABLE =
  'input, textarea, [contenteditable]:not([contenteditable="false"]), .monaco-editor, .xterm';

function onMouseUp(e: MouseEvent) {
  if (e.button !== 1) return;
  const target = e.target instanceof Element ? e.target : null;
  if (target?.closest(EDITABLE)) return;
  e.preventDefault();
}

export function installMiddleClickPasteGuard() {
  window.addEventListener('mouseup', onMouseUp, true);
}
