import assert from 'node:assert/strict';
import { test } from 'node:test';
import { formatHarnessRate, parseHarnessRate, savedHarnessRates } from './harnessRates';

test('saved verification prices distinguish unknown, invalid and explicitly free models', () => {
  assert.equal(savedHarnessRates(undefined), null);
  assert.equal(savedHarnessRates({}), null);
  assert.equal(savedHarnessRates({ harnessRates: { input: 0, output: 0 } }), null);
  for (const value of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, '0', null]) {
    assert.equal(
      savedHarnessRates({ harnessRates: { input: 0, cachedInput: value as number, output: 0 } }),
      null
    );
  }
  const rates = { input: 0, cachedInput: 0, output: 0 };
  const saved = savedHarnessRates({ harnessRates: rates });
  assert.deepEqual(saved, rates);
  assert.notEqual(saved, rates, 'a run receives its own copy of the configured prices');
});

test('currency prices convert to exact microunits without rounding or treating unknown as free', () => {
  for (const [raw, expected] of [
    ['0', 0],
    ['0.000000', 0],
    [' 2.50 ', 2_500_000],
    ['.5', 500_000],
    ['1.', 1_000_000],
    ['0.000001', 1],
    ['1.000001', 1_000_001],
    ['1.005', 1_005_000],
    ['9007199254.740991', Number.MAX_SAFE_INTEGER],
  ] as const)
    assert.equal(parseHarnessRate(raw), expected, raw);
  for (const raw of [
    '',
    ' ',
    '-1',
    '0.0000001',
    '2.5000000',
    '1e6',
    '1,50',
    'free',
    '9007199254.740992',
  ])
    assert.equal(parseHarnessRate(raw), undefined, raw);
});

test('saved microunit prices display and round-trip exactly including legacy and free prices', () => {
  for (const [saved, displayed] of [
    [0, '0'],
    [1, '0.000001'],
    [500_000, '0.5'],
    [2_500_000, '2.5'],
    [1_000_001, '1.000001'],
    [Number.MAX_SAFE_INTEGER, '9007199254.740991'],
  ] as const) {
    assert.equal(formatHarnessRate(saved), displayed);
    assert.equal(parseHarnessRate(formatHarnessRate(saved)), saved);
  }
  for (const value of [undefined, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1])
    assert.equal(formatHarnessRate(value), '');
});
