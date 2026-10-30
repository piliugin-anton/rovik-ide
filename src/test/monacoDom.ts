// The DOM globals Monaco's editor API needs in addition to the component-test DOM.
import './dom';

for (const name of [
  'customElements',
  'CSSStyleSheet',
  'HTMLCanvasElement',
  'PointerEvent',
  'CSS',
]) {
  Object.defineProperty(globalThis, name, {
    value: (window as unknown as Record<string, unknown>)[name],
    configurable: true,
  });
}
