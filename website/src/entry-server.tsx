import { generateHydrationScript, renderToString } from 'solid-js/web';
import App from './App';

/** The page as HTML, for scripts/prerender.mjs. */
export function render(): { html: string; head: string } {
  return { html: renderToString(() => <App />), head: generateHydrationScript() };
}
