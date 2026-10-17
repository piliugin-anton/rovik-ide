// The panel's full-window toggle: the floating button (criteria 1, 2 and 6),
// the `100vw` width it puts on the inner panel (criterion 2), the instant
// width while the divider is dragged (criterion 3) and the drag that does
// nothing while expanded (criterion 4). Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import Resizable from './ResizableDiv';

const button = (root: HTMLElement) =>
  root.querySelector<HTMLButtonElement>('.sidebar-expand-button');
const inner = (root: HTMLElement) => root.querySelector<HTMLElement>('.resizable-inner');
const handle = (root: HTMLElement) => root.querySelector<HTMLElement>('.resizable-handle');

/** The inline width of the panel, as the browser has it. */
const width = (root: HTMLElement) => inner(root)!.style.width;

/** A pixel width's number. */
const px = (value: string) => Number(value.replace(/px$/, ''));

/** A press on an element, the way a webview delivers it. */
const press = (el: Element) =>
  el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));

/** The height the button is announced with. */
const label = (root: HTMLElement) => ({
  title: button(root)!.getAttribute('title'),
  ariaLabel: button(root)!.getAttribute('aria-label'),
});

/**
 * A move of the pointer: the panel listens for it on the window, and, should
 * an implementation listen on the document instead, on that too — only when
 * the window move changed nothing, so a move never counts twice.
 */
const moveTo = (root: HTMLElement, clientX: number) => {
  const before = width(root);
  window.dispatchEvent(new MouseEvent('mousemove', { clientX, bubbles: true }));
  if (width(root) === before)
    document.dispatchEvent(new MouseEvent('mousemove', { clientX, bubbles: true }));
};

/** Drags the divider from `from` to `to`. */
const drag = (root: HTMLElement, from: number, to: number) => {
  const el = handle(root);
  assert.ok(el, 'the panel has a divider handle');
  el!.dispatchEvent(
    new MouseEvent('mousedown', { clientX: from, bubbles: true, cancelable: true })
  );
  moveTo(root, to);
  window.dispatchEvent(new MouseEvent('mouseup', { clientX: to, bubbles: true }));
  document.dispatchEvent(new MouseEvent('mouseup', { clientX: to, bubbles: true }));
};

test('criterion 1: the panel shows a floating button at its right edge, labelled "Expand panel"', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" maxWidth={480}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    const b = button(root);
    assert.ok(b, 'the expand button is rendered');
    assert.equal(b!.tagName, 'BUTTON');
    assert.equal(b!.getAttribute('type'), 'button');
    assert.deepEqual(label(root), { title: 'Expand panel', ariaLabel: 'Expand panel' });
    assert.equal(b!.getAttribute('aria-pressed'), 'false');
    // Floating over the content: the panel's last child, its children kept.
    assert.ok(inner(root)!.contains(b), 'the button is inside the panel');
    assert.equal(inner(root)!.lastElementChild, b, 'the button comes last in the panel');
    assert.ok(inner(root)!.querySelector('.panel-content'), 'the children are still rendered');
    assert.match(width(root), /^\d+px$/, 'collapsed, the panel keeps a pixel width');
  } finally {
    dispose();
  }
});

test('criterion 1: expanded, the button offers to collapse', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" expanded={true}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    assert.deepEqual(label(root), { title: 'Collapse panel', ariaLabel: 'Collapse panel' });
    assert.equal(button(root)!.getAttribute('aria-pressed'), 'true');
  } finally {
    dispose();
  }
});

test('criterion 2: a click reports the expansion it asks for', () => {
  const root = container();
  const reported: boolean[] = [];
  const dispose = render(
    () => (
      <Resizable expanded={false} onExpandedChange={(value) => reported.push(value)}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    press(button(root)!);
    assert.deepEqual(reported, [true], 'the click asks for the expanded panel');
    // The width follows `expanded` only: the panel itself does not widen.
    assert.match(width(root), /^\d+px$/, 'the width is untouched until the parent expands');
  } finally {
    dispose();
  }
});

test('criterion 2: expanded, the inner panel is 100vw wide and carries the expanded class', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" expanded={true}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    assert.equal(width(root), '100vw', 'the panel fills the window');
    assert.ok(inner(root)!.classList.contains('expanded'), 'the inner panel marks itself expanded');
  } finally {
    dispose();
  }
});

test('criterion 2: collapsed, the inner panel keeps a pixel width and no expanded class', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" expanded={false} maxWidth={480}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    assert.match(width(root), /^\d+px$/, 'collapsed, the panel keeps a pixel width');
    assert.equal(inner(root)!.classList.contains('expanded'), false);
  } finally {
    dispose();
  }
});

test('criterion 3: a drag sets a pixel width and marks the panel as resizing meanwhile', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" maxWidth={800}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    const start = px(width(root));
    assert.ok(Number.isFinite(start), `the panel starts at a pixel width (${width(root)})`);
    const el = handle(root);
    assert.ok(el, 'the panel has a divider handle');
    el!.dispatchEvent(
      new MouseEvent('mousedown', { clientX: 300, bubbles: true, cancelable: true })
    );
    assert.ok(inner(root)!.classList.contains('resizing'), 'a drag turns the transition off');
    moveTo(root, 340);
    window.dispatchEvent(new MouseEvent('mouseup', { clientX: 340, bubbles: true }));
    assert.equal(inner(root)!.classList.contains('resizing'), false, 'the drag is over');
    const expected = Math.max(100, Math.min(start + 40, 800));
    assert.equal(px(width(root)), expected, 'the divider drags the width 40px right, clamped');
    assert.match(width(root), /^\d+px$/, 'a dragged width is a pixel length');
  } finally {
    dispose();
  }
});

test('criterion 4: a mousedown on the handle while expanded starts no drag', () => {
  const root = container();
  const dispose = render(
    () => (
      <Resizable class="sidebar" expanded={true} maxWidth={800}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    assert.equal(width(root), '100vw');
    const el = handle(root);
    if (el) {
      el.dispatchEvent(
        new MouseEvent('mousedown', { clientX: 300, bubbles: true, cancelable: true })
      );
      moveTo(root, 500);
      assert.equal(inner(root)!.classList.contains('resizing'), false, 'no drag is under way');
      window.dispatchEvent(new MouseEvent('mouseup', { clientX: 500, bubbles: true }));
    }
    assert.equal(width(root), '100vw', 'the panel is still the full window');
  } finally {
    dispose();
  }
});

test('criterion 6: collapsing returns the panel to the width it had before expanding', () => {
  const root = container();
  const [expanded, setExpanded] = createSignal(false);
  const dispose = render(
    () => (
      <Resizable expanded={expanded()} onExpandedChange={setExpanded} maxWidth={800}>
        <div class="panel-content">files</div>
      </Resizable>
    ),
    root
  );
  try {
    // The width the user chose with the divider.
    drag(root, 300, 360);
    const chosen = width(root);
    assert.match(chosen, /^\d+px$/, `a dragged width (${chosen})`);

    press(button(root)!);
    assert.equal(expanded(), true);
    assert.equal(width(root), '100vw');
    assert.ok(inner(root)!.classList.contains('expanded'));
    assert.deepEqual(label(root), { title: 'Collapse panel', ariaLabel: 'Collapse panel' });

    press(button(root)!);
    assert.equal(expanded(), false);
    assert.equal(width(root), chosen, 'the width from before expanding is back');
    assert.equal(inner(root)!.classList.contains('expanded'), false);
    assert.deepEqual(label(root), { title: 'Expand panel', ariaLabel: 'Expand panel' });
  } finally {
    dispose();
  }
});
