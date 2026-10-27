// styles.css tests (node:test): the subagents' selectors have rules, and
// the tokens the stylesheet uses are declared. Run with: npm test (from the
// project's folder: the files are read from there).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const css = readFileSync('src/styles.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

/** Each innermost rule: its selectors (one per comma) and its body. */
const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({
  selectors: m[1].split(',').map((s) => s.trim().replace(/\s+/g, ' ')),
  body: m[2],
}));

/** Custom properties declared in `:root`. */
const declared = new Set(
  rules
    .filter((r) => r.selectors.includes(':root'))
    .flatMap((r) => [...r.body.matchAll(/(--[\w-]+)\s*:/g)].map((m) => m[1]))
);

/** `var(--x)` with no fallback: what must be declared. */
const required = (body: string) => [...body.matchAll(/var\(\s*(--[\w-]+)\s*\)/g)].map((m) => m[1]);

/** The records' and subagent blocks' states (P5.6). */
const NEW_SELECTORS = [
  '.chat-verify.running',
  '.chat-review.running',
  '.chat-verify.stopped',
  '.chat-review.stopped',
  '.chat-verify-triage',
  '.chat-verify-triage.running',
  '.chat-verify-triage.done',
  '.chat-verify-triage.skipped',
  '.chat-verify-triage.stopped',
  '.review-guide',
  '.review-guide.running',
  '.review-guide.done',
  '.review-guide.skipped',
  '.review-guide.stopped',
  '.review-rank',
  '.review-outside',
  '.review-optional summary',
  '.review-noted summary',
  '.chat-verify-repair',
  '.subagent-block',
  '.subagent-block.running',
  '.subagent-context',
  '.subagent-group',
  '.subagent-limits-toggle',
];

test('styles_declare_every_new_selector_and_token', () => {
  for (const selector of NEW_SELECTORS) {
    const own = rules.filter((r) => r.selectors.includes(selector));
    assert.ok(own.length > 0, `${selector} has a rule`);
    for (const r of own)
      for (const name of required(r.body))
        assert.ok(declared.has(name), `${selector}: var(${name}) is declared in :root`);
  }
  // Anywhere: a token with no fallback is declared.
  for (const r of rules)
    for (const name of required(r.body))
      assert.ok(declared.has(name), `${r.selectors.join(', ')}: var(${name}) is declared`);
});

test('every subagent-* class the components use has a rule', () => {
  const styled = new Set(
    rules.flatMap((r) =>
      r.selectors.flatMap((s) => [...s.matchAll(/\.([\w-]+)/g)].map((m) => m[1]))
    )
  );
  const dir = 'src/components';
  const used = new Set<string>();
  for (const file of readdirSync(dir).filter((f) => /\.tsx$/.test(f) && !/\.test\./.test(f))) {
    const source = readFileSync(join(dir, file), 'utf8');
    for (const m of source.matchAll(/\bsubagent-[\w-]+/g)) used.add(m[0]);
  }
  assert.ok(used.size > 10, `found the classes (${used.size})`);
  const missing = [...used].filter((c) => !styled.has(c));
  assert.deepEqual(missing, [], 'unstyled subagent classes');
});

test('every autopilot-* and chat-stage class the components use has a rule', () => {
  const styled = new Set(
    rules.flatMap((r) =>
      r.selectors.flatMap((s) => [...s.matchAll(/\.([\w-]+)/g)].map((m) => m[1]))
    )
  );
  const dir = 'src/components';
  const used = new Set<string>();
  for (const file of readdirSync(dir).filter((f) => /\.tsx$/.test(f) && !/\.test\./.test(f))) {
    const source = readFileSync(join(dir, file), 'utf8');
    // The classes a class attribute names, a template's words included.
    for (const m of source.matchAll(/\bclass=\{?[`"']([^`"']*)[`"']/g)) {
      for (const word of m[1].split(/\s+/)) {
        if (/^(autopilot-|chat-stage)[\w-]*$/.test(word)) used.add(word);
      }
    }
  }
  assert.ok(used.size > 5, `found the classes (${used.size})`);
  const missing = [...used].filter((c) => !styled.has(c));
  assert.deepEqual(missing, [], 'unstyled autopilot/chat-stage classes');
});
/** The bodies of the rules a selector list names, joined with newlines. */
const bodiesOf = (selector: string) =>
  rules.filter((r) => r.selectors.some((s) => s.endsWith(selector))).map((r) => r.body);

/** The bodies of the rules that style `target` under an expanded sidebar. */
const expandedBodies = (target: string) =>
  rules
    .filter((r) => r.selectors.some((s) => s.includes('sidebar-expanded') && s.endsWith(target)))
    .map((r) => r.body);

/** The text inside each `@media` block whose prelude matches. */
const mediaBlocks = (prelude: RegExp) => {
  const blocks: string[] = [];
  for (let at = css.indexOf('@media'); at !== -1; at = css.indexOf('@media', at + 1)) {
    const open = css.indexOf('{', at);
    let depth = 0;
    let end = open;
    for (; end < css.length; end++) {
      if (css[end] === '{') depth++;
      else if (css[end] === '}' && --depth === 0) break;
    }
    const head = css.slice(at, open).replace(/\s+/g, ' ');
    if (prelude.test(head)) blocks.push(css.slice(open + 1, end));
  }
  return blocks;
};

test('the expanded panel: a 333ms width, the hidden right pane and a 768px chat column', () => {
  // Criterion 3: the width change is animated, and instant while dragging.
  assert.ok(
    bodiesOf('.resizable-inner').some((b) => /position:\s*relative/.test(b)),
    '.resizable-inner is a positioned ancestor for the floating button'
  );
  assert.ok(
    bodiesOf('.resizable-inner').some((b) => /transition:\s*width\s+333ms/.test(b)),
    '.resizable-inner transitions its width over 333ms'
  );
  assert.ok(
    bodiesOf('.resizable-inner.resizing').some((b) => /transition:\s*none/.test(b)),
    '.resizable-inner.resizing turns the transition off'
  );
  // Criterion 3: off under reduced motion too.
  assert.ok(
    mediaBlocks(/prefers-reduced-motion/).some((b) =>
      /\.resizable-inner\b[^{}]*\{[^{}]*transition:\s*none/.test(b)
    ),
    'reduced motion turns the width transition off'
  );

  // Criterion 4: the open-file tabs, the editor and the terminal, and the divider, are hidden.
  assert.ok(
    expandedBodies('.app-main').some((b) => /display:\s*none/.test(b)),
    '.app.sidebar-expanded hides the right pane'
  );
  assert.ok(
    expandedBodies('.resizable-handle').some((b) => /display:\s*none/.test(b)),
    '.app.sidebar-expanded hides the divider'
  );

  // Criterion 5: the chat tab's column is capped at 768px and centered; nothing caps Files or Git.
  const chat = expandedBodies('.chat-panel').join('\n');
  assert.ok(chat.length > 0, 'the expanded chat panel has a rule');
  assert.match(chat, /max-width:\s*768px/);
  assert.match(chat, /margin(-inline)?\s*:\s*[^;]*auto/, 'the chat column is centered');

  // Criterion 1: the floating button sits at the panel's right edge, vertically centered.
  const button = bodiesOf('.sidebar-expand-button').join('\n');
  assert.ok(button.length > 0, '.sidebar-expand-button has a rule');
  assert.match(button, /position:\s*absolute/);
  assert.match(button, /right:\s*6px/);
  assert.match(button, /top:\s*50%/);
  assert.match(button, /translateY\(\s*-50%\s*\)/);
});

test('a chat record card wraps what it holds instead of spilling out of it', () => {
  // A report's facts (ReportFacts.tsx) carry paths, commands, branch names and
  // pull-request URLs. A token too long for the card breaks inside it: the
  // grid's second column may shrink (min-width: 0) and the record wraps.
  assert.ok(
    bodiesOf('.chat-verify').some((b) => /overflow-wrap:\s*anywhere/.test(b)),
    '.chat-verify breaks a token too long for the card'
  );
  assert.ok(
    bodiesOf('.chat-report-facts dd').some((b) => /min-width:\s*0/.test(b)),
    'a fact may be narrower than its longest token'
  );
});

test('the model picker’s source switch keeps its height and its three buttons', () => {
  // The popover's height is capped: a long list scrolls, and once squeezed
  // the switch above it all but vanished. In a narrow chat the long labels
  // are cut short, never "All" or a whole button.
  assert.ok(
    bodiesOf('.model-picker-sources').some((b) => /flex:\s*0 0 auto/.test(b)),
    'the switch never shrinks in the popover'
  );
  assert.ok(
    bodiesOf('.model-picker-sources .segmented-option').some((b) => /min-width:\s*0/.test(b)),
    'a button may be narrower than its label'
  );
  assert.ok(
    bodiesOf('.model-picker-sources .segmented-option > .model-picker-source-label').some((b) =>
      /text-overflow:\s*ellipsis/.test(b)
    ),
    'a label is cut short with an ellipsis'
  );
  assert.ok(
    bodiesOf(".model-picker-sources .segmented-option[data-source='all']").some((b) =>
      /flex:\s*0 0 auto/.test(b)
    ),
    '"All" keeps its width'
  );
});

test('the command picker keeps a command’s name whole: its hint, then its aliases, give way', () => {
  // Claude Code's `/code-review` takes a hint wider than the chat, and a
  // skill synced from claude.ai has an alias such as
  // `/anthropic-skills:google-workspace`. The name shrank with them, cut to
  // "/c…" in the sidebar.
  const name = bodiesOf('.command-suggestions .file-suggestion-head .file-suggestion-name');
  assert.ok(
    name.some((b) => /flex-shrink:\s*0/.test(b)),
    'the name never shrinks'
  );
  assert.ok(
    name.some((b) => /max-width:\s*100%/.test(b)),
    'a name wider than the whole row is cut at its edge'
  );
  const hint = bodiesOf('.command-suggestion-hint');
  assert.ok(
    hint.some((b) => /flex:\s*1\s*;/.test(b) && /min-width:\s*0/.test(b)),
    'the hint only fills the room the name and aliases leave'
  );
  const aliases = bodiesOf('.command-suggestions .file-suggestion-tag');
  assert.ok(
    aliases.some(
      (b) =>
        /flex-shrink:\s*1/.test(b) &&
        /min-width:\s*0/.test(b) &&
        /text-overflow:\s*ellipsis/.test(b)
    ),
    'aliases too long for the row are cut short with an ellipsis'
  );
  assert.ok(
    bodiesOf('.command-suggestions .file-suggestion-head').some((b) =>
      /overflow:\s*hidden/.test(b)
    ),
    'nothing spills out of the row, so the list never scrolls sideways'
  );
});

test('criterion 2: App owns the expanded state and marks its root with sidebar-expanded', () => {
  const app = readFileSync('src/App.tsx', 'utf8');
  assert.match(app, /sidebar-expanded/, 'the root .app element gets the sidebar-expanded class');
  assert.match(app, /expanded=\{[^}]*\}/, 'the panel is given the expanded state');
  assert.match(app, /onExpandedChange=\{/, 'the panel can report the toggle back to App');
  const panel = readFileSync('src/components/ResizableDiv.tsx', 'utf8');
  assert.match(panel, /sidebar-expand-button/, 'the panel renders the floating button');
  assert.match(
    app,
    /showFloatingButton=\{sidebarTab\(\) === 'chat'\}/,
    'the floating button shows on the Chat tab only'
  );
});

// --- The floating collapse button beside the expanded chat column ------------
// Criteria 1-5: expanded, the button's left edge is 8px right of the chat
// column's border (or, in a window too narrow for that gap, 6px in from the
// window's right edge), while the column and the button's vertical centre stay
// exactly as they are.

/** The declarations of some rule bodies, as property/value pairs. */
const declarationsOf = (bodies: string[]) =>
  bodies.flatMap((body) =>
    body
      .split(';')
      .map((d) => d.trim())
      .filter((d) => d.length > 0)
      .map((d) => {
        const at = d.indexOf(':');
        return { prop: d.slice(0, at).trim().toLowerCase(), value: d.slice(at + 1).trim() };
      })
  );

/** The bodies of the rules that place the button in its expanded state. */
const expandedButtonBodies = () =>
  rules
    .filter((r) =>
      r.selectors.some(
        (s) => /\.sidebar-expand-button\b/.test(s) && s.includes('expanded') && !s.includes(':')
      )
    )
    .map((r) => r.body);

/** The declarations that place the button in its expanded state. */
const expandedButtonDeclarations = () => declarationsOf(expandedButtonBodies());

/** The declarations of the button's collapsed (base) rule. */
const collapsedButtonDeclarations = () => declarationsOf(bodiesOf('.sidebar-expand-button'));

/** Every value the rules give a property of the button, `taken` preferred. */
const buttonValues = (taken: { prop: string; value: string }[], prop: string) =>
  taken.filter((d) => d.prop === prop).map((d) => d.value);

/** The value a property ends up with: the expanded rule's, else the base rule's. */
const buttonProp = (prop: string) => {
  const expanded = buttonValues(expandedButtonDeclarations(), prop);
  return expanded.length > 0 ? expanded : buttonValues(collapsedButtonDeclarations(), prop);
};

/** The `left` the expanded rule gives the button. */
const expandedLeft = () => buttonValues(expandedButtonDeclarations(), 'left')[0];

/**
 * A CSS length at a window `w` px wide: px terms as they are, `%` terms against
 * `w`, `+ - * /`, parentheses, `calc()` and `min()`.
 */
const cssLength = (text: string, w: number): number => {
  let i = 0;
  const skip = () => {
    while (i < text.length && /\s/.test(text.charAt(i))) i++;
  };
  const atom = (): number => {
    skip();
    if (text.startsWith('min(', i)) {
      i += 4;
      const first = sum();
      skip();
      if (text.charAt(i) === ',') i++;
      const second = sum();
      skip();
      if (text.charAt(i) === ')') i++;
      return Math.min(first, second);
    }
    if (text.startsWith('calc(', i)) {
      i += 5;
      const inner = sum();
      skip();
      if (text.charAt(i) === ')') i++;
      return inner;
    }
    if (text.charAt(i) === '(') {
      i++;
      const inner = sum();
      skip();
      if (text.charAt(i) === ')') i++;
      return inner;
    }
    if (text.charAt(i) === '-') {
      i++;
      return -atom();
    }
    const m = /^(\d+(?:\.\d+)?)(%|px)?/.exec(text.slice(i));
    if (!m) throw new Error(`not a length: "${text.slice(i)}"`);
    i += m[0].length;
    const n = Number(m[1]);
    return m[2] === '%' ? (n / 100) * w : n;
  };
  const product = (): number => {
    let value = atom();
    for (;;) {
      skip();
      const c = text.charAt(i);
      if (c === '*') {
        i++;
        value *= atom();
      } else if (c === '/') {
        i++;
        value /= atom();
      } else return value;
    }
  };
  const sum = (): number => {
    let value = product();
    for (;;) {
      skip();
      const c = text.charAt(i);
      if (c === '+') {
        i++;
        value += product();
      } else if (c === '-') {
        i++;
        value -= product();
      } else return value;
    }
  };
  const value = sum();
  skip();
  if (i !== text.length) throw new Error(`unexpected "${text.slice(i)}" in "${text}"`);
  return value;
};

/** How wide the chat column's border box is at most (`.chat-panel`, expanded). */
const chatColumnCap = () => {
  const chat = expandedBodies('.chat-panel').join('\n');
  const cap = /max-width\s*:\s*(\d+)px/.exec(chat);
  assert.ok(cap, 'the expanded chat column has a pixel max width');
  return Number(cap![1]);
};

test('criterion 1: the expanded chat column keeps its border, width cap and centring', () => {
  const chat = expandedBodies('.chat-panel').join('\n');
  assert.ok(chat.length > 0, 'the expanded chat panel has a rule');
  assert.match(
    chat,
    /border\s*:\s*1px solid var\(\s*--border\s*\)/,
    'the chat column keeps its 1px border'
  );
  assert.match(chat, /max-width\s*:\s*768px/, 'the chat column keeps its 768px cap');
  assert.match(chat, /width\s*:\s*100%/, 'the column takes the full width up to the cap');
  assert.match(
    chat,
    /margin(-inline)?\s*:\s*[^;]*auto/,
    'the chat column is still centred horizontally'
  );
});

test('criterion 2: expanded, the button sits 8px right of the chat column border', () => {
  assert.ok(
    expandedButtonBodies().length > 0,
    '.sidebar-expand-button has a rule for the expanded state'
  );
  assert.deepEqual(
    buttonValues(expandedButtonDeclarations(), 'right'),
    ['auto'],
    'the expanded button clears the base right: 6px'
  );
  const left = expandedLeft();
  assert.ok(left, 'the expanded button is placed from its left edge');
  assert.match(left!, /min\(/, 'the placement is clamped, so a narrow window keeps it visible');

  // The column is centred and at most 768px wide, so its right border sits
  // 768px / 2 right of the window's middle; the button's left edge is 8px past
  // it. (Its own 1px border sits inside its 22px border-box width.)
  for (const w of [1600, 1200, 1000, 860]) {
    assert.equal(
      cssLength(left!, w),
      w / 2 + 768 / 2 + 8,
      `in a ${w}px window the button's left edge is 8px right of the column's border`
    );
  }
});

test("criterion 2: the button's offset is derived from the column's own width cap", () => {
  const left = expandedLeft();
  assert.ok(left, 'the expanded button is placed from its left edge');
  const cap = chatColumnCap();
  assert.match(left!, /50%/, "the offset is measured from the window's middle");
  assert.ok(
    new RegExp(`(?:${cap}px\\s*/\\s*2|${cap / 2}px)\\s*\\+\\s*8px`).test(left!) ||
      new RegExp(`8px\\s*\\+\\s*(?:${cap}px\\s*/\\s*2|${cap / 2}px)`).test(left!),
    `half the ${cap}px column plus the 8px gap (in "${left}")`
  );
});

test('criterion 4: a window too narrow for the gap keeps the button at its right edge', () => {
  const left = expandedLeft();
  assert.ok(left, 'the expanded button is placed from its left edge');
  // The button is 22px wide; its right edge stays at most 6px inside the
  // window, so `.resizable-inner { overflow: hidden }` cannot clip it.
  for (const w of [1000, 860, 840, 839, 800, 700, 480, 360, 260]) {
    const x = cssLength(left!, w);
    assert.ok(x >= 0, `a ${w}px window: the left edge is on screen (${x}px)`);
    assert.ok(x + 22 <= w, `a ${w}px window: the button is fully inside (${x + 22} <= ${w})`);
    assert.ok(
      x + 22 <= w - 6 + 1e-9,
      `a ${w}px window: the button is 6px or more in from the right edge (${x + 22} <= ${w - 6})`
    );
  }
  // Just inside the crossover the clamp takes over: 22px + 6px from the right.
  for (const w of [839, 800, 700, 480, 360]) {
    assert.equal(
      cssLength(left!, w),
      w - 28,
      `a ${w}px window: the button mirrors the collapsed right: 6px inset`
    );
  }
  // And just outside it the gap is full, so nothing shrinks before it must.
  assert.equal(cssLength(left!, 840), 840 / 2 + 768 / 2 + 8, 'at 840px the two meet exactly');
  assert.ok(
    cssLength(left!, 900) === 900 / 2 + 768 / 2 + 8,
    'at 900px there is room for the full gap'
  );
});

test('criterion 3: the button keeps its vertical centre in both states', () => {
  assert.deepEqual(
    buttonValues(collapsedButtonDeclarations(), 'top'),
    ['50%'],
    'the collapsed button is vertically centred'
  );
  assert.ok(
    buttonValues(collapsedButtonDeclarations(), 'transform').some((v) =>
      /translateY\(\s*-50%\s*\)/.test(v)
    ),
    'the collapsed button is pulled up by half its height'
  );
  for (const value of buttonValues(expandedButtonDeclarations(), 'top'))
    assert.equal(value, '50%', 'the expanded rule leaves the vertical centre where it was');
  for (const value of buttonValues(expandedButtonDeclarations(), 'transform'))
    assert.match(
      value.replace(/\s+/g, ''),
      /translateY\(-50%\)/,
      'the expanded rule leaves the vertical transform where it was'
    );
  // Whichever rule wins, the placement is the same vertical centre as before.
  assert.deepEqual(buttonProp('top'), ['50%'], 'the button is vertically centred when expanded');
  assert.ok(
    buttonProp('transform').some((v) => /translateY\(\s*-50%\s*\)/.test(v)),
    'the button is pulled up by half its height when expanded'
  );
});

test('criterion 5: the collapsed button is unchanged', () => {
  const collapsed = collapsedButtonDeclarations();
  const one = (prop: string) => buttonValues(collapsed, prop);
  assert.deepEqual(one('position'), ['absolute'], 'the button still floats over the panel');
  assert.deepEqual(one('right'), ['6px'], 'the collapsed button keeps its 6px inset');
  assert.deepEqual(one('top'), ['50%'], 'and its vertical centre');
  assert.ok(
    one('transform').some((v) => /translateY\(\s*-50%\s*\)/.test(v)),
    'and its half-height lift'
  );
  assert.deepEqual(one('width'), ['22px'], 'the button is 22px wide');
  assert.deepEqual(one('height'), ['22px'], 'the button is 22px tall');
  assert.ok(
    one('border').some((v) => /^1px\s+solid\b/.test(v)),
    'the button keeps its 1px border'
  );
  assert.deepEqual(
    one('left'),
    [],
    'the collapsed button is placed from the right only, never from the left'
  );
  // Faint until hover, as before: the rule that makes it faint is still there.
  const hover = rules
    .filter((r) =>
      r.selectors.some((s) => /\.sidebar-expand-button\b/.test(s) && s.includes(':hover'))
    )
    .map((r) => r.body)
    .join('\n');
  assert.match(
    collapsed.map((d) => `${d.prop}: ${d.value}`).join('; ') + '; ' + hover,
    /opacity\s*:/,
    'the button is still faint until hover'
  );
  // The expanded rule changes the horizontal placement only: size, border and
  // the rest of the collapsed look carry over untouched.
  const touched = expandedButtonDeclarations().map((d) => d.prop);
  assert.deepEqual(
    touched.filter((p) => ['width', 'height', 'border'].includes(p)),
    [],
    'the expanded rule changes only where the button sits'
  );
  // Its opacity is the one thing that predates this change: the shared
  // hover/focus/expanded rule lifts it, before and after.
  assert.ok(
    expandedButtonDeclarations().some((d) => d.prop === 'opacity' && d.value === '1'),
    'the expanded button is still fully opaque'
  );
});

// --- The editor pane never scrolls: long lines scroll inside Monaco --------
// Plan criteria 1-2 (2026-10-23-long-lines-may-make-editor-to-show): Monaco
// measures its host (`.editor`, the element handed to `monaco.editor.create`)
// with `clientWidth`/`clientHeight`, which count CSS padding, and sizes its
// own node to the measured numbers. CSS padding on the host therefore makes
// the editor box overflow the pane, so the pane scrolls (a horizontal
// scrollbar appears and Monaco's own vertical scrollbar is pushed out of the
// visible area). The host carries no padding and never scrolls; the 8px
// vertical breathing room is Monaco's own `padding` option (as in
// `components/CodeBlock.tsx`), which shifts content inside the editor and
// cannot make it overflow.

test('criterion 1: the .editor host never scrolls and carries no padding', () => {
  const bodies = bodiesOf('.editor');
  assert.ok(bodies.length > 0, '.editor has a rule');
  const decls = declarationsOf(bodies);
  // The pane is not a scroll box: scrolling happens inside Monaco.
  assert.ok(
    decls.some((d) => d.prop === 'overflow' && d.value === 'hidden'),
    '.editor declares overflow: hidden'
  );
  // No CSS padding on the Monaco host: clientWidth counts padding, so any
  // padding would size Monaco larger than the pane and scroll it.
  assert.deepEqual(
    decls.filter((d) => d.prop === 'padding' || d.prop.startsWith('padding-')),
    [],
    '.editor declares no padding'
  );
  // No scrolling overflow may come back on any axis.
  for (const d of decls) {
    if (d.prop === 'overflow' || d.prop === 'overflow-x' || d.prop === 'overflow-y') {
      assert.ok(!/\b(auto|scroll)\b/.test(d.value), `.editor never uses ${d.prop}: ${d.value}`);
    }
  }
});

test('criterion 2: Editor.tsx gives the vertical breathing room to Monaco itself', () => {
  const source = readFileSync('src/components/Editor.tsx', 'utf8');
  const at = source.indexOf('monaco.editor.create(');
  assert.ok(at !== -1, 'Editor hands .editor to monaco.editor.create');
  // From the create call on: the 8px above the first and below the last line
  // stay without the pane scrolling, because Monaco's padding option shifts
  // content inside the editor.
  const created = source.slice(at);
  assert.match(
    created,
    /padding\s*:\s*\{\s*top\s*:\s*8\s*,\s*bottom\s*:\s*8\s*\}/,
    'monaco.editor.create passes padding: { top: 8, bottom: 8 }'
  );
});
