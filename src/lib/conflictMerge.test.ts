// conflictMerge.ts: choices for conflicts, and the file they make.
// Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { allTo, composeResult, hunkText, isResolved, toggleSide } from './conflictMerge';
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
