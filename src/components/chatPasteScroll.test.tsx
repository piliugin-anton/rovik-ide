// Component tests: where the composer's scroll sits after content goes into it
// by script. The input grows from 60px to its 200px maximum and then scrolls
// (styles.css), and content a script inserts — a paste, a copy with a chip, a
// picked `@` file — does not move that scroll by itself, so the caret could
// end up below the box. The panel now asks the composer to scroll to the
// caret after such an insert. happy-dom has no layout: the composer's and the
// caret's rectangles are faked here and put back after each test (the way
// test/scroll.ts patches a prototype and restores it). Run with: npm test
import { container } from '../test/dom';
import { composer, flush, installTauri, placeCaret, press, typeText } from '../test/chatPanel';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';

// A few files, so the `@` list has a row to pick.
const FILES = ['src/app.ts', 'src/chat.ts', 'src/main.ts'];
installTauri(FILES);

/** The chat panel as the app mounts it, settled. */
async function show() {
  localStorage.clear();
  const root = container();
  const dispose = render(
    () => (
      <ChatPanel
        rootPath={() => '/p'}
        activeFileContext={() => null}
        files={() => []}
        projectId={() => 'project-1'}
      />
    ),
    root
  );
  await flush();
  return { root, dispose };
}

/** A rectangle with the given edges; the composer's code reads top and bottom. */
const rect = (top: number, bottom: number) =>
  ({
    top,
    bottom,
    height: bottom - top,
    left: 0,
    right: 0,
    width: 0,
    x: 0,
    y: top,
    toJSON: () => ({}),
  }) as DOMRect;

/** The composer at its 200px maximum. */
const BOX = rect(0, 200);
/** A caret 120px past the box's bottom edge: the scroll it asks for. */
const BELOW_THE_BOX = { top: 300, bottom: 320 };
/** A caret the box shows already. */
const IN_THE_BOX = { top: 160, bottom: 180 };

/**
 * Gives happy-dom the layout it lacks: `BOX` for the composer element and
 * `caret` for every range (a contenteditable caret is a range). Restore in a
 * finally: both the prototype and the element are shared.
 */
function fakeLayout(box: HTMLElement, caret: { top: number; bottom: number }) {
  const rangeProto = Object.getPrototypeOf(document.createRange()) as {
    getBoundingClientRect: () => DOMRect;
  };
  const realRangeRect = rangeProto.getBoundingClientRect;
  const ownBoxRect = Object.getOwnPropertyDescriptor(box, 'getBoundingClientRect');
  box.getBoundingClientRect = () => BOX;
  rangeProto.getBoundingClientRect = () => rect(caret.top, caret.bottom);
  return {
    restore: () => {
      rangeProto.getBoundingClientRect = realRangeRect;
      if (ownBoxRect) Object.defineProperty(box, 'getBoundingClientRect', ownBoxRect);
      else delete (box as { getBoundingClientRect?: unknown }).getBoundingClientRect;
    },
  };
}

/** Stubs the webview's `document.execCommand`: `run` says whether it did it. */
function stubExecCommand(run: (command: string, text: string) => boolean) {
  const doc = document as Document & {
    execCommand: (command: string, showUi?: boolean, value?: string) => boolean;
  };
  const saved = Object.getOwnPropertyDescriptor(doc, 'execCommand');
  doc.execCommand = (command: string, _showUi?: boolean, value?: string) =>
    run(command, value ?? '');
  return () => {
    if (saved) Object.defineProperty(doc, 'execCommand', saved);
    else delete (doc as { execCommand?: unknown }).execCommand;
  };
}

/** Pastes a clipboard holding `data` into the composer, then lets the panel settle. */
async function paste(root: HTMLElement, data: Record<string, string>) {
  const transfer = new window.DataTransfer();
  for (const [type, value] of Object.entries(data)) transfer.setData(type, value);
  const event = new window.ClipboardEvent('paste', { clipboardData: transfer, bubbles: true });
  composer(root).dispatchEvent(event);
  await flush();
}

/** How many times `part` occurs in `text`. */
const occurrences = (text: string, part: string) => text.split(part).length - 1;

const LINES = Array.from({ length: 15 }, (_, i) => `line ${i + 1} of the paste`);
/** Taller than the box: 15 lines, few enough to stay text. */
const PASTE = LINES.join('\n');
/** A line from the middle, looked for once. */
const MIDDLE = LINES[7];

/** What a copy of a chip pastes back as: the app's own clipboard type. */
const SEGMENTS_TYPE = 'application/x-rovik-segments+json';
const CHIP_COPY = JSON.stringify([
  { type: 'text', text: 'look at this ' },
  { type: 'pasted', text: 'pasted body\nsecond line' },
]);

test('a paste taller than the input scrolls it down to the caret', async () => {
  const { root, dispose } = await show();
  // The webview's own command does nothing here: the panel inserts the text itself.
  const putBack = stubExecCommand(() => false);
  try {
    const box = composer(root);
    placeCaret(box);
    const layout = fakeLayout(box, BELOW_THE_BOX);
    try {
      await paste(root, { 'text/plain': PASTE });
    } finally {
      layout.restore();
    }
    assert.equal(
      occurrences(box.textContent ?? '', MIDDLE),
      1,
      'the pasted lines are in the composer, once'
    );
    assert.equal(box.scrollTop, 120, 'the box scrolled so the caret sits at its bottom edge');
  } finally {
    putBack();
    dispose();
  }
});

test("the webview's own insert puts the paste in once and scrolls to the caret", async () => {
  const { root, dispose } = await show();
  try {
    const box = composer(root);
    placeCaret(box);
    // The webview's `insertText` succeeds: it writes the text at the caret and
    // says the input changed, as the real command does.
    const putBack = stubExecCommand((command, text) => {
      if (command !== 'insertText') return false;
      box.textContent = (box.textContent ?? '') + text;
      placeCaret(box, box.childNodes.length);
      box.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    });
    const layout = fakeLayout(box, BELOW_THE_BOX);
    try {
      await paste(root, { 'text/plain': PASTE });
    } finally {
      layout.restore();
      putBack();
    }
    assert.equal(
      occurrences(box.textContent ?? '', MIDDLE),
      1,
      'the webview inserted it: the panel does not insert it a second time'
    );
    assert.equal(box.scrollTop, 120, 'the box scrolled so the caret sits at its bottom edge');
  } finally {
    dispose();
  }
});

test('pasting a copy with a chip scrolls the input down to the caret', async () => {
  const { root, dispose } = await show();
  const putBack = stubExecCommand(() => false);
  try {
    const box = composer(root);
    placeCaret(box);
    const layout = fakeLayout(box, BELOW_THE_BOX);
    try {
      await paste(root, { [SEGMENTS_TYPE]: CHIP_COPY, 'text/plain': 'look at this [pasted]' });
    } finally {
      layout.restore();
    }
    assert.match(box.textContent ?? '', /look at this/, 'the copy’s text is in the composer');
    assert.ok(
      box.querySelectorAll('*').length > 0,
      'the chip went in as an element of its own, not as words'
    );
    assert.equal(box.scrollTop, 120, 'the box scrolled so the caret sits at its bottom edge');
  } finally {
    putBack();
    dispose();
  }
});

test('picking a row of the @ file list keeps its chip in view', async () => {
  const { root, dispose } = await show();
  const putBack = stubExecCommand(() => false);
  try {
    // "@" offers the project's files, the first row picked; Enter takes it.
    await typeText(root, '@');
    const box = composer(root);
    const layout = fakeLayout(box, BELOW_THE_BOX);
    try {
      await press(root, 'Enter');
    } finally {
      layout.restore();
    }
    assert.match(
      box.textContent ?? '',
      /app\.ts|chat\.ts|main\.ts/,
      'the picked file is in the composer as its chip'
    );
    assert.equal(box.scrollTop, 120, 'the box scrolled down to the chip’s caret');
  } finally {
    putBack();
    dispose();
  }
});

test('a paste the input has room for leaves its scroll alone', async () => {
  const { root, dispose } = await show();
  const putBack = stubExecCommand(() => false);
  try {
    const box = composer(root);
    placeCaret(box);
    box.scrollTop = 40;
    const layout = fakeLayout(box, IN_THE_BOX);
    try {
      await paste(root, { 'text/plain': 'a short line' });
    } finally {
      layout.restore();
    }
    assert.match(box.textContent ?? '', /a short line/, 'the line is in the composer');
    assert.equal(box.scrollTop, 40, 'the caret was in view: the scroll stays where it was');
  } finally {
    putBack();
    dispose();
  }
});
