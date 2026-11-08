// composerCaret.ts
//
// Keeping the composer's caret in view. The input is a box that grows to a
// maximum height and then scrolls (styles.css: .chat-input-contenteditable,
// max-height 200px), and text put in by script — a paste, a drop, a picked
// file's chip — leaves the box's scroll where it was, so the caret can end up
// below its bottom edge, out of sight. The browser moves the scroll for text
// typed by hand; this puts it back on the caret after an insert.

/**
 * Scrolls `container` just enough to show the caret, and leaves an already
 * visible caret where it is. Called after an insert, so it waits for the
 * layout of what was inserted (`setTimeout`, as the `@` picker's chip did).
 */
export function revealCaret(container: HTMLElement): void {
  setTimeout(() => {
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return;
    const range = selection.getRangeAt(0);
    if (!container.contains(range.commonAncestorContainer)) return;

    // getClientRects() returns nothing for a collapsed range, so measure the
    // range itself: a caret at the end of the composer sits on its last line.
    const caret = range.getBoundingClientRect();
    const box = container.getBoundingClientRect();
    // Either edge: the box's own scroll can be anywhere the caret isn't (the
    // user scrolled to read the rest of a long draft without clicking).
    if (caret.bottom > box.bottom) container.scrollTop += caret.bottom - box.bottom;
    else if (caret.top < box.top) container.scrollTop -= box.top - caret.top;
  }, 0);
}
