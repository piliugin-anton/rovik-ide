import { test } from 'node:test';
import assert from 'node:assert/strict';
import { scriptKind } from './projectProfile';

test('a script’s check kind, as the backend reads script names', () => {
  assert.equal(scriptKind('tsc'), 'typecheck');
  assert.equal(scriptKind('check-types'), 'typecheck');
  assert.equal(scriptKind('lint'), 'lint');
  // Ignite's React Native apps: `lint` fixes, `lint:check` only checks.
  assert.equal(scriptKind('lint:check'), 'lint');
  assert.equal(scriptKind('build'), 'build');
  assert.equal(scriptKind('test'), 'test');
  assert.equal(scriptKind('e2e'), 'test');
});
