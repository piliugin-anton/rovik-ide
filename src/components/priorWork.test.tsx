// Component tests: the "Relevant prior work" panel a chat shows what the run
// remembered: one row per memory, its state and evidence, and which checks the
// run reported after them. Run with: npm test
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

test('prior work: the run reports which checks came after the memories', () => {
  const passed = panel([memory({ verification: 'provisional', association: 'checksPassed' })]);
  assert.equal(verdict(passed), 'The checks passed after this');

  const failed = panel([memory({ verification: 'provisional', association: 'checksFailed' })]);
  const failing = verdict(failed) ?? '';
  assert.notEqual(
    failing,
    'The checks passed after this',
    'a run whose checks failed does not say they passed'
  );
  assert.ok(!/helped/i.test(failing), `the panel calls no memory helpful: ${failing}`);
  assert.match(failing, /checks failed/i, `the panel says the checks failed: ${failing}`);

  // A run that checked nothing says nothing.
  const unchecked = panel([memory({ verification: 'provisional' })]);
  assert.equal(unchecked.querySelector('.prior-work-verdict'), null);
});

test('prior work: a row says whether the memory still holds for the code', () => {
  const root = panel([
    memory({ title: 'The retry needs a lock', applicability: 'needs_revalidation' }),
    memory({ id: 'fx-current', title: 'The queue is bounded', applicability: 'current' }),
  ]);
  const listed = [...root.querySelectorAll('.prior-work-item')];
  assert.equal(listed.length, 2, 'one row per memory');
  assert.match(
    listed[0].textContent ?? '',
    /revalidat|check again|stale/i,
    `the row says the claim has to be checked again: "${listed[0].textContent}"`
  );
  assert.doesNotMatch(
    listed[1].textContent ?? '',
    /revalidat|check again|stale/i,
    `a claim expected to hold is not marked stale: "${listed[1].textContent}"`
  );
});

test('prior work: the card folds to its header and opens again', () => {
  // The other tests here render an open card: the fold is remembered per app.
  localStorage.removeItem('rovik.priorWorkCollapsed');
  const root = panel([memory(), memory({ id: 'fx-old' })]);
  const card = () => root.querySelector<HTMLElement>('section.prior-work')!;
  const head = () => root.querySelector<HTMLButtonElement>('button.prior-work-head')!;

  assert.ok(head(), 'the header is a button');
  assert.equal(head().getAttribute('aria-expanded'), 'true');
  assert.equal(rows(root).length, 2);

  head().click();
  assert.equal(head().getAttribute('aria-expanded'), 'false');
  assert.equal(rows(root).length, 0);
  assert.match(head().textContent ?? '', /Relevant prior work/);
  assert.ok(card().classList.contains('collapsed'));
  assert.equal(localStorage.getItem('rovik.priorWorkCollapsed'), '1');

  head().click();
  assert.equal(head().getAttribute('aria-expanded'), 'true');
  assert.equal(rows(root).length, 2);
  assert.ok(!card().classList.contains('collapsed'));
  assert.equal(localStorage.getItem('rovik.priorWorkCollapsed'), '0');
  localStorage.removeItem('rovik.priorWorkCollapsed');
});

test('prior work: a folded card still says how the run ended', () => {
  localStorage.setItem('rovik.priorWorkCollapsed', '1');
  const root = panel([memory({ association: 'checksPassed' })]);
  // The card reads the key at creation: keep the store clean for the other tests.
  localStorage.removeItem('rovik.priorWorkCollapsed');
  const card = root.querySelector<HTMLElement>('section.prior-work')!;
  const head = root.querySelector<HTMLButtonElement>('button.prior-work-head')!;
  const shown = head.querySelector('.prior-work-verdict')?.textContent;

  assert.ok(card.classList.contains('collapsed'), 'it starts folded');
  assert.equal(card.getAttribute('aria-label'), 'Relevant prior work');
  assert.equal(rows(root).length, 0, 'no memory row shows');
  assert.equal(head.getAttribute('aria-expanded'), 'false');
  assert.equal(verdict(root), 'The checks passed after this');
  assert.equal(shown, 'The checks passed after this');
  assert.match(head.textContent ?? '', /Relevant prior work/);
});
