import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  checksThatRun,
  effectiveChecks,
  projectChecksSummary,
  scriptKind,
  type ProjectCheck,
  type ProjectProfile,
} from './projectProfile';

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

test('detected checks are each off until the user turns one on; the user’s list wins', () => {
  const profile: ProjectProfile = {
    stack: ['TypeScript'],
    checks: [
      { kind: 'typecheck', command: 'npx tsc --noEmit', source: 'tsconfig.json' },
      { kind: 'test', command: 'npm test', source: 'package.json scripts.test' },
    ],
    instructions: null,
  };
  const detected = effectiveChecks(profile, {});
  assert.deepEqual(detected, [
    { kind: 'typecheck', command: 'npx tsc --noEmit', enabled: false, source: 'tsconfig.json' },
    { kind: 'test', command: 'npm test', enabled: false, source: 'package.json scripts.test' },
  ]);
  assert.deepEqual(checksThatRun(detected), [], 'nothing runs on detection alone');

  const saved: ProjectCheck[] = [
    { kind: 'test', command: 'npm test', enabled: true },
    { kind: 'lint', command: 'npm run lint', enabled: false },
    // On, but with no command: nothing to run.
    { kind: 'build', command: '  ', enabled: true },
  ];
  assert.equal(effectiveChecks(profile, { checks: saved }), saved);
  assert.deepEqual(checksThatRun(saved), [saved[0]]);
  assert.deepEqual(effectiveChecks(null, {}), []);
});

test('the Settings menu’s line for Project checks says how many run', () => {
  assert.equal(projectChecksSummary(false, 2), 'Not run: checking is off in Agent runs');
  assert.equal(projectChecksSummary(true, 0), 'Nothing to run: no check is turned on');
  assert.equal(projectChecksSummary(true, 1), '1 check runs when the agent finishes a change');
  assert.equal(projectChecksSummary(true, 3), '3 checks run when the agent finishes a change');
  // Not read (yet, or it couldn't be): when they'd run.
  assert.equal(projectChecksSummary(true, undefined), 'Run when the agent finishes a change');
  assert.equal(projectChecksSummary(true, null), 'Run when the agent finishes a change');
});
