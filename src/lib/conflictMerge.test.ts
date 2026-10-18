// conflictMerge.ts: choices for conflicts, and the file they make.
// Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  allTo,
  composeResult,
  differingParts,
  hunkText,
  isPicked,
  isResolved,
  likeness,
  rowsOf,
  toggleLine,
  toggleSide,
  type HunkChoice,
  type Side,
} from './conflictMerge';
import type { GitMergeSegment } from '../types';

const segments: GitMergeSegment[] = [
  { kind: 'common', text: 'a\n' },
  { kind: 'conflict', ours: 'O\n', base: 'B\n', theirs: 'T\n' },
  { kind: 'common', text: 'm\n' },
  { kind: 'conflict', ours: 'o2\n', base: '', theirs: 't2\n' },
  { kind: 'common', text: 'z\n' },
];
const file = { segments, ours: 'whole', theirs: 'whole' };

describe('conflict choices', () => {
  it('unresolved conflicts add nothing and are counted', () => {
    assert.deepEqual(composeResult(file, []), { text: 'a\nm\nz\n', unresolved: 2, deleted: false });
  });

  it('takes sides in the order they were taken', () => {
    const first = toggleSide(undefined, 'theirs');
    const both = toggleSide(first, 'ours');
    assert.deepEqual(both.sides, ['theirs', 'ours']);
    const r = composeResult(file, [both, { sides: ['ours'] }]);
    assert.equal(r.text, 'a\nT\nO\nm\no2\nz\n');
    assert.equal(r.unresolved, 0);
  });

  it('a choice can be changed again: dropping a side, or every side', () => {
    let c = toggleSide(undefined, 'ours');
    c = toggleSide(c, 'theirs');
    c = toggleSide(c, 'ours');
    assert.deepEqual(c.sides, ['theirs']);
    c = toggleSide(c, 'theirs');
    assert.equal(isResolved(c), false);
    assert.equal(composeResult(file, [c]).unresolved, 2);
  });

  it('an edit by hand wins, and taking a side again replaces it', () => {
    const edited = { sides: ['ours' as const], custom: 'hand\n' };
    assert.equal(composeResult(file, [edited, { sides: ['theirs'] }]).text, 'a\nhand\nm\nt2\nz\n');
    // An edit to nothing still counts as resolved (the lines are dropped).
    assert.equal(isResolved({ sides: [], custom: '' }), true);
    assert.deepEqual(toggleSide(edited, 'theirs'), { sides: ['theirs'] });
  });

  it('keeps lines apart when a side has no final newline', () => {
    const conflict = { kind: 'conflict' as const, ours: 'x', base: '', theirs: 'y' };
    assert.equal(hunkText(conflict, { sides: ['ours', 'theirs'] }), 'x\ny');
  });

  it('accept all', () => {
    assert.equal(composeResult(file, allTo(segments, 'theirs')).text, 'a\nT\nm\nt2\nz\n');
  });

  it('taking only the side that deleted the file deletes it', () => {
    const del = {
      segments: [{ kind: 'conflict' as const, ours: 'kept\n', base: 'old\n', theirs: '' }],
      ours: 'kept\n',
      theirs: null,
    };
    assert.equal(composeResult(del, [{ sides: ['theirs'] }]).deleted, true);
    assert.equal(composeResult(del, [{ sides: ['ours'] }]).deleted, false);
    assert.equal(composeResult(del, [{ sides: ['theirs'], custom: 'x' }]).deleted, false);
    assert.equal(composeResult(del, []).deleted, false);
  });
});

const conflict = (ours: string, theirs: string) => ({
  kind: 'conflict' as const,
  ours,
  base: '',
  theirs,
});

/** Picks lines one after another: [side, index] each. */
function pick(
  c: ReturnType<typeof conflict>,
  start: HunkChoice | undefined,
  ...picks: [Side, number][]
) {
  return picks.reduce<HunkChoice | undefined>(
    (choice, [side, i]) => toggleLine(c, choice, side, i),
    start
  );
}

describe('picking lines', () => {
  // Yours changed lines 2 and 4, incoming line 3: git can't split them.
  const interleaved = conflict(
    '  let sum = 0; // running total\n  for (const item of items) sum += item.price;\n  return Math.round(sum);\n',
    '  let sum = 0;\n  for (const item of items) sum += item.price * item.qty;\n  return sum;\n'
  );

  it('lines up the versions of each line in a row', () => {
    assert.deepEqual(
      rowsOf(interleaved).map((r) => [r.ours, r.theirs, r.same]),
      [
        [0, 0, false],
        [1, 1, false],
        [2, 2, false],
      ]
    );
  });

  it('takes each line from the side that changed it: three picks', () => {
    const c = pick(interleaved, undefined, ['ours', 0], ['theirs', 1], ['ours', 2]);
    assert.equal(
      hunkText(interleaved, c),
      '  let sum = 0; // running total\n  for (const item of items) sum += item.price * item.qty;\n  return Math.round(sum);\n'
    );
    assert.equal(isResolved(c), true);
    assert.equal(isPicked(c, 'theirs', 1), true);
    assert.equal(isPicked(c, 'theirs', 0), false);
  });

  it('starts from a side taken whole: its lines stay, in its order', () => {
    // Yours whole, then incoming's line 3 for yours'.
    const c = pick(interleaved, { sides: ['ours'] }, ['ours', 1], ['theirs', 1]);
    assert.equal(
      hunkText(interleaved, c),
      '  let sum = 0; // running total\n  for (const item of items) sum += item.price * item.qty;\n  return Math.round(sum);\n'
    );
    assert.deepEqual(c?.sides, []);
  });

  it('all of one side and none of the other is that side taken whole', () => {
    let c = pick(interleaved, undefined, ['theirs', 2], ['theirs', 0], ['theirs', 1]);
    assert.deepEqual(c, { sides: ['theirs'] });
    c = pick(interleaved, c, ['theirs', 1]);
    assert.ok(c?.lines);
    c = pick(interleaved, c, ['theirs', 0], ['theirs', 2]);
    assert.deepEqual(c, { sides: [] });
    assert.equal(isResolved(c), false);
  });

  it('dropping a line from both sides taken keeps their order', () => {
    // Both added imports at the same place: the result keeps yours first.
    const added = conflict(
      'import { c } from "./c";\nimport { e } from "./e";\n',
      'import { d } from "./d";\n'
    );
    const c = pick(added, { sides: ['ours', 'theirs'] }, ['ours', 1]);
    assert.equal(hunkText(added, c), 'import { c } from "./c";\nimport { d } from "./d";\n');
    assert.deepEqual(pick(added, c, ['ours', 1]), { sides: ['ours', 'theirs'] });
    // Taken incoming first: incoming's lines stay first.
    const d = pick(added, { sides: ['theirs', 'ours'] }, ['ours', 0]);
    assert.equal(hunkText(added, d), 'import { d } from "./d";\nimport { e } from "./e";\n');
  });

  it('a line both sides have is one row, taken once', () => {
    const c = conflict('a1\n}\na2\n', 'b1\n}\nb2\n');
    assert.deepEqual(
      rowsOf(c).map((r) => [r.ours, r.theirs, r.same]),
      [
        [0, null, false],
        [null, 0, false],
        [1, 1, true],
        [2, null, false],
        [null, 2, false],
      ]
    );
    const picked = pick(c, undefined, ['ours', 0], ['ours', 1], ['theirs', 1], ['theirs', 2]);
    assert.equal(hunkText(c, picked), 'a1\n}\nb2\n');
  });

  it("lines that don't look alike get rows of their own, yours first", () => {
    const c = conflict('function a() {\n  return 1;\n', 'const x = [\n  "y",\n];\n');
    const rows = rowsOf(c);
    assert.deepEqual(
      rows.map((r) => [r.ours, r.theirs]),
      [
        [0, null],
        [1, null],
        [null, 0],
        [null, 1],
        [null, 2],
      ]
    );
  });

  it('a re-indented line is the same line', () => {
    assert.equal(likeness('    prepare(job)', '        prepare(job)'), 1);
    const c = conflict(
      '    if job.enabled:\n        prepare(job)\n        execute(job)\n',
      '    prepare(job)\n    execute(job, retries=3)\n'
    );
    assert.deepEqual(
      rowsOf(c).map((r) => [r.ours, r.theirs]),
      [
        [0, null],
        [1, 0],
        [2, 1],
      ]
    );
  });

  it('lines are alike for their words, not their brackets', () => {
    // Two different calls stay apart; the two returns are one line, changed.
    const c = conflict(
      '  logger.debug("loaded", path);\n  return Object.freeze(data);\n',
      '  metrics.count("load");\n  return data ?? {};\n'
    );
    assert.deepEqual(
      rowsOf(c).map((r) => [r.ours, r.theirs]),
      [
        [0, null],
        [null, 0],
        [1, 1],
      ]
    );
    assert.ok(likeness('}', '};') >= 0.5);
    assert.equal(likeness('}', 'return x;'), 0);
  });

  it('marks where two versions of a line differ, whole words', () => {
    const a = 'const c = 30;';
    const b = 'const c = 3;';
    const parts = differingParts(a, b);
    assert.equal(a.slice(...parts.ours), '30');
    assert.equal(b.slice(...parts.theirs), '3');
    const added = differingParts('let sum = 0; // running total', 'let sum = 0;');
    assert.equal('let sum = 0; // running total'.slice(...added.ours), ' // running total');
    assert.deepEqual(added.theirs, [12, 12]);
  });

  it('keeps line endings, and a last line without one', () => {
    const c = conflict('a\r\nb\r\n', 'x\r\ny');
    assert.equal(hunkText(c, pick(c, undefined, ['ours', 0], ['theirs', 1])), 'a\r\ny');
    assert.equal(hunkText(c, pick(c, undefined, ['theirs', 1], ['ours', 1])), 'b\r\ny');
  });

  it('an edit by hand gives way to a picked line, and a side taken to the picks', () => {
    const c = pick(interleaved, { sides: ['ours'], custom: 'hand\n' }, ['theirs', 0]);
    assert.equal(hunkText(interleaved, c), '  let sum = 0;\n');
    assert.deepEqual(toggleSide(c, 'ours'), { sides: ['ours'] });
  });

  it('picked lines of a deleted file keep it', () => {
    const del = {
      segments: [conflict('', 'old 2\nold 3\n')],
      ours: null,
      theirs: 'old 2\nold 3\n',
    };
    const c = pick(del.segments[0], undefined, ['theirs', 1]);
    assert.deepEqual(composeResult(del, [c]), { text: 'old 3\n', unresolved: 0, deleted: false });
  });
});
