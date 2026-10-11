// A DOM for component tests (scripts/test.mjs): happy-dom's window, its
// globals installed before any component module is evaluated. Import it
// first in a *.test.tsx file.
import { Window } from 'happy-dom';

const window = new Window({ url: 'http://localhost/' });
const globals = [
  'window',
  'document',
  'navigator',
  'Node',
  'Element',
  'HTMLElement',
  'SVGElement',
  'Text',
  'Comment',
  'DocumentFragment',
  'Event',
  'MouseEvent',
  'KeyboardEvent',
  'MutationObserver',
  'ResizeObserver',
  'IntersectionObserver',
  'getComputedStyle',
  'requestAnimationFrame',
  'cancelAnimationFrame',
] as const;
for (const name of globals) {
  const value = name === 'window' ? window : (window as unknown as Record<string, unknown>)[name];
  // Some (navigator) are getters on Node's global: defined over, not assigned.
  Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
}

/** A fresh container in the document, for one render. */
export function container(): HTMLElement {
  const el = document.createElement('div');
  document.body.appendChild(el);
  return el as unknown as HTMLElement;
}
