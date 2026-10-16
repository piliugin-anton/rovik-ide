#!/usr/bin/env node
// Adds the end-to-end bridge (bridge-page.js) to the built frontend, for a
// build the runner drives where the webview has no WebDriver (macOS:
// WKWebView). Never part of a normal build: bridge.tauri.conf.json runs it
// after `npm run build`, and lets the page reach the runner:
//
//   npx tauri build --config scripts/e2e/bridge.tauri.conf.json
//   node scripts/e2e/run.mjs --driver bridge --app <the built binary> ...
//
//   node scripts/e2e/inject-bridge.mjs [dist-dir]   (default: dist)
// The runner's port is E2E_BRIDGE_PORT (17999, what the config allows).
import { copyFileSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const dist = resolve(process.argv[2] ?? 'dist');
const port = process.env.E2E_BRIDGE_PORT ?? '17999';

copyFileSync(join(HERE, 'bridge-page.js'), join(dist, 'e2e-bridge.js'));
const index = join(dist, 'index.html');
const html = readFileSync(index, 'utf8');
if (!html.includes('e2e-bridge.js')) {
  // First in <head>: connected before the app's own scripts run.
  const tag = `<script src="/e2e-bridge.js" data-port="${port}"></script>`;
  if (!/<head[^>]*>/i.test(html)) throw new Error(`no <head> in ${index}`);
  writeFileSync(
    index,
    html.replace(/<head[^>]*>/i, (head) => `${head}\n    ${tag}`)
  );
}
console.log(`e2e bridge added to ${index} (port ${port})`);
