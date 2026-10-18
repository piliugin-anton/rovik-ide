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
