import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  // The site lives at the root of rovik-ide.dev. Without a custom domain,
  // GitHub Pages serves it under /<repository>/: the Pages workflow passes
  // that path in BASE_PATH.
  base: process.env.BASE_PATH || '/',
  // Prerendered, then hydrated: `npm run build` renders the page to HTML
  // (scripts/prerender.mjs), and the browser picks it up from there.
  plugins: [solid({ ssr: true })],
});
