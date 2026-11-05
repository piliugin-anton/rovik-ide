// Component tests: the "Relevant prior work" panel a chat shows what the run
// remembered: one row per memory, its state and evidence, and whether this
// approach helped. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import PriorWorkPanel from './PriorWorkPanel';
import type { RecalledMemory } from '../lib/memory';

/** One memory, as the run's `memoryRecalled` event carries it. */
function memory(over: Partial<RecalledMemory> = {}): RecalledMemory {
  return {
    id: 'fx-memory',
    kind: 'episode',
    title: 'Rename the helper',
    content: 'Outcome: done',
    verification: 'provisional',
    ...over,
  };
}

function panel(memories: RecalledMemory[]): HTMLElement {
  const root = container();
  render(() => <PriorWorkPanel memories={memories} />, root);
  return root;
}

/** Each row: its chip, the chip's class and title, and everything it says. */
const rows = (root: HTMLElement) =>
  [...root.querySelectorAll('.prior-work-item')].map((li) => {
    const chip = li.querySelector('.prior-work-state') as HTMLElement | null;
    const classes = chip?.className.split(/\s+/) ?? [];
    return {
      state: chip?.textContent,
      stateClass: ['verified', 'provisional', 'deprecated'].find((c) => classes.includes(c)),
      evidence: chip?.getAttribute('title') ?? null,
      text: li.textContent ?? '',
    };
  });

const verdict = (root: HTMLElement) => root.querySelector('.prior-work-verdict')?.textContent;

test('prior work: each memory is a row with its state, kind, title, content and files', () => {
  const root = panel([
    memory({
      kind: 'episode',
      title: 'Rename the helper',
      content: 'Outcome: verified. Changed: src/a.ts',
      files: ['src/a.ts', 'src/b.ts'],
      verification: 'verified',
      evidence: 'the checks passed: cargo test',
    }),
    memory({
      id: 'fx-old',
      kind: 'project fact',
      title: 'The API lives in src/api',
      content: 'Requests go through net::client()',
      verification: 'deprecated',
      evidence: 'main failed in src/a.ts, which this memory is about',
    }),
  ]);
  const section = root.querySelector('section.prior-work') as HTMLElement;
  assert.ok(section, 'a section of its own');
  assert.equal(section.getAttribute('aria-label'), 'Relevant prior work');
  assert.match(section.textContent!, /Relevant prior work/);

  const listed = rows(root);
  assert.equal(listed.length, 2, 'one row per memory');
  // A verified memory says so, and its evidence is the chip's title.
  assert.equal(listed[0].state, 'verified');
  assert.equal(listed[0].stateClass, 'verified');
  assert.equal(listed[0].evidence, 'the checks passed: cargo test');
  for (const part of [
    'episode',
    'Rename the helper',
    'Outcome: verified. Changed: src/a.ts',
    'src/a.ts',
    'src/b.ts',
  ]) {
    assert.ok(listed[0].text.includes(part), `${part} in "${listed[0].text}"`);
  }
  // A deprecated one reads "invalidated", its chip class says deprecated.
  assert.equal(listed[1].state, 'invalidated');
  assert.equal(listed[1].stateClass, 'deprecated');
  assert.equal(listed[1].evidence, 'main failed in src/a.ts, which this memory is about');
  assert.ok(listed[1].text.includes('The API lives in src/api'));
});

test('prior work: a provisional memory reads provisional, with nothing said about it', () => {
  const root = panel([memory({ verification: 'provisional' })]);
  assert.equal(rows(root)[0].state, 'provisional');
  assert.equal(rows(root)[0].stateClass, 'provisional');
  assert.equal(verdict(root), undefined, 'no verdict before the run has one');
});

test('prior work: the run’s verdict says whether the approach helped', () => {
  const helped = panel([
    memory({
      verification: 'verified',
      evidence: 'the checks passed: cargo test',
      helped: true,
    }),
  ]);
  assert.equal(verdict(helped), 'This approach helped');

  const notHelped = panel([
    memory({
      verification: 'deprecated',
      evidence: 'cargo test failed in src/a.ts, which this memory is about',
      helped: false,
    }),
  ]);
  assert.equal(verdict(notHelped), 'This approach did not help');

  // A memory whose outcome gave no verdict says none.
  const unverified = panel([memory({ verification: 'provisional' })]);
  assert.equal(unverified.querySelector('.prior-work-verdict'), null);
});
