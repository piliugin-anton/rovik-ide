// A DOM for component tests (scripts/test.mjs): happy-dom's window, its
// globals installed before any component module is evaluated. Import it
// first in a *.test.tsx file.
import { Window } from 'happy-dom';
import { after } from 'node:test';

const window = new Window({ url: 'http://localhost/' });
// Failed assertions can leave a component's timers alive. Stop them after the
// file's tests so Node can flush every result and exit without being forced.
after(() => window.happyDOM.abort());
const globals = [
  'window',
  'document',
  'navigator',
  'Node',
  'Element',
  'HTMLElement',
  // Solid's Portal (ConfirmDialog) checks for it.
  'HTMLHeadElement',
  'SVGElement',
  'Text',
  'Comment',
  'DocumentFragment',
  // The composer walks its text nodes (ChatPanel's findTextOffset).
  'NodeFilter',
  'Event',
  'MouseEvent',
  'KeyboardEvent',
  'MutationObserver',
  'ResizeObserver',
  'IntersectionObserver',
  'getComputedStyle',
  'requestAnimationFrame',
  'cancelAnimationFrame',
  'localStorage',
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
