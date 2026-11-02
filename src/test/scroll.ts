// scroll.ts
//
// What a component asked to be scrolled into view, for tests. happy-dom has
// no layout, so nothing actually scrolls: scrollIntoView is a no-op there
// (nodes/element/Element.js), and a component that follows the picked row
// can only be checked by what it asked to reveal.
// Import it after ../test/dom (which installs happy-dom's globals).

export interface RevealedScrolls {
  /** Each element scrollIntoView was called on, in call order. */
  revealed: Element[];
  /** Puts the real scrollIntoView back. */
  restore: () => void;
}

/** Records every scrollIntoView call under the returned `revealed`. */
export function recordScrollIntoView(): RevealedScrolls {
  const revealed: Element[] = [];
  const original = Element.prototype.scrollIntoView;
  Element.prototype.scrollIntoView = function (this: Element) {
    revealed.push(this);
  };
  return {
    revealed,
    restore: () => {
      Element.prototype.scrollIntoView = original;
    },
  };
}
