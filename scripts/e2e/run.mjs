#!/usr/bin/env node
// End-to-end run of the built app: launches it, clicks and types through
// its main scenarios (files, editor, search, terminal, Git, settings, the
// AI chat against a scripted fake model) and writes a report with a
// screenshot per step.
//
//   node scripts/e2e/run.mjs --app <binary> --out <dir> [--driver webdriver|bridge]
//        [--tauri-driver <path>] [--native-driver <path>] [--only <regex>]
//
// webdriver (Linux, Windows): starts tauri-driver, which starts the app
// through WebKitWebDriver or msedgedriver. bridge (macOS, where WKWebView
// has no WebDriver): the app must be built with the bridge injected
// (inject-bridge.mjs); it's launched directly and driven over HTTP.
// On Linux with xdotool, clicks and drags are real X events.
//
// Exit code 1 when a step failed; report.md and report.json are in --out.
import { spawn, execFileSync, spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
  appendFileSync,
} from 'node:fs';
import { basename, join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebDriver, Keys, sleep } from './webdriver.mjs';
import { BridgeDriver } from './bridge.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const args = parseArgs(process.argv.slice(2));
const PLATFORM = process.platform;
const OUT = resolve(args.out ?? 'e2e-out');
const SHOTS = join(OUT, 'shots');
const WORK = join(OUT, 'work');
const PROJECTS = join(WORK, 'projects');
const DEMO = join(PROJECTS, 'demo');
const MODEL_PORT = Number(args['model-port'] ?? 18080);
const ONLY = args.only ? new RegExp(args.only, 'i') : null;
const DRIVER = args.driver ?? (PLATFORM === 'darwin' ? 'bridge' : 'webdriver');
const APP = resolve(args.app ?? '');

function parseArgs(list) {
  const out = {};
  for (let i = 0; i < list.length; i++) {
    const a = list[i];
    if (!a.startsWith('--')) continue;
    const key = a.slice(2);
    const next = list[i + 1];
    if (next === undefined || next.startsWith('--')) out[key] = true;
    else {
      out[key] = next;
      i++;
    }
  }
  return out;
}

rmSync(OUT, { recursive: true, force: true });
mkdirSync(SHOTS, { recursive: true });
const LOG = join(OUT, 'run.log');
function log(...parts) {
  const line = `[${new Date().toISOString().slice(11, 19)}] ${parts.join(' ')}`;
  console.log(line);
  appendFileSync(LOG, `${line}\n`);
}

const report = {
  platform: PLATFORM,
  arch: process.arch,
  driver: DRIVER,
  app: APP,
  startedAt: new Date().toISOString(),
  steps: [],
  notes: [],
};
const note = (text) => {
  report.notes.push(text);
  log(`note: ${text}`);
};

// --- the project the app opens --------------------------------------------

function git(cwd, ...a) {
  return execFileSync('git', a, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function write(path, text) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

function setupProject() {
  mkdirSync(PROJECTS, { recursive: true });
  write(
    join(DEMO, 'package.json'),
    JSON.stringify(
      { name: 'demo', version: '1.0.0', type: 'module', scripts: { test: 'node --test' } },
      null,
      2
    ) + '\n'
  );
  write(
    join(DEMO, 'tsconfig.json'),
    '{ "compilerOptions": { "target": "ES2020", "module": "ESNext", "strict": true, "noEmit": true }, "include": ["src"] }\n'
  );
  write(
    join(DEMO, 'src', 'main.ts'),
    "import { add, greet } from './lib/math';\n\nexport function main(): void {\n  const total = add(2, 3);\n  console.log(greet('world'), total);\n}\n\nmain();\n"
  );
  write(
    join(DEMO, 'src', 'lib', 'math.ts'),
    '/** Adds two numbers. */\nexport function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n'
  );
  write(
    join(DEMO, 'README.md'),
    '# Demo project\n\nA small project for testing **Rovik IDE**.\n\n- [x] Done task\n- [ ] Open task\n\n```ts\nconst x: number = 1;\n```\n\nSee [the math module](src/lib/math.ts).\n'
  );
  write(join(DEMO, 'docs', 'notes.md'), '# Notes\n\n| a | b |\n|---|---|\n| 1 | 2 |\n');
  write(join(DEMO, 'notes.txt'), 'line one\nline two\nfind me here\n');
  write(
    join(DEMO, 'long.ts'),
    Array.from({ length: 300 }, (_, i) => `// line ${i + 1}: long enough to scroll`).join('\n') +
      '\n'
  );
  write(join(DEMO, '.gitignore'), 'node_modules\n');
  git(DEMO, 'init', '-q');
  git(DEMO, 'checkout', '-q', '-b', 'main');
  git(DEMO, 'config', 'user.email', 'e2e@example.com');
  git(DEMO, 'config', 'user.name', 'E2E');
  git(DEMO, 'config', 'core.autocrlf', 'false');
  git(DEMO, 'add', '-A');
  git(DEMO, 'commit', '-qm', 'Initial commit');
  // A remote with a commit the project doesn't have, for pull and push.
  const remote = join(WORK, 'remote.git');
  git(WORK, 'init', '-q', '--bare', remote);
  git(remote, 'symbolic-ref', 'HEAD', 'refs/heads/main');
  git(DEMO, 'remote', 'add', 'origin', remote);
  git(DEMO, 'push', '-q', '-u', 'origin', 'main');
  // Someone else's clone, for a commit the project doesn't have (the pull
  // step makes it: before, the app's commit, which pushes, would be refused).
  const other = join(WORK, 'other');
  git(WORK, 'clone', '-q', remote, other);
  git(other, 'config', 'user.email', 'other@example.com');
  git(other, 'config', 'user.name', 'Other');
}

// --- processes ------------------------------------------------------------

const children = [];
function startChild(cmd, argv, opts = {}) {
  const logFile = join(OUT, `${opts.name ?? 'child'}.log`);
  const child = spawn(cmd, argv, {
    env: { ...process.env, ...(opts.env ?? {}) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', (c) => appendFileSync(logFile, c));
  child.stderr.on('data', (c) => appendFileSync(logFile, c));
  children.push(child);
  return child;
}

async function waitHttp(url, timeout = 30_000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    try {
      const res = await fetch(url);
      if (res.status < 500) return true;
    } catch {}
    await sleep(250);
  }
  throw new Error(`${url} never answered`);
}

// --- the driver -----------------------------------------------------------

let d;
const X = PLATFORM === 'linux' && DRIVER === 'webdriver' && hasXdotool();
function hasXdotool() {
  try {
    execFileSync('xdotool', ['version'], { stdio: 'ignore' });
    return !!process.env.DISPLAY;
  } catch {
    return false;
  }
}
const MOD = PLATFORM === 'darwin' ? Keys.META : Keys.CTRL;

/** A leftover instance would take the new launch (single instance) and the session would hang. */
function killApp() {
  // By the launched binary's name: an installed build's may differ.
  const name = basename(APP) || 'rovik-ide';
  try {
    if (PLATFORM === 'win32') spawnSync('taskkill', ['/IM', name, '/F'], { stdio: 'ignore' });
    else spawnSync('pkill', ['-x', name.slice(0, 15)], { stdio: 'ignore' });
  } catch {}
}

async function startApp() {
  killApp();
  await sleep(500);
  if (DRIVER === 'bridge') {
    if (!(d instanceof BridgeDriver))
      d = new BridgeDriver({ shots: SHOTS, platform: PLATFORM, log });
    await d.start(APP, { logFile: join(OUT, 'app.log') });
  } else {
    d = d instanceof WebDriver ? d : new WebDriver({ shots: SHOTS, platform: PLATFORM });
    await d.start(APP);
  }
  await d.waitFor(() => d.exists('.app'), { timeout: 60_000, what: 'the app' });
  await installHooks();
}

const HOOK = `
if (!window.__e2e) {
  window.__e2e = { errors: [], alerts: [] };
  const push = (kind, msg) => window.__e2e.errors.push({ kind, msg: String(msg).slice(0, 1500) });
  window.addEventListener('error', (e) => push('error', (e.message || '') + ' ' + ((e.error && e.error.stack) || '')));
  window.addEventListener('unhandledrejection', (e) => push('rejection', (e.reason && (e.reason.stack || e.reason.message)) || e.reason));
  const ce = console.error.bind(console);
  console.error = (...a) => { push('console.error', a.map((x) => (x && x.stack) || (typeof x === 'object' ? JSON.stringify(x) : String(x))).join(' ')); ce(...a); };
  window.alert = (m) => { window.__e2e.alerts.push(String(m)); push('alert', m); };
}
return true;`;

async function installHooks() {
  await d.exec(HOOK);
}

async function takeErrors() {
  try {
    return await d.exec(
      'const e = (window.__e2e && window.__e2e.errors) || []; if (window.__e2e) window.__e2e.errors = []; return e;'
    );
  } catch {
    return [];
  }
}

// Input: real X events on Linux when xdotool is there, else the driver's.
async function screenPoint(el) {
  // The middle of its visible part, scrolled to only when none of it shows:
  // scrolling Monaco's line layer (taller than the window) moves its DOM
  // behind its back, and the click lands on another line.
  const point = () =>
    d.exec(
      `const r = arguments[0].getBoundingClientRect();
       const left = Math.max(r.left, 0), right = Math.min(r.right, innerWidth);
       const top = Math.max(r.top, 0), bottom = Math.min(r.bottom, innerHeight);
       return right - left >= 1 && bottom - top >= 1
         ? [screenX + (left + right) / 2, screenY + (top + bottom) / 2]
         : null;`,
      d.ref(el)
    );
  let at = await point();
  if (!at) {
    await d.exec(`arguments[0].scrollIntoView({ block: 'center', inline: 'center' })`, d.ref(el));
    await sleep(120);
    at = await point();
  }
  if (!at) throw new Error('nothing of the element shows');
  return at.map(Math.round);
}
const xdo = (...a) => execFileSync('xdotool', a.map(String));

async function click(target) {
  // Found again when it was re-rendered between finding and clicking.
  for (let attempt = 1; ; attempt++) {
    const el = typeof target === 'string' ? await d.find(target) : target;
    try {
      if (X) {
        const [x, y] = await screenPoint(el);
        xdo('mousemove', x, y);
        await sleep(60);
        xdo('click', 1);
        return;
      }
      return await d.click(el);
    } catch (e) {
      if (typeof target !== 'string' || attempt >= 3 || !/stale/i.test(String(e?.message))) throw e;
      log(`re-rendered before the click, finding ${target} again`);
      await sleep(200);
    }
  }
}

async function rightClick(target) {
  const el = typeof target === 'string' ? await d.find(target) : target;
  if (X) {
    const [x, y] = await screenPoint(el);
    xdo('mousemove', x, y);
    await sleep(60);
    xdo('click', 3);
    return;
  }
  return d.rightClick(el);
}

async function drag(from, to) {
  const a = typeof from === 'string' ? await d.find(from) : from;
  const b = typeof to === 'string' ? await d.find(to) : to;
  if (X) {
    const [x1, y1] = await screenPoint(a);
    const [x2, y2] = await screenPoint(b);
    xdo('mousemove', x1, y1);
    await sleep(100);
    xdo('mousedown', 1);
    for (let i = 1; i <= 10; i++) {
      xdo(
        'mousemove',
        Math.round(x1 + ((x2 - x1) * i) / 10),
        Math.round(y1 + ((y2 - y1) * i) / 10)
      );
      await sleep(40);
    }
    await sleep(200);
    xdo('mouseup', 1);
    return;
  }
  return d.drag(a, b);
}

async function modClick(target) {
  const el = typeof target === 'string' ? await d.find(target) : target;
  if (X) {
    const [x, y] = await screenPoint(el);
    xdo('mousemove', x, y);
    await sleep(80);
    // Held a moment first, as a person does: Monaco's Ctrl+click (go to
    // definition) missed a Ctrl pressed in the same instant as the click.
    xdo('keydown', 'Control_L');
    await sleep(300);
    xdo('click', 1);
    await sleep(100);
    xdo('keyup', 'Control_L');
    return;
  }
  return d.modClick(el, MOD);
}

/** Waits until the tree's rows stop changing (a watcher refresh landing). */
async function treeSettled() {
  let last = '';
  let stable = 0;
  const end = Date.now() + 10_000;
  while (Date.now() < end && stable < 3) {
    const now = JSON.stringify(
      await d.exec(
        'return [...document.querySelectorAll(".tree-row")].map((r) => r.title + "@" + Math.round(r.getBoundingClientRect().top))'
      )
    );
    stable = now === last ? stable + 1 : 0;
    last = now;
    await sleep(250);
  }
}

const clickText = async (text, tag = '*') => click(await d.byText(text, { tag }));
const keys = (...k) => d.keys(...k);
const typeKeys = (t) => d.typeKeys(t);

/** Marks the first match of a script's element for a CSS lookup. */
async function mark(script, name, ...a) {
  const tryMark = () =>
    d.exec(
      `document.querySelectorAll('[data-e2e="${name}"]').forEach((e) => e.removeAttribute('data-e2e'));
       const el = (function () { ${script} }).apply(null, arguments);
       if (!el) return false;
       el.setAttribute('data-e2e', '${name}');
       return true;`,
      ...a
    );
  // What was just opened (a menu, a dialog) renders a moment later.
  await d.waitFor(tryMark, { timeout: 5000, interval: 150, what: name }).catch(() => {
    throw new Error(`nothing to mark as ${name}`);
  });
  return `[data-e2e="${name}"]`;
}

/** The last visible button (or menu item) whose text is `text`. */
const buttonByText = (text, within = 'document') =>
  mark(
    `return [...${within === 'document' ? 'document' : `document.querySelector(${JSON.stringify(within)})`}.querySelectorAll('button, [role=menuitem], li.context-menu-item, a')].filter((b) => b.textContent.trim() === ${JSON.stringify(text)} && b.offsetParent).pop();`,
    'button'
  );

const tabs = () =>
  d.exec('return [...document.querySelectorAll(".tab")].map((t) => t.innerText.split("\\n")[0])');
const tabTitles = () => d.exec('return [...document.querySelectorAll(".tab")].map((t) => t.title)');
const activeTab = () =>
  d.exec('return (document.querySelector(".tab.active")?.innerText || "").split("\\n")[0]');
const rows = () => d.exec('return [...document.querySelectorAll(".tree-row")].map((r) => r.title)');
const row = (title) => `.tree-row[title=${JSON.stringify(title)}]`;
// Monaco draws spaces as non-breaking ones.
const panelText = (css) =>
  d.exec(
    `return (document.querySelector(${JSON.stringify(css)})?.innerText ?? '').replace(/\u00a0/g, ' ')`
  );

async function setInput(css, value) {
  await d.exec(
    `const el = document.querySelector(${JSON.stringify(css)});
     const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
     Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, arguments[0]);
     el.dispatchEvent(new Event('input', { bubbles: true }));
     el.dispatchEvent(new Event('change', { bubbles: true }));`,
    value
  );
}

async function waitFor(fn, what, timeout = 10_000) {
  return d.waitFor(fn, { timeout, what });
}

async function treeOpen(title) {
  if (!(await d.exists(row(title)))) throw new Error(`no tree row ${title}`);
  const expanded = await d.exec(
    `return !!document.querySelector(${JSON.stringify(row(title))} + ' .tree-chevron.open')`
  );
  if (!expanded) await click(row(title));
  await sleep(400);
}

async function sidebar(tab) {
  await click(await buttonByText(tab, '.sidebar-tabs'));
  await sleep(500);
}

async function closeModal() {
  const closer = await mark(
    `return [...document.querySelectorAll('.modal button')].find((b) => b.textContent.trim() === '×');`,
    'close'
  );
  await click(closer);
  await sleep(500);
}

async function contextMenu(target, item) {
  await rightClick(target);
  await waitFor(() => d.exists('.file-context-menu'), 'the context menu');
  await click(
    await mark(
      `return [...document.querySelectorAll('.file-context-menu [role=menuitem], .file-context-menu button, .file-context-menu li')].find((e) => e.textContent.trim().startsWith(${JSON.stringify(item)}));`,
      'menu-item'
    )
  );
  await sleep(400);
}

async function composer(text) {
  await click('.chat-input-contenteditable');
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (i > 0) await keys([Keys.SHIFT, Keys.ENTER]);
    await typeKeys(lines[i]);
  }
}

async function send(text) {
  await composer(text);
  await keys(Keys.ENTER);
}

const chatText = () => panelText('.chat-panel');
const lastRequest = () => {
  const file = join(OUT, 'fake-model.jsonl');
  if (!existsSync(file)) return null;
  const lines = readFileSync(file, 'utf8').trim().split('\n');
  return JSON.parse(lines[lines.length - 1]);
};

// --- steps ----------------------------------------------------------------

async function step(name, fn, { optional = false } = {}) {
  if (ONLY && !ONLY.test(name)) return;
  const started = Date.now();
  const entry = { name, ok: true };
  try {
    await fn();
    entry.shot = await d.screenshot(slug(name)).catch(() => null);
  } catch (e) {
    entry.ok = false;
    entry.optional = optional;
    entry.error = String(e?.message ?? e).slice(0, 1500);
    entry.shot = await d?.screenshot(`FAIL-${slug(name)}`).catch(() => null);
    entry.body = (await d?.bodyText().catch(() => '')).slice(0, 3000);
  }
  entry.errors = d ? await takeErrors() : [];
  entry.ms = Date.now() - started;
  report.steps.push(entry);
  log(
    `${entry.ok ? 'PASS' : optional ? 'WARN' : 'FAIL'} ${name} (${entry.ms} ms)${entry.ok ? '' : `: ${entry.error}`}`
  );
  for (const e of entry.errors) log(`   js ${e.kind}: ${e.msg.split('\n')[0]}`);
  writeReport();
}

const slug = (s) =>
  s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .slice(0, 50);
const assert = (cond, msg) => {
  if (!cond) throw new Error(msg);
};

function writeReport() {
  report.finishedAt = new Date().toISOString();
  writeFileSync(join(OUT, 'report.json'), JSON.stringify(report, null, 2));
  const lines = [
    `# E2E report: ${PLATFORM} ${process.arch} (${DRIVER})`,
    '',
    `App: \`${APP}\``,
    '',
    '| | Step | ms | Problem |',
    '|---|---|---|---|',
    ...report.steps.map(
      (s) =>
        `| ${s.ok ? '✅' : s.optional ? '⚠️' : '❌'} | ${s.name} | ${s.ms} | ${
          s.ok
            ? s.errors.length
              ? `JS: ${s.errors
                  .map((e) => e.msg.split('\n')[0])
                  .join('; ')
                  .slice(0, 300)}`
              : ''
            : s.error.split('\n')[0].slice(0, 300)
        } |`
    ),
    '',
    ...(report.notes.length ? ['## Notes', '', ...report.notes.map((n) => `- ${n}`), ''] : []),
  ];
  writeFileSync(join(OUT, 'report.md'), lines.join('\n'));
}

async function run() {
  log(`platform ${PLATFORM} ${process.arch}, driver ${DRIVER}, app ${APP}`);
  setupProject();

  startChild(process.execPath, [join(HERE, 'fake-model.mjs')], {
    name: 'fake-model',
    env: { FAKE_PORT: String(MODEL_PORT), FAKE_LOG: join(OUT, 'fake-model.jsonl') },
  });
  await waitHttp(`http://127.0.0.1:${MODEL_PORT}/v1/models`);

  if (DRIVER === 'webdriver') {
    const td = args['tauri-driver'] ?? 'tauri-driver';
    const tdArgs = ['--port', '4444'];
    if (args['native-driver']) tdArgs.push('--native-driver', args['native-driver']);
    startChild(td, tdArgs, { name: 'tauri-driver' });
    await waitHttp('http://127.0.0.1:4444/status');
  }

  await step('launch: the window opens and the boot splash goes', async () => {
    await startApp();
    await waitFor(
      async () => !(await d.exists('#boot-splash:not(.done)')),
      'the boot splash to go',
      20_000
    );
  });
  if (!d) return;

  await step('empty state: open and new project', async () => {
    await waitFor(() => d.exists('.files-empty-open'), 'Open project…');
    assert(await d.exists('.files-empty-new'), 'no New project…');
  });

  await step('virtual files: what the app says at start', async () => {
    await sleep(2500);
    const text = await d.exec(
      'return [...document.querySelectorAll(".notification")].map((n) => n.innerText.replace(/\\n+/g, " | ")).join(" || ")'
    );
    note(`start-up notifications: ${text || '(none)'}`);
    const notNow = await d
      .exec(
        `return [...document.querySelectorAll('.notification button')].some((b) => b.textContent.trim() === 'Not now')`
      )
      .catch(() => false);
    if (notNow) await click(await buttonByText('Not now'));
  });

  await step('project: open an existing folder from New project…', async () => {
    await click('.files-empty-new');
    await waitFor(() => d.exists('#new-project-root'), 'the New project dialog');
    await setInput('#new-project-root', PROJECTS);
    await click('#new-project-name');
    await typeKeys('demo');
    await waitFor(() => d.exists('.new-project-open-existing'), '"Open existing"');
    await click('.new-project-open-existing');
    await waitFor(async () => (await rows()).includes('README.md'), 'the tree', 15_000);
    await waitFor(
      async () => (await panelText('.project-switcher-name')).includes('demo'),
      'the project switcher to name demo'
    );
  });

  await step('tree: expand a folder and open a file', async () => {
    await treeOpen('src');
    await click(row('main.ts'));
    await waitFor(
      async () => (await panelText('.monaco-editor .view-lines')).includes('export function main'),
      'main.ts in the editor'
    );
  });

  await step('editor: type, see it unsaved, save to disk', async () => {
    await click('.monaco-editor .view-lines');
    await keys([MOD, Keys.END]);
    await typeKeys('export const typed = 1;');
    await keys(Keys.ENTER);
    await waitFor(async () => (await tabs()).includes('main.ts •'), 'the unsaved mark');
    await keys([MOD, 's']);
    await waitFor(
      () => readFileSync(join(DEMO, 'src', 'main.ts'), 'utf8').includes('export const typed = 1;'),
      'main.ts saved'
    );
    await waitFor(async () => (await tabs()).includes('main.ts'), 'the unsaved mark to go');
  });

  await step('editor: completion lists a project symbol', async () => {
    await typeKeys('gre');
    await waitFor(
      async () =>
        (
          await d.exec(
            'return [...document.querySelectorAll(".suggest-widget .monaco-list-row")].map((r) => r.innerText).join("|")'
          )
        ).includes('greet'),
      'greet in the completion list',
      15_000
    );
    await keys(Keys.ESC);
    for (let i = 0; i < 3; i++) await keys(Keys.BACKSPACE);
    await keys([MOD, 's']);
  });

  await step('editor: go to definition opens the other file', async () => {
    const token = await mark(
      `const lines = [...document.querySelectorAll('.monaco-editor .view-line')];
       const spans = lines.flatMap((l) => [...l.querySelectorAll('span > span')]).filter((s) => s.textContent.trim() === 'add');
       return spans[1] || spans[0];`,
      'token'
    );
    // The project's sources reach the TypeScript worker a moment after it starts.
    await d.waitFor(
      async () => {
        await modClick(token);
        await sleep(1500);
        return (await activeTab()) === 'math.ts';
      },
      { timeout: 30_000, interval: 500, what: 'math.ts to open' }
    );
  });

  await step('markdown: preview beside the source, tick a task, discard', async () => {
    await click(row('README.md'));
    await waitFor(async () => (await activeTab()) === 'README.md', 'README.md');
    await click('.md-view-option[title^="Source and preview"]');
    await waitFor(
      async () =>
        (
          await d.exec(
            'return [...document.querySelectorAll("h1")].map((h) => h.innerText).join("|")'
          )
        ).includes('Demo project'),
      'the rendered heading'
    );
    const box = await mark(
      'return [...document.querySelectorAll("input[type=checkbox]")].find((c) => !c.checked);',
      'task'
    );
    await click(box);
    await waitFor(
      async () => (await tabs()).includes('README.md •'),
      'the ticked task to change the source'
    );
    await click('.tab.active .tab-close');
    await waitFor(() => d.exists('.confirm-dialog'), 'the discard question');
    await click(await buttonByText('Discard', '.confirm-dialog'));
    await waitFor(
      async () => !(await tabs()).some((t) => t.startsWith('README.md')),
      'the tab to close'
    );
    assert(
      readFileSync(join(DEMO, 'README.md'), 'utf8').includes('- [ ] Open task'),
      'the file changed'
    );
  });

  await step(
    'editor: switching tabs keeps scroll, cursor, selection; a tab click focuses',
    async () => {
      await click(row('long.ts'));
      await waitFor(async () => (await activeTab()) === 'long.ts', 'long.ts');
      await click('.monaco-editor .view-lines');
      await keys([Keys.CTRL, 'g']);
      await sleep(400);
      await typeKeys('150');
      await keys(Keys.ENTER);
      await sleep(300);
      await keys([Keys.SHIFT, Keys.END]);
      await sleep(300);
      const place = () =>
        d.exec(`const nums = [...document.querySelectorAll('.monaco-editor .line-numbers')].map((e) => +e.textContent).filter((n) => n);
        return { first: Math.min(...nums), line: +(document.querySelector('.monaco-editor .active-line-number')?.textContent || 0),
          selected: document.querySelectorAll('.monaco-editor .selected-text').length,
          focused: !!document.activeElement?.closest('.monaco-editor') };`);
      const before = await place();
      assert(before.line === 150 && before.selected > 0, `not set up: ${JSON.stringify(before)}`);
      await click(
        await mark(
          `return [...document.querySelectorAll('.tab')].find((t) => t.innerText.startsWith('main.ts'));`,
          'tab'
        )
      );
      await waitFor(async () => (await activeTab()) === 'main.ts', 'main.ts');
      await click(
        await mark(
          `return [...document.querySelectorAll('.tab')].find((t) => t.innerText.startsWith('long.ts'));`,
          'tab'
        )
      );
      await waitFor(async () => (await activeTab()) === 'long.ts', 'long.ts again');
      await sleep(500);
      const after = await place();
      assert(
        after.first === before.first && after.line === 150 && after.selected > 0,
        `before ${JSON.stringify(before)}, after ${JSON.stringify(after)}`
      );
      assert(after.focused, 'the editor didn’t take the keyboard');
      await click(
        await mark(
          `return [...document.querySelectorAll('.tab')].find((t) => t.innerText.startsWith('long.ts'));`,
          'tab'
        )
      );
      await click('.tab.active .tab-close');
    }
  );

  await step('tree: new file from the context menu', async () => {
    await contextMenu(row('docs'), 'New file');
    await typeKeys('guide.md');
    await keys(Keys.ENTER);
    await waitFor(() => existsSync(join(DEMO, 'docs', 'guide.md')), 'docs/guide.md on disk');
    await waitFor(async () => (await activeTab()) === 'guide.md', 'its tab');
  });

  await step('tree: rename an open file; its tab follows', async () => {
    await contextMenu(row('guide.md'), 'Rename');
    await keys([MOD, 'a']);
    await typeKeys('handbook.md');
    await keys(Keys.ENTER);
    await waitFor(() => existsSync(join(DEMO, 'docs', 'handbook.md')), 'handbook.md on disk');
    await waitFor(async () => (await activeTab()) === 'handbook.md', 'the tab renamed');
  });

  await step('tree: rename a folder with an open file; its tab follows', async () => {
    await contextMenu(row('docs'), 'Rename');
    await keys([MOD, 'a']);
    await typeKeys('documentation');
    await keys(Keys.ENTER);
    await waitFor(
      () => existsSync(join(DEMO, 'documentation', 'handbook.md')),
      'the folder renamed'
    );
    await sleep(800);
    const titles = await tabTitles();
    const handbook = titles.find((t) => t.includes('handbook.md')) ?? '';
    assert(!handbook.includes('DELETED') && !handbook.includes('deleted'), `tab: ${handbook}`);
    assert(
      handbook.includes(join('documentation', 'handbook.md')),
      `the tab's path isn't the new one: ${handbook}`
    );
  });

  await step('tree: delete a file to the trash', async () => {
    await treeOpen('documentation');
    await contextMenu(row('notes.md'), 'Delete');
    await waitFor(() => d.exists('.confirm-dialog'), 'the delete question');
    await click(await buttonByText('Move to Trash', '.confirm-dialog'));
    await waitFor(() => !existsSync(join(DEMO, 'documentation', 'notes.md')), 'notes.md gone');
    await waitFor(async () => !(await rows()).includes('notes.md'), 'its row gone');
  });

  await step('tree: drag a file into a folder', async () => {
    await treeSettled();
    await drag(row('notes.txt'), row('src'));
    await waitFor(
      () => existsSync(join(DEMO, 'src', 'notes.txt')),
      'notes.txt moved into src',
      10_000
    );
  });

  await step('search: find text, open the match', async () => {
    await click('input[placeholder="Search in files"]');
    await typeKeys('find me');
    await waitFor(
      async () => (await panelText('.sidebar')).includes('find me here'),
      'a result',
      15_000
    );
    const hit = await mark(
      `return [...document.querySelectorAll('.search-line')].find((e) => e.textContent.includes('find me here'));`,
      'hit'
    );
    await click(hit);
    await waitFor(async () => (await activeTab()) === 'notes.txt', 'notes.txt opened');
    // Kept while another sidebar tab is shown.
    await sidebar('Git');
    await sidebar('Files');
    await waitFor(
      async () =>
        (await d.exec(
          'return document.querySelector(\'input[placeholder="Search in files"]\')?.value'
        )) === 'find me' && (await panelText('.sidebar')).includes('find me here'),
      'the search kept after Git and back'
    );
    await click('input[placeholder="Search in files"]');
    await keys([MOD, 'a'], Keys.BACKSPACE, Keys.ESC);
  });

  await step('terminal: run a command', async () => {
    await click('.terminal-toggle-button');
    await waitFor(() => d.exists('.xterm-rows'), 'a terminal', 15_000);
    await sleep(2500);
    await click('.xterm-screen');
    await typeKeys('echo e2e-hello');
    await keys(Keys.ENTER);
    await waitFor(
      async () => ((await panelText('.xterm-rows')).match(/e2e-hello/g) ?? []).length >= 2,
      'the command’s output',
      20_000
    );
    await click('.terminal-toggle-button');
  });

  await step('git: changes listed, commit', async () => {
    await sidebar('Git');
    await waitFor(() => d.exists('.git-file-row'), 'changed files');
    await click('textarea[placeholder="Commit message"], input[placeholder="Commit message"]');
    await typeKeys('E2E changes');
    // A message being written is kept while another sidebar tab is shown.
    await sidebar('Files');
    await sidebar('Git');
    const kept = await d.exec(
      'return document.querySelector(\'textarea[placeholder="Commit message"], input[placeholder="Commit message"]\')?.value'
    );
    assert(
      kept === 'E2E changes',
      `the commit message after Files and back: ${JSON.stringify(kept)}`
    );
    await click(await buttonByText('Commit'));
    await waitFor(() => git(DEMO, 'log', '-1', '--format=%s') === 'E2E changes', 'the commit');
    // The commit is pushed too.
    await waitFor(
      () => git(join(WORK, 'remote.git'), 'log', '-1', '--format=%s', 'main') === 'E2E changes',
      'the push',
      30_000
    );
  });

  /** Git's "More actions" menu (its button is disabled while the panel is busy). */
  async function moreActions() {
    await waitFor(
      () => d.exec(`return document.querySelector('[title="More actions"]')?.disabled === false`),
      'More actions to be enabled',
      30_000
    );
    await click('[title="More actions"]');
    await waitFor(() => d.exists('.git-more-dropdown'), 'the More actions menu');
  }

  await step('git: new branch, stash and pop, back to main', async () => {
    await moreActions();
    await click(await buttonByText('New branch…'));
    await click('input[placeholder="Branch name"]');
    await typeKeys('feature/e2e');
    await keys(Keys.ENTER);
    await waitFor(() => git(DEMO, 'branch', '--show-current') === 'feature/e2e', 'the branch');
    writeFileSync(join(DEMO, 'src', 'notes.txt'), 'line one\nline two\nfind me here\nstash me\n');
    // "Stash changes…" is disabled until the panel lists a change.
    await waitFor(
      async () => (await panelText('.git-panel')).includes('notes.txt'),
      'the change listed',
      20_000
    );
    await moreActions();
    await click(await buttonByText('Stash changes…'));
    await click('input[placeholder="Message (optional)"]');
    await typeKeys('wip');
    await keys(Keys.ENTER);
    await waitFor(() => git(DEMO, 'stash', 'list').includes('wip'), 'the stash');
    await moreActions();
    await click(
      await mark(
        `return [...document.querySelectorAll('.git-more-dropdown li.context-menu-item')].find((b) => b.textContent.trim().startsWith('Stashed changes'));`,
        'stashes'
      )
    );
    await click('[title="Restore this stash and remove it from the list"]');
    await waitFor(() => git(DEMO, 'stash', 'list') === '', 'the stash popped');
    await keys(Keys.ESC);
    git(DEMO, 'checkout', '-q', '--', '.');
    await click('.git-branch-button');
    await click(
      await mark(
        `return [...document.querySelectorAll('.git-branch-dropdown li.context-menu-item')].find((e) => e.textContent.trim() === 'main');`,
        'main'
      )
    );
    await waitFor(() => git(DEMO, 'branch', '--show-current') === 'main', 'back on main');
  });

  await step('git: pull a conflicting change, resolve it, push', async () => {
    // On main, whatever the steps before left.
    if (git(DEMO, 'branch', '--show-current') !== 'main') git(DEMO, 'checkout', '-qf', 'main');
    // A file both sides then change on the same line.
    write(join(DEMO, 'conflict.txt'), 'alpha\nbeta\ngamma\n');
    git(DEMO, 'add', 'conflict.txt');
    git(DEMO, 'commit', '-qm', 'Add conflict.txt');
    git(DEMO, 'push', '-q');
    const other = join(WORK, 'other');
    git(other, 'pull', '-q');
    write(join(other, 'conflict.txt'), 'alpha\nbeta from remote\ngamma\n');
    git(other, 'commit', '-qam', 'Remote edit');
    git(other, 'push', '-q');
    writeFileSync(join(DEMO, 'conflict.txt'), 'alpha\nbeta LOCAL\ngamma\n');
    git(DEMO, 'commit', '-qam', 'Local edit');
    await sleep(1000);
    await click('[title="Pull"]');
    await waitFor(
      async () =>
        (await d.exists('.cr-accept')) || (await d.bodyText()).includes('Merge or rebase?'),
      'the pull to ask or conflict',
      30_000
    );
    if (!(await d.exists('.cr-accept'))) {
      await click(
        await mark(
          `return [...document.querySelectorAll('button, [role=button]')].find((b) => b.textContent.trim().startsWith('Merge') && b.offsetParent);`,
          'merge'
        )
      );
    }
    await waitFor(() => d.exists('.cr-accept'), 'the conflict resolver', 30_000);
    await click('.cr-ours .cr-accept');
    await click(
      await mark(
        `return [...document.querySelectorAll('.cr-accept')].find((b) => !b.closest('.cr-ours'));`,
        'theirs'
      )
    );
    await click(await buttonByText('Mark resolved'));
    await sleep(800);
    await click(
      await mark(
        `return [...document.querySelectorAll('button')].filter((b) => b.textContent.trim() === 'Commit merge' && b.offsetParent).pop();`,
        'commit-merge'
      )
    );
    await waitFor(
      () => git(DEMO, 'log', '-1', '--format=%p').split(' ').length === 2,
      'the merge commit',
      20_000
    );
    const merged = readFileSync(join(DEMO, 'conflict.txt'), 'utf8');
    assert(merged.includes('LOCAL') && merged.includes('from remote'), `merged: ${merged}`);
    await click(
      await mark(
        `return [...document.querySelectorAll('button')].find((b) => b.textContent.trim().endsWith('Push') && b.offsetParent);`,
        'push'
      )
    );
    await waitFor(
      () =>
        git(join(WORK, 'remote.git'), 'log', '-1', '--format=%H') ===
        git(DEMO, 'rev-parse', 'HEAD'),
      'the push',
      30_000
    );
  });

  await step('settings: every page opens', async () => {
    const pages = [
      'Editor',
      'Model providers',
      'Project checks',
      'Permissions',
      'Subagents',
      'Builds',
      'Virtual files',
      'Cloud hosts',
    ];
    for (const page of pages) {
      await click('.sidebar-settings-button');
      await waitFor(() => d.exists('.settings-menu-item'), 'the settings menu');
      await click(
        await mark(
          `return [...document.querySelectorAll('.settings-menu-item')].find((b) => b.innerText.trim().startsWith(${JSON.stringify(page)}));`,
          'page'
        )
      );
      await sleep(900);
      const text = await panelText('.modal');
      assert(text.length > 40, `${page}: empty`);
      if (page === 'Virtual files' || page === 'Cloud hosts')
        note(`${page}: ${text.replace(/\n+/g, ' | ').slice(0, 400)}`);
      await closeModal();
    }
  });

  await step('settings: add a model provider', async () => {
    await click('.sidebar-settings-button');
    await click(
      await mark(
        `return [...document.querySelectorAll('.settings-menu-item')].find((b) => b.innerText.trim().startsWith('Model providers'));`,
        'page'
      )
    );
    await click(await buttonByText('+ Add provider'));
    await click('input[placeholder="https://api.openai.com/v1"]');
    await typeKeys(`http://127.0.0.1:${MODEL_PORT}/v1`);
    await click('input[type=password]');
    await typeKeys('sk-e2e-key');
    await click('input[placeholder="e.g. OpenRouter"]');
    await keys([MOD, 'a']);
    await typeKeys('Fake');
    await click(
      await mark(
        `return [...document.querySelectorAll('.modal button')].find((b) => /^Test/.test(b.textContent.trim()));`,
        'test'
      )
    );
    await waitFor(
      async () => (await panelText('.modal')).includes('Connected, 2 models'),
      'the models',
      15_000
    );
    await click(await buttonByText('Save', '.modal'));
    await waitFor(
      async () => (await panelText('.modal')).includes('2 of 2 models enabled'),
      'the provider listed'
    );
    await closeModal();
  });

  await step('chat: a reply streams in', async () => {
    await sidebar('AI Chat');
    await waitFor(() => d.exists('.chat-input-contenteditable'), 'the chat');
    await send('Hello there, what is this project?');
    await waitFor(
      async () => (await chatText()).includes('Echo of your message'),
      'the reply',
      30_000
    );
    await waitFor(() => d.exists('.chat-send-button:not(.stop)'), 'the run to end', 30_000);
  });

  await step('chat: the agent writes a file, it lands, Undo takes it back', async () => {
    await send(
      'Please create the file.\n@tool write {"path":"hello.txt","content":"Hi from agent\\n"}\n@text Wrote hello.txt.'
    );
    await waitFor(() => existsSync(join(DEMO, 'hello.txt')), 'hello.txt landed', 60_000);
    await waitFor(async () => (await chatText()).includes('Landed'), 'the Landed bar', 30_000);
    const gap = () =>
      d.exec(
        'const el = document.querySelector(".chat-messages"); return el.scrollHeight - el.clientHeight - el.scrollTop'
      );
    await d
      .waitFor(async () => (await gap()) <= 4, { timeout: 3000, what: 'the list at its newest' })
      .catch(async () => {
        throw new Error(`the list isn't at its newest message (${await gap()}px above the bottom)`);
      });
    await click(await buttonByText('Undo'));
    await waitFor(() => !existsSync(join(DEMO, 'hello.txt')), 'the landing undone', 30_000);
  });

  await step('chat: Stop ends a streaming reply', async () => {
    await send(
      'Write a long story\n@slow 20\n@text Once upon a time there was a very long story that keeps going and going for quite a while so we can stop it.'
    );
    await waitFor(() => d.exists('.chat-send-button.stop'), 'the Stop button', 15_000);
    await sleep(3000);
    await click('.chat-send-button.stop');
    await waitFor(async () => (await chatText()).includes('Stopped'), 'the reply stopped', 15_000);
  });

  await step('chat: a rejected API key is explained', async () => {
    await send('Auth error please\n@error 401 Invalid API key');
    await waitFor(
      async () => (await chatText()).includes('API key was rejected'),
      'the explanation',
      30_000
    );
    // The failed message comes back to the composer: clear it.
    await click('.chat-input-contenteditable');
    await keys([MOD, 'a'], Keys.BACKSPACE);
  });

  await step('chat: Plan mode asks a question, then saves a plan', async () => {
    await click(await buttonByText('Plan', '.mode-selector'));
    try {
      await planScenario();
    } finally {
      await click(await buttonByText('Agent', '.mode-selector')).catch(() => {});
    }
  });

  async function planScenario() {
    await send(
      'Plan a subtract function\n@tool ask_user {"questions":[{"question":"Two numbers or more?","options":[{"label":"Two numbers","recommended":true},{"label":"Any count"}]},{"question":"Which name?","options":[{"label":"subtract"},{"label":"minus"}]}]}'
    );
    await waitFor(() => d.exists('.question-option'), 'the question', 30_000);
    await click('.question-option');
    await waitFor(
      async () => (await panelText('.question-panel')).includes('Question 2 of 2'),
      'the second question'
    );
    // An answer given is kept while another sidebar tab is shown.
    await sidebar('Files');
    await sidebar('AI Chat');
    await waitFor(
      async () => (await panelText('.question-panel')).includes('Question 2 of 2'),
      'the panel back at the second question'
    );
    await click('.question-option');
    await waitFor(
      async () => (await panelText('.question-panel')).includes('Two numbers'),
      'the review'
    );
    await click(await buttonByText('Send answers'));
    await waitFor(
      async () => (await chatText()).includes('Two numbers'),
      'the answer sent',
      30_000
    );
    await waitFor(() => d.exists('.chat-send-button:not(.stop)'), 'the run to end', 30_000);
    const plan = JSON.stringify({
      plan: '# Add subtract\n\nAdd subtract to math.\n\n## Steps\n\n1. Add subtract to the math module\n   Files: src/lib/math.ts\n2. Use it in main\n   Files: src/main.ts\n\n## Decisions\n\n- Two numbers.\n\n## Validation\n\n- npm test passes.\n\n## Risks\n\n- None.',
    });
    await send(`Now the plan\n@tool submit_plan ${plan}\n@text The plan is saved.`);
    await waitFor(
      async () => (await chatText()).includes('Implement this plan'),
      'the saved plan',
      30_000
    );
    const plans = join(DEMO, '.rovik', 'plans');
    assert(existsSync(plans) && readdirSync(plans).length > 0, 'no plan file');
  }

  await step(
    'chat: an edit waiting for approval, reviewed and accepted in the editor',
    async () => {
      await click('.sidebar-settings-button');
      await click(
        await mark(
          `return [...document.querySelectorAll('.settings-menu-item')].find((b) => b.innerText.trim().startsWith('Permissions'));`,
          'page'
        )
      );
      await click(
        await mark(
          `const h = [...document.querySelectorAll('.modal *')].find((e) => /^file edits$/i.test(e.textContent.trim()));
         let p = h; while (p && !p.querySelector('button')) p = p.parentElement;
         return [...p.querySelectorAll('button')].find((b) => b.textContent.trim() === 'By you');`,
          'by-you'
        )
      );
      await closeModal();
      await send(
        'Edit main please\n@tool write {"path":"src/main.ts","old_text":"main();","content":"main(); // reviewed"}\n@text Edited.'
      );
      await waitFor(
        async () => (await chatText()).includes('Waiting for your review'),
        'the approval',
        30_000
      );
      await click(await buttonByText('Review in editor'));
      await waitFor(
        () => d.exists('.monaco-editor .review-button.accept'),
        'the review in the editor',
        15_000
      );
      const accept = await mark(
        `return [...document.querySelectorAll('.monaco-editor .review-button.accept')].find((b) => b.textContent.trim() === 'Accept' && b.offsetParent);`,
        'accept'
      );
      const onTop = await d.exec(
        `const b = document.querySelector('${accept}'); const r = b.getBoundingClientRect();
       const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
       return top === b || b.contains(top) ? '' : top.className;`
      );
      assert(!onTop, `the Accept button is covered by ${onTop}`);
      await click(accept);
      await waitFor(
        () => readFileSync(join(DEMO, 'src', 'main.ts'), 'utf8').includes('main(); // reviewed'),
        'the accepted edit landed',
        60_000
      );
    }
  );

  await step('chat: the reading place is kept while another sidebar tab is shown', async () => {
    const where = () =>
      d.exec(`const el = document.querySelector('.chat-messages'); const top = el.getBoundingClientRect().top;
        const first = [...document.querySelectorAll('.chat-item')].find((i) => i.getBoundingClientRect().bottom > top + 1);
        return { id: first?.dataset.id, scrollTop: el.scrollTop, max: el.scrollHeight - el.clientHeight };`);
    const start = await where();
    assert(start.max > 400, `not enough to scroll (${start.max}px)`);
    // Up, as the wheel does it (the list then stops following).
    await d.exec(`const el = document.querySelector('.chat-messages');
      el.dispatchEvent(new WheelEvent('wheel', { deltaY: -400, bubbles: true }));
      el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight - 400);`);
    await sleep(800);
    const before = await where();
    await sidebar('Files');
    await sidebar('AI Chat');
    await sleep(1000);
    const after = await where();
    assert(
      after.id === before.id,
      `was at ${JSON.stringify(before)}, back at ${JSON.stringify(after)}`
    );
  });

  await step('chat history: a new chat, search, delete', async () => {
    await click('.chat-history-button');
    await click(await buttonByText('New chat'));
    await send('Second chat about bananas');
    await waitFor(
      async () => (await chatText()).includes('Echo of your message'),
      'the reply',
      30_000
    );
    await click('.chat-history-button');
    await click('input[placeholder="Search chats"]');
    await typeKeys('bananas');
    await waitFor(
      async () => (await panelText('.chat-history')).includes('bananas'),
      'the search hit',
      10_000
    );
    const before = (await d.findAll('.chat-history-delete')).length;
    await click('.chat-history-delete');
    await waitFor(() => d.exists('.confirm-dialog'), 'the delete question');
    await click(
      await mark(
        `return document.querySelector('.confirm-dialog button.danger-filled') || [...document.querySelectorAll('.confirm-dialog button')].pop();`,
        'delete'
      )
    );
    await waitFor(
      async () => (await d.findAll('.chat-history-delete')).length < before,
      'the chat deleted',
      10_000
    );
  });

  await step('restart: the project and its tabs come back', async () => {
    const open = (await tabs()).filter((t) => !t.includes('(')).map((t) => t.replace(' •', ''));
    await d.quit();
    await sleep(2000);
    await startApp();
    await waitFor(async () => (await rows()).includes('README.md'), 'the project restored', 30_000);
    const now = await tabs();
    for (const name of open)
      assert(now.includes(name), `tab ${name} not restored (${now.join(', ')})`);
  });
}

async function main() {
  try {
    await run();
  } catch (e) {
    log(`runner error: ${e.stack ?? e}`);
    report.runnerError = String(e.stack ?? e);
  } finally {
    writeReport();
    try {
      await d?.close();
    } catch {}
    for (const c of children) c.kill();
  }
  const failed = report.steps.filter((s) => !s.ok && !s.optional);
  log(`${report.steps.length - failed.length}/${report.steps.length} steps passed`);
  console.log(readFileSync(join(OUT, 'report.md'), 'utf8'));
  process.exit(failed.length || report.runnerError ? 1 : 0);
}

void main();
