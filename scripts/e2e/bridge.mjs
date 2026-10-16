// The end-to-end runner's driver where the webview has no WebDriver
// (macOS: WKWebView): the app is built with bridge-page.js injected
// (inject-bridge.mjs), launched directly, and driven over HTTP long
// polling. Same methods as WebDriver in webdriver.mjs, so the scenarios
// don't care which one runs them. Screenshots come from the OS
// (`screencapture` on macOS, .NET on Windows, ImageMagick's `import`
// elsewhere).
import { createServer } from 'node:http';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, openSync } from 'node:fs';
import { join } from 'node:path';
import { Keys, sleep, xpathLiteral } from './webdriver.mjs';

export class BridgeDriver {
  constructor({ port = 17999, shots = '.', platform = process.platform, log } = {}) {
    this.port = port;
    this.shots = shots;
    this.platform = platform;
    this.shotN = 0;
    this.log = log;
    this.queue = [];
    this.waiting = null; // a page's /next request waiting for a command
    this.pending = new Map();
    this.seq = 0;
    mkdirSync(shots, { recursive: true });
  }

  get MOD() {
    return this.platform === 'darwin' ? Keys.META : Keys.CTRL;
  }

  listen() {
    if (this.server) return Promise.resolve();
    this.server = createServer((req, res) => {
      res.setHeader('Access-Control-Allow-Origin', '*');
      let body = '';
      req.on('data', (c) => (body += c));
      req.on('end', () => {
        if (req.url === '/next') {
          this.connected = true;
          if (this.waiting) this.reply(this.waiting, null);
          this.waiting = res;
          // Answer an idle poll now and then, so a dead connection is noticed.
          res.idle = setTimeout(() => {
            if (this.waiting === res) {
              this.waiting = null;
              this.reply(res, null);
            }
          }, 20_000);
          this.flush();
        } else if (req.url === '/result') {
          try {
            const { id, value, error } = JSON.parse(body);
            const p = this.pending.get(id);
            if (p) {
              this.pending.delete(id);
              clearTimeout(p.timer);
              if (error) p.reject(new Error(error));
              else p.resolve(value);
            }
          } catch {}
          res.end('ok');
        } else {
          res.statusCode = 404;
          res.end();
        }
      });
    });
    return new Promise((resolve) => this.server.listen(this.port, '127.0.0.1', resolve));
  }

  reply(res, command) {
    clearTimeout(res.idle);
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify(command));
  }

  flush() {
    if (!this.waiting || this.queue.length === 0) return;
    const res = this.waiting;
    this.waiting = null;
    this.reply(res, this.queue.shift());
  }

  call(op, args = {}, timeout = 30_000) {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`bridge: ${op} timed out`));
      }, timeout);
      this.pending.set(id, { resolve, reject, timer });
      this.queue.push({ id, op, args });
      this.flush();
    });
  }

  /** Launches the app and waits for its page to connect. */
  async start(application, { env = {}, logFile } = {}) {
    await this.listen();
    const out = logFile ? openSync(logFile, 'a') : 'ignore';
    this.child = spawn(application, [], {
      env: { ...process.env, ...env },
      stdio: ['ignore', out, out],
    });
    this.child.on('exit', (code, signal) => {
      this.exited = { code, signal };
      this.log?.(`app exited: code ${code} signal ${signal}`);
    });
    const end = Date.now() + 120_000;
    while (Date.now() < end) {
      if (this.exited) throw new Error(`the app exited before its page connected`);
      try {
        if ((await this.call('ping', {}, 3000)) === 'pong') return;
      } catch {}
      await sleep(500);
    }
    throw new Error('the page never connected to the bridge');
  }

  async quit() {
    if (!this.child) return;
    const child = this.child;
    this.child = null;
    if (this.exited) return;
    child.kill('SIGTERM');
    for (let i = 0; i < 50 && !this.exited; i++) await sleep(100);
    if (!this.exited) child.kill('SIGKILL');
    this.exited = null;
    this.connected = false;
    // Commands queued for the old page are dropped.
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.reject(new Error('app quit'));
    }
    this.pending.clear();
    this.queue = [];
  }

  async close() {
    await this.quit();
    this.server?.close();
  }

  // --- the WebDriver-like surface --------------------------------------

  /** An element as a script argument (already one here). */
  ref(el) {
    return el;
  }

  async findAll(css) {
    return (await this.call('findAll', { css })) ?? [];
  }

  async find(css, { timeout = 5000 } = {}) {
    const end = Date.now() + timeout;
    for (;;) {
      const el = await this.call('find', { css }).catch(() => null);
      if (el) return el;
      if (Date.now() > end) throw new Error(`not found: ${css}`);
      await sleep(150);
    }
  }

  async findXAll(xpath) {
    return (await this.call('findX', { xpath, all: true })) ?? [];
  }

  async findX(xpath, { timeout = 5000 } = {}) {
    const end = Date.now() + timeout;
    for (;;) {
      const els = await this.call('findX', { xpath, all: false }).catch(() => []);
      if (els && els.length) return els[0];
      if (Date.now() > end) throw new Error(`not found: ${xpath}`);
      await sleep(150);
    }
  }

  async byText(text, { tag = '*', exact = true, timeout = 5000 } = {}) {
    const lit = xpathLiteral(text);
    const xp = exact
      ? `//${tag}[normalize-space(.)=${lit}]`
      : `//${tag}[contains(normalize-space(.), ${lit})]`;
    return this.findX(xp, { timeout });
  }

  async exists(css) {
    return (await this.findAll(css).catch(() => [])).length > 0;
  }

  /**
   * A click where the element is. One that landed on something else
   * (covering it, or drawn where it was scrolled away from) is an error, as
   * WebDriver's "element click intercepted": not a click that did nothing.
   * Only for a click, as there: a pointer action (a right or double click, a
   * click with a key held) goes to whatever is there, Monaco's own layers
   * over the text included.
   */
  async pointAt(el, opts, { strict = false } = {}) {
    const covered = await this.call('click', { el, opts });
    if (covered && strict) throw new Error(`element click intercepted: it landed on ${covered}`);
    return null;
  }

  async click(el) {
    return this.pointAt(el, undefined, { strict: true });
  }

  /** A click held down a moment before its release, as a person's is. */
  async holdClick(el, ms = 150) {
    return this.pointAt(el, { hold: ms }, { strict: true });
  }

  async clickCss(css, opts) {
    return this.click(await this.find(css, opts));
  }

  async clickText(text, opts) {
    return this.click(await this.byText(text, opts));
  }

  async type(el, text) {
    return this.call('type', { el, text: [...text] });
  }

  async text(el) {
    return this.call('text', { el });
  }

  async rect(el) {
    return this.call('rect', { el });
  }

  async attr(el, name) {
    return this.exec('return arguments[0].getAttribute(arguments[1])', el, name);
  }

  async prop(el, name) {
    return this.exec('return arguments[0][arguments[1]]', el, name);
  }

  async exec(script, ...args) {
    return this.call('exec', { script, args });
  }

  async keys(...chords) {
    return this.call('keys', {
      chords: chords.map((c) => (Array.isArray(c) ? c : [c])),
    });
  }

  async typeKeys(text) {
    return this.call('keys', { chords: [...text].map((c) => [c]) });
  }

  async rightClick(el) {
    return this.pointAt(el, { button: 2 });
  }

  async doubleClick(el) {
    return this.pointAt(el, { count: 2 });
  }

  async modClick(el, mod) {
    const mods =
      mod === Keys.META
        ? { metaKey: true }
        : mod === Keys.SHIFT
          ? { shiftKey: true }
          : { ctrlKey: true };
    return this.pointAt(el, { mods });
  }

  async drag(from, to) {
    return this.call('drag', { from, to }, 60_000);
  }

  async screenshot(name) {
    const file = join(this.shots, `${String(++this.shotN).padStart(3, '0')}-${name}.png`);
    try {
      if (this.platform === 'darwin') {
        // The whole screen: WKWebView gives the page no window frame (its
        // outerWidth and outerHeight are 0), so there's no rectangle to cut.
        execFileSync('screencapture', ['-x', file], { timeout: 15_000 });
      } else if (this.platform === 'win32') {
        // The whole primary screen, through .NET.
        const script = [
          'Add-Type -AssemblyName System.Windows.Forms, System.Drawing',
          '$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds',
          '$img = New-Object System.Drawing.Bitmap $b.Width, $b.Height',
          '$g = [System.Drawing.Graphics]::FromImage($img)',
          '$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)',
          `$img.Save('${file.replace(/'/g, "''")}', [System.Drawing.Imaging.ImageFormat]::Png)`,
        ].join('; ');
        execFileSync('powershell', ['-NoProfile', '-Command', script], { timeout: 30_000 });
      } else {
        execFileSync('import', ['-window', 'root', file], { timeout: 15_000 });
      }
    } catch (e) {
      this.log?.(`screenshot ${name} failed: ${e.message}`);
    }
    return file;
  }

  async waitFor(fn, { timeout = 10000, interval = 200, what = 'condition' } = {}) {
    const end = Date.now() + timeout;
    let last;
    for (;;) {
      try {
        last = await fn();
        if (last) return last;
      } catch (e) {
        last = e;
      }
      if (Date.now() > end)
        throw new Error(`timed out waiting for ${what}: ${last?.message ?? last}`);
      await sleep(interval);
    }
  }

  async waitGone(css, { timeout = 10000 } = {}) {
    return this.waitFor(async () => !(await this.exists(css)), { timeout, what: `${css} to go` });
  }

  async bodyText() {
    return this.exec('return document.body.innerText');
  }
}
