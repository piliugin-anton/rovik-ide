// Injected into the app's page for an end-to-end run where the webview has
// no WebDriver (macOS: WKWebView). It polls the runner
// (scripts/e2e/bridge.mjs) over HTTP and does what it's told: find
// elements, click, type, run a script. Never part of a normal build: the
// runner adds it to a copy of dist/ for its own build (see bridge.mjs).
//
// Input is synthesized, but hit-tested as the real thing would be: a click
// goes to whatever element is on top at the target's center, so something
// covering a button keeps the click from it, as it would for a person.
(function () {
  const PORT = Number(document.currentScript?.dataset.port || 17999);
  const handles = new Map();
  let next = 1;

  function ref(el) {
    if (!el) return null;
    if (!el.__e2e) {
      el.__e2e = `e${next++}`;
      handles.set(el.__e2e, el);
    }
    return { __el: el.__e2e };
  }

  function deref(value) {
    if (value && typeof value === 'object' && typeof value.__el === 'string') {
      const el = handles.get(value.__el);
      // Gone from the page (re-rendered): as WebDriver says, not clicked at 0,0.
      if (!el || !el.isConnected) throw new Error(`stale element reference ${value.__el}`);
      return el;
    }
    if (Array.isArray(value)) return value.map(deref);
    return value;
  }

  function out(value) {
    if (value instanceof Element) return ref(value);
    if (Array.isArray(value)) return value.map(out);
    return value;
  }

  function byXPath(xpath, all) {
    const result = document.evaluate(
      xpath,
      document,
      null,
      XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,
      null
    );
    const nodes = [];
    for (let i = 0; i < result.snapshotLength; i++) nodes.push(result.snapshotItem(i));
    return all ? nodes : nodes.slice(0, 1);
  }

  /**
   * Where to press: the middle of the element's visible part. Scrolled to
   * only when none of it shows: Monaco's line layer is taller than the
   * window, and scrolling it moves Monaco's DOM behind its back (the click
   * then lands on another line, or under the sticky header).
   */
  function center(el) {
    const visible = () => {
      const r = el.getBoundingClientRect();
      const left = Math.max(r.left, 0);
      const right = Math.min(r.right, innerWidth);
      const top = Math.max(r.top, 0);
      const bottom = Math.min(r.bottom, innerHeight);
      return right - left >= 1 && bottom - top >= 1
        ? { x: (left + right) / 2, y: (top + bottom) / 2 }
        : null;
    };
    const point = visible();
    if (point) return point;
    el.scrollIntoView({ block: 'center', inline: 'center' });
    const r = el.getBoundingClientRect();
    return visible() ?? { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }

  function fire(target, type, init) {
    const Ctor = type.startsWith('pointer')
      ? PointerEvent
      : type.startsWith('key')
        ? KeyboardEvent
        : MouseEvent;
    const event = new Ctor(type, {
      bubbles: true,
      cancelable: true,
      composed: true,
      view: window,
      ...init,
    });
    // Monaco reads the legacy keyCode of key events.
    if (init && init.keyCode !== undefined) {
      Object.defineProperty(event, 'keyCode', { get: () => init.keyCode });
      Object.defineProperty(event, 'which', { get: () => init.keyCode });
    }
    return target.dispatchEvent(event);
  }

  const FOCUSABLE =
    'input, textarea, select, button, a[href], [contenteditable="true"], [contenteditable=""], [tabindex]';

  /** A press and release at the element's center, on whatever is on top there. */
  function click(el, { button = 0, mods = {}, count = 1 } = {}) {
    const { x, y } = center(el);
    const target = document.elementFromPoint(x, y) || el;
    const buttons = button === 2 ? 2 : 1;
    const base = { clientX: x, clientY: y, screenX: x, screenY: y, button, ...mods };
    const pointer = { pointerId: 1, pointerType: 'mouse', isPrimary: true };
    for (let i = 1; i <= count; i++) {
      fire(target, 'pointerdown', { ...base, ...pointer, buttons, detail: i });
      const pressed = fire(target, 'mousedown', { ...base, buttons, detail: i });
      if (pressed) {
        const focusable = target.closest(FOCUSABLE);
        if (focusable && document.activeElement !== focusable) focusable.focus();
        if (focusable && focusable.isContentEditable) caretToEnd(focusable);
      }
      if (button === 2) fire(target, 'contextmenu', { ...base, buttons, detail: i });
      fire(target, 'pointerup', { ...base, ...pointer, buttons: 0, detail: i });
      fire(target, 'mouseup', { ...base, buttons: 0, detail: i });
      if (button === 0) fire(target, 'click', { ...base, buttons: 0, detail: i });
    }
    if (count === 2) fire(target, 'dblclick', { ...base, buttons: 0, detail: 2 });
    return target === el || el.contains(target)
      ? null
      : `${target.tagName.toLowerCase()}.${String(target.className)}`;
  }

  function caretToEnd(el) {
    const range = document.createRange();
    range.selectNodeContents(el);
    range.collapse(false);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  }

  /** A pointer drag from one element to another, in steps. */
  async function drag(from, to) {
    const a = center(from);
    const b = center(to);
    const pointer = { pointerId: 1, pointerType: 'mouse', isPrimary: true };
    const at = (p) => document.elementFromPoint(p.x, p.y) || document.body;
    const start = at(a);
    fire(start, 'pointerdown', { clientX: a.x, clientY: a.y, button: 0, buttons: 1, ...pointer });
    fire(start, 'mousedown', { clientX: a.x, clientY: a.y, button: 0, buttons: 1 });
    for (let i = 1; i <= 10; i++) {
      const p = { x: a.x + ((b.x - a.x) * i) / 10, y: a.y + ((b.y - a.y) * i) / 10 };
      fire(at(p), 'pointermove', { clientX: p.x, clientY: p.y, buttons: 1, ...pointer });
      fire(at(p), 'mousemove', { clientX: p.x, clientY: p.y, buttons: 1 });
      await new Promise((r) => setTimeout(r, 30));
    }
    await new Promise((r) => setTimeout(r, 150));
    const end = at(b);
    fire(end, 'pointerup', { clientX: b.x, clientY: b.y, button: 0, buttons: 0, ...pointer });
    fire(end, 'mouseup', { clientX: b.x, clientY: b.y, button: 0, buttons: 0 });
    fire(end, 'click', { clientX: b.x, clientY: b.y, button: 0, buttons: 0 });
  }

  const NAMED = {
    '': ['Enter', 'Enter', 13],
    '': ['Enter', 'Enter', 13],
    '': ['Escape', 'Escape', 27],
    '': ['Tab', 'Tab', 9],
    '': ['Backspace', 'Backspace', 8],
    '': ['Delete', 'Delete', 46],
    '': ['ArrowUp', 'ArrowUp', 38],
    '': ['ArrowDown', 'ArrowDown', 40],
    '': ['ArrowLeft', 'ArrowLeft', 37],
    '': ['ArrowRight', 'ArrowRight', 39],
    '': ['Home', 'Home', 36],
    '': ['End', 'End', 35],
    '': ['F2', 'F2', 113],
    '': ['PageDown', 'PageDown', 34],
  };
  const MODS = { '': 'ctrlKey', '': 'shiftKey', '': 'altKey', '': 'metaKey' };

  function keyInfo(k) {
    if (NAMED[k]) return NAMED[k];
    const upper = k.toUpperCase();
    if (/^[a-z]$/i.test(k)) return [k, `Key${upper}`, upper.charCodeAt(0)];
    if (/^[0-9]$/.test(k)) return [k, `Digit${k}`, k.charCodeAt(0)];
    const punct = {
      '`': ['Backquote', 192],
      '~': ['Backquote', 192],
      ' ': ['Space', 32],
      '/': ['Slash', 191],
      '.': ['Period', 190],
      ',': ['Comma', 188],
      '-': ['Minus', 189],
      '=': ['Equal', 187],
      ';': ['Semicolon', 186],
      "'": ['Quote', 222],
      '[': ['BracketLeft', 219],
      ']': ['BracketRight', 221],
      '\\': ['Backslash', 220],
    };
    const p = punct[k];
    return p ? [k, p[0], p[1]] : [k, '', 0];
  }

  function editable(el) {
    return (
      el &&
      ((el.tagName === 'INPUT' && !['checkbox', 'radio', 'button', 'file'].includes(el.type)) ||
        el.tagName === 'TEXTAREA' ||
        el.isContentEditable)
    );
  }

  /**
   * Typing into an element edited through an EditContext (Monaco in
   * Chromium, so WebView2): what the browser does for a key there. The
   * context's text and caret change, then a textupdate says how (Enter: a
   * beforeinput on the element). execCommand doesn't reach it, and without
   * the caret moved each key replaced the one before (the editor brings
   * the context up to date only when it next renders).
   */
  function typeIntoEditContext(target, context, name) {
    if (name === 'Enter') {
      target.dispatchEvent(
        new InputEvent('beforeinput', {
          inputType: 'insertParagraph',
          bubbles: true,
          cancelable: true,
        })
      );
    } else if (name.length === 1) {
      const start = Math.min(context.selectionStart, context.selectionEnd);
      const end = Math.max(context.selectionStart, context.selectionEnd);
      const caret = start + name.length;
      context.updateText(start, end, name);
      context.updateSelection(caret, caret);
      context.dispatchEvent(
        new TextUpdateEvent('textupdate', {
          updateRangeStart: start,
          updateRangeEnd: end,
          text: name,
          selectionStart: caret,
          selectionEnd: caret,
        })
      );
    }
  }

  /** One key with modifiers held: keydown, the text it types, keyup. */
  function press(keys) {
    const mods = {};
    let key = null;
    for (const k of keys) {
      if (MODS[k]) mods[MODS[k]] = true;
      else key = k;
    }
    if (key === null) return;
    const [name, code, keyCode] = keyInfo(key);
    const target = document.activeElement || document.body;
    const init = { key: name, code, keyCode, ...mods };
    const allowed = fire(target, 'keydown', init);
    const plain = !mods.ctrlKey && !mods.metaKey && !mods.altKey;
    if (allowed && plain && target.editContext)
      typeIntoEditContext(target, target.editContext, name);
    else if (allowed && plain && editable(target)) {
      if (name === 'Enter') {
        if (target.tagName !== 'INPUT') document.execCommand('insertText', false, '\n');
      } else if (name === 'Backspace') document.execCommand('delete');
      else if (name === 'Delete') document.execCommand('forwardDelete');
      else if (name.length === 1) document.execCommand('insertText', false, name);
    }
    if (
      allowed &&
      mods.ctrlKey !== mods.metaKey &&
      name.toLowerCase() === 'a' &&
      editable(target)
    ) {
      document.execCommand('selectAll');
    }
    fire(target, 'keyup', init);
  }

  async function handle(op, args) {
    switch (op) {
      case 'exec': {
        const fn = new Function(args.script);
        return out(await fn.apply(null, deref(args.args || [])));
      }
      case 'find':
        return out(document.querySelector(args.css));
      case 'findAll':
        return out([...document.querySelectorAll(args.css)]);
      case 'findX':
        return out(byXPath(args.xpath, args.all));
      case 'click':
        return click(deref(args.el), args.opts);
      case 'drag':
        return drag(deref(args.from), deref(args.to));
      case 'type': {
        const el = deref(args.el);
        el.focus();
        if (el.tagName === 'INPUT' && el.type === 'file')
          throw new Error('file inputs need a person');
        for (const ch of args.text) press([ch]);
        return null;
      }
      case 'keys':
        for (const chord of args.chords) press(chord);
        return null;
      case 'rect': {
        const r = deref(args.el).getBoundingClientRect();
        return { x: r.left, y: r.top, width: r.width, height: r.height };
      }
      case 'text':
        return deref(args.el).innerText;
      case 'window':
        return {
          screenX: window.screenX,
          screenY: window.screenY,
          outerWidth: window.outerWidth,
          outerHeight: window.outerHeight,
          innerWidth: window.innerWidth,
          innerHeight: window.innerHeight,
          dpr: window.devicePixelRatio,
        };
      case 'ping':
        return 'pong';
      default:
        throw new Error(`unknown op ${op}`);
    }
  }

  // Long polling: ask the runner for the next command, answer it, repeat.
  // (fetch, not a WebSocket: the runner then needs no WebSocket server.)
  async function serve() {
    const base = `http://127.0.0.1:${PORT}`;
    for (;;) {
      let command;
      try {
        const res = await fetch(`${base}/next`, { method: 'POST', body: '' });
        command = await res.json();
      } catch {
        await new Promise((r) => setTimeout(r, 1000));
        continue;
      }
      if (!command) continue;
      const { id, op, args } = command;
      let reply;
      try {
        const value = await handle(op, args || {});
        reply = { id, value: value === undefined ? null : value };
      } catch (e) {
        // The message first: WebKit's stack is only the frames, and the
        // runner tells a stale element from the message.
        const message = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
        const stack = e instanceof Error && e.stack ? `\n${e.stack}` : '';
        reply = { id, error: stack.includes(message) ? stack.trim() : message + stack };
      }
      try {
        // text/plain: a simple request, no CORS preflight.
        await fetch(`${base}/result`, { method: 'POST', body: JSON.stringify(reply) });
      } catch {
        // the runner went away; the next poll waits for it
      }
    }
  }
  void serve();
})();
