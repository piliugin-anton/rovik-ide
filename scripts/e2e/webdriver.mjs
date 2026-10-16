// A minimal W3C WebDriver client for the end-to-end runner (run.mjs):
// tauri-driver on :4444, in front of WebKitWebDriver (Linux) or
// msedgedriver (Windows). bridge.mjs has the same methods for macOS.
import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

const ELEMENT = 'element-6066-11e4-a52e-4f735466cecf';
export const Keys = {
  CTRL: '',
  SHIFT: '',
  ALT: '',
  META: '',
  ENTER: '',
  RETURN: '',
  ESC: '',
  TAB: '',
  BACKSPACE: '',
  DELETE: '',
  UP: '',
  DOWN: '',
  LEFT: '',
  RIGHT: '',
  HOME: '',
  END: '',
  F2: '',
  PAGEDOWN: '',
};
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export class WebDriver {
  constructor({ base = 'http://127.0.0.1:4444', shots = '.', platform = process.platform } = {}) {
    this.base = base;
    this.shots = shots;
    this.platform = platform;
    this.shotN = 0;
    mkdirSync(shots, { recursive: true });
  }

  /** The platform's shortcut modifier (Cmd on macOS, Ctrl elsewhere). */
  get MOD() {
    return this.platform === 'darwin' ? Keys.META : Keys.CTRL;
  }

  async req(method, path, body) {
    const url = `${this.base}${path}`;
    const res = await fetch(url, {
      method,
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    const text = await res.text();
    let json;
    try {
      json = JSON.parse(text);
    } catch {
      json = { value: text };
    }
    if (!res.ok || (json.value && json.value.error)) {
      const err = new Error(
        `${method} ${path}: ${res.status} ${json.value?.error ?? ''} ${json.value?.message ?? text}`.slice(
          0,
          600
        )
      );
      err.wd = json.value;
      throw err;
    }
    return json.value;
  }

  s(path) {
    return `/session/${this.sid}${path}`;
  }

  /** An element as a script argument. */
  ref(el) {
    return { [ELEMENT]: el };
  }

  async start(application, extra = {}) {
    const caps = {
      capabilities: {
        alwaysMatch: { 'tauri:options': { application, ...extra }, browserName: 'wry' },
      },
    };
    const v = await this.req('POST', '/session', caps);
    this.sid = v.sessionId;
    return v;
  }

  async quit() {
    if (!this.sid) return;
    try {
      await this.req('DELETE', `/session/${this.sid}`);
    } catch {}
    this.sid = null;
  }

  async close() {
    await this.quit();
  }

  async findAll(css, from) {
    const path = from ? this.s(`/element/${from}/elements`) : this.s('/elements');
    const v = await this.req('POST', path, { using: 'css selector', value: css });
    return v.map((e) => e[ELEMENT]);
  }

  async find(css, { timeout = 5000, from } = {}) {
    const end = Date.now() + timeout;
    for (;;) {
      const els = await this.findAll(css, from).catch(() => []);
      if (els.length) return els[0];
      if (Date.now() > end) throw new Error(`not found: ${css}`);
      await sleep(150);
    }
  }

  async findXAll(xpath) {
    const v = await this.req('POST', this.s('/elements'), { using: 'xpath', value: xpath });
    return v.map((e) => e[ELEMENT]);
  }

  async findX(xpath, { timeout = 5000 } = {}) {
    const end = Date.now() + timeout;
    for (;;) {
      const els = await this.findXAll(xpath).catch(() => []);
      if (els.length) return els[0];
      if (Date.now() > end) throw new Error(`not found: ${xpath}`);
      await sleep(150);
    }
  }

  /** The first element whose trimmed text is `text` (or contains it). */
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

  async click(el) {
    return this.req('POST', this.s(`/element/${el}/click`), {});
  }

  async clickCss(css, opts) {
    return this.click(await this.find(css, opts));
  }

  async clickText(text, opts) {
    return this.click(await this.byText(text, opts));
  }

  async type(el, text) {
    return this.req('POST', this.s(`/element/${el}/value`), { text });
  }

  async clear(el) {
    return this.req('POST', this.s(`/element/${el}/clear`), {});
  }

  async text(el) {
    return this.req('GET', this.s(`/element/${el}/text`));
  }

  async attr(el, name) {
    return this.req('GET', this.s(`/element/${el}/attribute/${name}`));
  }

  async prop(el, name) {
    return this.req('GET', this.s(`/element/${el}/property/${name}`));
  }

  async displayed(el) {
    return this.req('GET', this.s(`/element/${el}/displayed`));
  }

  async rect(el) {
    return this.req('GET', this.s(`/element/${el}/rect`));
  }

  async exec(script, ...args) {
    return this.req('POST', this.s('/execute/sync'), { script, args });
  }

  async execAsync(script, ...args) {
    return this.req('POST', this.s('/execute/async'), { script, args });
  }

  /** Presses keys: each argument is a key or a chord array like [MOD, 's']. */
  async keys(...chords) {
    const actions = [];
    for (const chord of chords) {
      const ks = Array.isArray(chord) ? chord : [...chord];
      for (const k of ks) actions.push({ type: 'keyDown', value: k });
      for (const k of [...ks].reverse()) actions.push({ type: 'keyUp', value: k });
    }
    await this.req('POST', this.s('/actions'), {
      actions: [{ type: 'key', id: 'kbd', actions }],
    });
    await this.req('DELETE', this.s('/actions')).catch(() => {});
  }

  /** Types text with the keyboard into whatever has focus. */
  async typeKeys(text) {
    const actions = [];
    for (const ch of text) {
      actions.push({ type: 'keyDown', value: ch }, { type: 'keyUp', value: ch });
    }
    await this.req('POST', this.s('/actions'), { actions: [{ type: 'key', id: 'kbd', actions }] });
  }

  /** Mouse actions on an element: right click, double click, drag. */
  async pointer(actions) {
    await this.req('POST', this.s('/actions'), {
      actions: [{ type: 'pointer', id: 'mouse', parameters: { pointerType: 'mouse' }, actions }],
    });
    await this.req('DELETE', this.s('/actions')).catch(() => {});
  }

  async rightClick(el) {
    await this.pointer([
      { type: 'pointerMove', origin: { [ELEMENT]: el }, x: 0, y: 0 },
      { type: 'pointerDown', button: 2 },
      { type: 'pointerUp', button: 2 },
    ]);
  }

  async doubleClick(el) {
    await this.pointer([
      { type: 'pointerMove', origin: { [ELEMENT]: el }, x: 0, y: 0 },
      { type: 'pointerDown', button: 0 },
      { type: 'pointerUp', button: 0 },
      { type: 'pointerDown', button: 0 },
      { type: 'pointerUp', button: 0 },
    ]);
  }

  async modClick(el, mod) {
    await this.req('POST', this.s('/actions'), {
      actions: [
        {
          type: 'key',
          id: 'kbd',
          actions: [
            { type: 'keyDown', value: mod },
            { type: 'pause', duration: 0 },
            { type: 'pause', duration: 0 },
            { type: 'pause', duration: 0 },
            { type: 'keyUp', value: mod },
          ],
        },
        {
          type: 'pointer',
          id: 'mouse',
          parameters: { pointerType: 'mouse' },
          actions: [
            { type: 'pause', duration: 0 },
            { type: 'pointerMove', origin: { [ELEMENT]: el }, x: 0, y: 0 },
            { type: 'pointerDown', button: 0 },
            { type: 'pointerUp', button: 0 },
            { type: 'pause', duration: 0 },
          ],
        },
      ],
    });
    await this.req('DELETE', this.s('/actions')).catch(() => {});
  }

  async drag(from, to) {
    await this.pointer([
      { type: 'pointerMove', origin: { [ELEMENT]: from }, x: 0, y: 0 },
      { type: 'pointerDown', button: 0 },
      { type: 'pause', duration: 200 },
      { type: 'pointerMove', origin: { [ELEMENT]: from }, x: 5, y: 5, duration: 100 },
      { type: 'pointerMove', origin: { [ELEMENT]: to }, x: 0, y: 0, duration: 300 },
      { type: 'pause', duration: 200 },
      { type: 'pointerUp', button: 0 },
    ]);
  }

  async screenshot(name) {
    const b64 = await this.req('GET', this.s('/screenshot'));
    const file = join(this.shots, `${String(++this.shotN).padStart(3, '0')}-${name}.png`);
    writeFileSync(file, Buffer.from(b64, 'base64'));
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

export function xpathLiteral(s) {
  if (!s.includes("'")) return `'${s}'`;
  if (!s.includes('"')) return `"${s}"`;
  return `concat('${s.split("'").join(`', "'", '`)}')`;
}
