// integrity.mjs tests (P02 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { parseNameStatus, tamperCalls, tamperReasons, tampering } from './integrity.mjs';

describe('the grader’s integrity check (P02)', () => {
  const flags = [
    {
      path: 'tests/conftest.py',
      flags: ['tests/conftest.py: added, and it decides what the checks run'],
    },
    { path: 'tests/test_more.py', flags: ['tests/test_more.py: 2 tests removed (9 → 7)'] },
  ];

  it('a flag on a file the reference doesn’t change is tampering', () => {
    const tampered = tampering(flags, new Set(['tests/test_more.py', 'more_itertools/more.py']));
    assert.deepEqual(
      tampered.map((f) => f.path),
      ['tests/conftest.py']
    );
    assert.deepEqual(tamperReasons(tampered), [
      'tests/conftest.py: added, and it decides what the checks run',
    ]);
    assert.deepEqual(tampering(flags, new Set(['tests/conftest.py', 'tests/test_more.py'])), []);
  });

  it('git’s name-status list, NUL-separated', () => {
    assert.deepEqual(parseNameStatus('M\0src/a.py\0A\0tests/conftest.py\0D\0old.py\0'), [
      { status: 'M', path: 'src/a.py' },
      { status: 'A', path: 'tests/conftest.py' },
      { status: 'D', path: 'old.py' },
    ]);
    assert.deepEqual(parseNameStatus(''), []);
  });

  it('the dry run’s tampering cheater writes a conftest.py beside a Python task’s tests', () => {
    const calls = tamperCalls({ lang: 'python', hidden: ['tests/test_more.py'] });
    assert.equal(calls.length, 1);
    assert.equal(calls[0].name, 'write');
    assert.equal(calls[0].args.path, 'tests/conftest.py');
    assert.match(calls[0].args.content, /def pytest_runtest_makereport/);
    assert.equal(
      tamperCalls({ lang: 'python', hidden: ['test_x.py'] })[0].args.path,
      'conftest.py'
    );
    assert.equal(tamperCalls({ lang: 'go', hidden: ['a_test.go'] }), null);
  });
});
