import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  applyHunks,
  diffLines,
  diffRows,
  snippetLines,
  splitLines,
  type DiffHunk,
} from './lineDiff';

/*
 * A partly accepted agent edit is written to disk as applyHunks computes it
 * (approvals.ts): a wrong hunk here is a corrupted file.
 */

const lines = (text: string) => (text === '' ? [] : text.split(','));

/** The hunks' invariants: in order, apart, and describing `a` and `b` exactly. */
function checkHunks(a: string[], b: string[], hunks: DiffHunk[]) {
  let after = 0; // first line of `a` a hunk may start at (0-based)
  for (const h of hunks) {
    const at = h.origStart - 1;
    assert.ok(h.deleted.length + h.added.length > 0, 'an empty hunk');
    assert.ok(at >= after, `hunk at ${h.origStart} overlaps or touches the one before`);
    assert.deepEqual(
      h.deleted,
      a.slice(at, at + h.deleted.length),
      'deleted lines are the original'
    );
    // At least one unchanged line between hunks, or they'd be one.
    after = at + h.deleted.length + 1;
  }
  assert.deepEqual(
    applyHunks(a, hunks, () => true),
    b,
    'all accepted gives the new text'
  );
  assert.deepEqual(
    applyHunks(a, hunks, () => false),
    a,
    'all rejected gives the original'
  );
}

/** Applying the kept hunks by splicing from the end: independent of applyHunks. */
function spliced(a: string[], hunks: DiffHunk[], keep: boolean[]): string[] {
  const out = [...a];
  for (let i = hunks.length - 1; i >= 0; i--) {
    if (keep[i]) out.splice(hunks[i].origStart - 1, hunks[i].deleted.length, ...hunks[i].added);
  }
  return out;
}

test('known edits', () => {
  const cases: [string, string, DiffHunk[]][] = [
    ['a,b,c', 'a,b,c', []],
    ['', '', []],
    ['', 'x,y', [{ origStart: 1, deleted: [], added: ['x', 'y'] }]],
    ['x,y', '', [{ origStart: 1, deleted: ['x', 'y'], added: [] }]],
    ['a,b,c', 'z,a,b,c', [{ origStart: 1, deleted: [], added: ['z'] }]],
    ['a,b,c', 'a,b,c,z', [{ origStart: 4, deleted: [], added: ['z'] }]],
    ['a,b,c', 'a,c', [{ origStart: 2, deleted: ['b'], added: [] }]],
    ['a,b,c', 'a,B,c', [{ origStart: 2, deleted: ['b'], added: ['B'] }]],
    [
      'a,b,c,d,e',
      'A,b,c,d,E',
      [
        { origStart: 1, deleted: ['a'], added: ['A'] },
        { origStart: 5, deleted: ['e'], added: ['E'] },
      ],
    ],
    ['a,b', 'c,d', [{ origStart: 1, deleted: ['a', 'b'], added: ['c', 'd'] }]],
  ];
  for (const [from, to, want] of cases) {
    const a = lines(from);
    const b = lines(to);
    const hunks = diffLines(a, b);
    assert.deepEqual(hunks, want, `${from} → ${to}`);
    checkHunks(a, b, hunks);
  }
});

test('every mix of accepted and rejected changes gives the right text', () => {
  // A small alphabet makes many equal lines, so diffs have several hunks.
  let seed = 12345;
  const rand = (n: number) => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % n;
  };
  const words = ['a', 'b', 'c', 'd', '', '}'];
  const text = () => Array.from({ length: rand(25) }, () => words[rand(words.length)]);
  let multi = 0;
  for (let round = 0; round < 600; round++) {
    const a = text();
    // Half the time an edit of `a`, half the time unrelated text.
    const b = round % 2 ? text() : a.filter(() => rand(4) > 0).map((l) => (rand(5) ? l : 'x'));
    const hunks = diffLines(a, b);
    checkHunks(a, b, hunks);
    if (hunks.length > 1) multi++;
    const n = hunks.length;
    const subsets = n <= 8 ? 1 << n : 64;
    for (let s = 0; s < subsets; s++) {
      const mask = n <= 8 ? s : rand(1 << 30);
      const keep = hunks.map((_, i) => ((mask >> i) & 1) === 1);
      assert.deepEqual(
        applyHunks(a, hunks, (_, i) => keep[i]),
        spliced(a, hunks, keep),
        `${a.join(',')} → ${b.join(',')} keeping ${keep}`
      );
    }
  }
  assert.ok(multi > 100, `only ${multi} rounds had several hunks`);
});

test('past the edit-distance limit the change is one hunk, and still exact', () => {
  const a = Array.from({ length: 3000 }, (_, i) => `old ${i}`);
  const b = Array.from({ length: 3000 }, (_, i) => `new ${i}`);
  const hunks = diffLines(['same', ...a, 'end'], ['same', ...b, 'end']);
  assert.equal(hunks.length, 1);
  assert.equal(hunks[0].origStart, 2);
  checkHunks(['same', ...a, 'end'], ['same', ...b, 'end'], hunks);
});

test('line endings and the last line', () => {
  assert.deepEqual(splitLines('a\nb\n'), { lines: ['a', 'b', ''], eol: '\n' });
  assert.deepEqual(splitLines('a\r\nb\r\n'), { lines: ['a', 'b', ''], eol: '\r\n' });
  assert.deepEqual(splitLines('a\nb'), { lines: ['a', 'b'], eol: '\n' });
  assert.deepEqual(splitLines(''), { lines: [''], eol: '\n' });

  // A CRLF file keeps CRLF, and its missing final newline stays missing.
  const base = splitLines('one\r\ntwo\r\nthree');
  const next = splitLines('ONE\r\ntwo\r\nTHREE');
  const hunks = diffLines(base.lines, next.lines);
  assert.equal(hunks.length, 2);
  const firstOnly = applyHunks(base.lines, hunks, (_, i) => i === 0).join(base.eol);
  assert.equal(firstOnly, 'ONE\r\ntwo\r\nthree');

  // Adding a final newline is a change of its own.
  const plain = splitLines('a\nb');
  const ended = splitLines('a\nb\n');
  assert.deepEqual(diffLines(plain.lines, ended.lines), [
    { origStart: 3, deleted: [], added: [''] },
  ]);
});

test('diff rows walk both texts in order', () => {
  for (const [from, to] of [
    ['a,b,c', 'a,x,c'],
    ['', 'a'],
    ['a,b', ''],
    ['a,b,c,d', 'b,d,e'],
  ]) {
    const a = lines(from);
    const b = lines(to);
    const rows = diffRows(a, b);
    const old = rows.filter((r) => r.kind !== 'added').map((r) => a[r.oldIndex!]);
    const now = rows.filter((r) => r.kind !== 'removed').map((r) => b[r.newIndex!]);
    assert.deepEqual(old, a, `old side of ${from} → ${to}`);
    assert.deepEqual(now, b, `new side of ${from} → ${to}`);
  }
});

test('snippet lines', () => {
  assert.deepEqual(snippetLines(''), []);
  assert.deepEqual(snippetLines('a\nb\n'), ['a', 'b']);
  assert.deepEqual(snippetLines('a\r\nb'), ['a', 'b']);
  assert.deepEqual(snippetLines('\n'), ['']);
});
