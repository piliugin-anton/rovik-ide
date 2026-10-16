import { test } from 'node:test';
import assert from 'node:assert/strict';
import { recallViewState, rememberViewState } from './viewStates';

test('a file’s place is kept when its editor goes, and given back', () => {
  const state = {
    cursorState: [{ position: { lineNumber: 150, column: 1 } }],
    viewState: { scrollTop: 2470 },
  };
  rememberViewState('/p/long.ts', state);
  assert.deepEqual(recallViewState('/p/long.ts'), state);
  assert.equal(recallViewState('/p/other.ts'), undefined);
  // Left again elsewhere: the newer place wins.
  rememberViewState('/p/long.ts', { viewState: { scrollTop: 0 } });
  assert.deepEqual(recallViewState('/p/long.ts'), { viewState: { scrollTop: 0 } });
  // Nothing to keep (an editor without a model): forgotten.
  rememberViewState('/p/long.ts', null);
  assert.equal(recallViewState('/p/long.ts'), undefined);
});

test('only the most recently left files are kept', () => {
  for (let i = 0; i < 250; i++) rememberViewState(`/p/f${i}.ts`, { i });
  assert.equal(recallViewState('/p/f0.ts'), undefined, 'the oldest went');
  assert.deepEqual(recallViewState('/p/f249.ts'), { i: 249 });
  // Leaving an old one again makes it recent.
  rememberViewState('/p/f60.ts', { i: 60 });
  for (let i = 250; i < 390; i++) rememberViewState(`/p/f${i}.ts`, { i });
  assert.deepEqual(recallViewState('/p/f60.ts'), { i: 60 });
  assert.equal(recallViewState('/p/f61.ts'), undefined);
});
