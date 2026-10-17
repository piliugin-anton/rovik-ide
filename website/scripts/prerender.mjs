// Renders the page into dist/index.html, so that it shows before (and
// without) JavaScript; the browser then hydrates it. Runs after both
// builds in `npm run build`: the client's in dist/, the server's in
// dist-ssr/, which it deletes when it's done.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const serverDir = path.join(root, 'dist-ssr');
const page = path.join(root, 'dist/index.html');

const { render } = await import(pathToFileURL(path.join(serverDir, 'entry-server.js')).href);
const { html, head } = render();

let template = fs.readFileSync(page, 'utf8');
for (const mark of ['<!--app-head-->', '<!--app-html-->']) {
  if (!template.includes(mark)) throw new Error(`dist/index.html has no ${mark}`);
}

// The heading's font, asked for with the page rather than once the
// stylesheet is read: the hero's text doesn't change face as it loads.
const assets = fs.readdirSync(path.join(root, 'dist/assets'));
const font = assets.find((f) => /^inter-latin-wght-normal-.*\.woff2$/.test(f));
const base = template.match(/<script type="module" crossorigin src="([^"]*)assets\//)?.[1] ?? '/';
const preload = font
  ? `<link rel="preload" href="${base}assets/${font}" as="font" type="font/woff2" crossorigin />`
  : '';

template = template.replace('<!--app-head-->', `${preload}${head}`);
template = template.replace('<!--app-html-->', html);
fs.writeFileSync(page, template);
fs.rmSync(serverDir, { recursive: true, force: true });
console.log(`Prerendered dist/index.html (${(Buffer.byteLength(template) / 1024).toFixed(1)} kB)`);
