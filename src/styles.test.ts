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
