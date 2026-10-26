// branchNames.ts tests (node:test): which branch names the Git panel and
// the new chat's branch offer take, and the names suggested for a new
// branch. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ADJECTIVES, branchNameProblem, PROGRAMMERS, suggestBranchName } from './branchNames';

test('a branch name follows git’s rules, as the backend checks them', () => {
  // The cases of src-tauri/src/git/tests.rs `branch_names_follow_gits_rules`.
  for (const ok of ['feature', 'feature/x', 'v1.2', 'a-b_c', 'ünïcode', '']) {
    assert.equal(branchNameProblem(ok), null, ok);
  }
  for (const bad of [
    'HEAD',
    '@',
    '-x',
    '/x',
    'x/',
    'x.',
    'a..b',
    'a//b',
    'a@{1}',
    'a b',
    'a~b',
    'a^b',
    'a:b',
    'a?b',
    'a*b',
    'a[b',
    'a\\b',
    '.hidden',
    'a/.b',
    'a.lock',
    'a.lock/b',
    'a\x07b',
  ]) {
    assert.equal(branchNameProblem(bad), `'${bad}' isn't a valid branch name.`, bad);
  }
});

test('a name with spaces around it is judged without them', () => {
  assert.equal(branchNameProblem('  feature  '), null);
  assert.equal(branchNameProblem('   '), null, 'empty: nothing to say yet');
});

test('the words of a suggested name are plain lowercase letters, each listed once', () => {
  for (const [list, what] of [
    [ADJECTIVES, 'adjectives'],
    [PROGRAMMERS, 'programmers'],
  ] as const) {
    assert.ok(list.length >= 100, `${what}: enough of them for names to rarely repeat`);
    for (const word of list) assert.match(word, /^[a-z]+$/, `${what}: ${word}`);
    assert.equal(new Set(list).size, list.length, `${what}: no word twice`);
  }
  const both = ADJECTIVES.filter((word) => PROGRAMMERS.includes(word));
  assert.deepEqual(both, [], 'no name is the same word twice');
});

/** A random source that draws these numbers in turn. */
function drawing(...numbers: number[]): () => number {
  return () => numbers.shift()!;
}

/** The draw that picks `word` from `list`. */
const drawOf = (list: readonly string[], word: string) => (list.indexOf(word) + 0.5) / list.length;

test('a suggested name is rovik-, an adjective, a programmer’s last name and 8 random characters', () => {
  // Drawn in that order: the adjective, the name, then each character.
  const name = suggestBranchName(
    drawing(drawOf(ADJECTIVES, 'kind'), drawOf(PROGRAMMERS, 'torvalds'), ...Array(8).fill(0))
  );
  assert.equal(name, 'rovik-kind-torvalds-aaaaaaaa');
  // Any draw Math.random makes picks one, the lowest and the highest too.
  assert.equal(
    suggestBranchName(() => 0),
    `rovik-${ADJECTIVES[0]}-${PROGRAMMERS[0]}-aaaaaaaa`
  );
  assert.equal(
    suggestBranchName(() => 1 - Number.EPSILON),
    `rovik-${ADJECTIVES[ADJECTIVES.length - 1]}-${PROGRAMMERS[PROGRAMMERS.length - 1]}-99999999`
  );
});

test('suggested names are valid branch names, ending in 8 lowercase letters and digits, that don’t repeat', () => {
  const names = Array.from({ length: 500 }, () => suggestBranchName());
  for (const name of names) {
    assert.match(name, /^rovik-[a-z]+-[a-z]+-[a-z0-9]{8}$/, name);
    assert.equal(branchNameProblem(name), null, name);
  }
  // Without the random end, 500 of the pairs alone would almost surely repeat.
  assert.equal(new Set(names).size, names.length, 'no name twice');
});
