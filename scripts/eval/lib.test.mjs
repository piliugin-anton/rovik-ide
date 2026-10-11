// lib.mjs tests. Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  applyDefaults,
  budgetAllows,
  compareVariant,
  resultsMarkdown,
  subagentsOn,
  variantSettings,
  grade,
  parseCargo,
  parseGoJson,
  parseJunit,
  parseTsLines,
  projection,
  summarize,
  summaryMarkdown,
  validation,
} from './lib.mjs';

describe('test output', () => {
  it('cargo', () => {
    const out = [
      'running 3 tests',
      'test clm::tests::a ... ok',
      'test clm::tests::b ... FAILED',
      'test clm::tests::c ... ignored',
      'test result: FAILED. 1 passed; 1 failed',
    ].join('\n');
    assert.deepEqual(parseCargo(out), {
      'clm::tests::a': 'pass',
      'clm::tests::b': 'fail',
      'clm::tests::c': 'skip',
    });
  });

  it('go test -json, subtests included', () => {
    const out = [
      '{"Action":"run","Test":"TestA"}',
      '{"Action":"pass","Package":"x","Test":"TestA"}',
      '{"Action":"fail","Package":"x","Test":"TestB/sub"}',
      '{"Action":"pass","Package":"x"}',
      'not json',
    ].join('\n');
    assert.deepEqual(parseGoJson(out), { TestA: 'pass', 'TestB/sub': 'fail' });
  });

  it('pytest JUnit XML', () => {
    const xml = `<testsuites><testsuite>
      <testcase classname="tests.test_more.OneTests" name="test_a" time="0.1" />
      <testcase classname="tests.test_more.OneTests" name="test_b"><failure message="x">boom &lt;1&gt;</failure></testcase>
      <testcase classname="tests.test_more.T" name="test_c"><skipped message="no" /></testcase>
      <testcase classname="tests.test_more.T" name="test_&quot;d&quot;"><error message="e" /></testcase>
    </testsuite></testsuites>`;
    assert.deepEqual(parseJunit(xml), {
      'tests.test_more.OneTests::test_a': 'pass',
      'tests.test_more.OneTests::test_b': 'fail',
      'tests.test_more.T::test_c': 'skip',
      'tests.test_more.T::test_"d"': 'fail',
    });
  });

  it('ts-test.mjs lines', () => {
    const out =
      '{"test":"a.test.ts > s > t","status":"pass"}\nnoise\n{"test":"b","status":"fail"}\n';
    assert.deepEqual(parseTsLines(out), { 'a.test.ts > s > t': 'pass', b: 'fail' });
  });
});

describe('grading', () => {
  it('passes only when every required test passes', () => {
    assert.equal(grade(['a', 'b'], { a: 'pass', b: 'pass', c: 'fail' }).success, true);
    const g = grade(['a', 'b', 'd'], { a: 'pass', b: 'fail' });
    assert.equal(g.success, false);
    assert.deepEqual([g.passed, g.failed, g.missing], [1, ['b'], ['d']]);
    // A run whose tests didn't even compile has no results: a failure.
    assert.equal(grade(['a'], {}).success, false);
    assert.equal(grade([], {}).success, false);
  });

  it('validation requires what the reference always passes, and something failing at the base', () => {
    const v = validation({ a: 'pass', b: 'fail' }, [
      { a: 'pass', b: 'pass', c: 'pass', d: 'fail' },
      { a: 'pass', b: 'pass', c: 'fail' },
    ]);
    assert.equal(v.ok, true);
    assert.deepEqual(v.required, ['a', 'b']);
    assert.deepEqual(v.failingAtBase, ['b']);
    assert.deepEqual(v.flaky, ['c']);
    const same = validation({ a: 'pass' }, [{ a: 'pass' }]);
    assert.equal(same.ok, false);
    assert.match(same.problems.join(), /already passes at the base/);
    // A base that doesn't compile fails every test.
    assert.deepEqual(validation({}, [{ a: 'pass' }]).failingAtBase, ['a']);
  });
});

describe('budget', () => {
  it('stops before a run that would pass the cap', () => {
    assert.equal(budgetAllows({ cap: 5, spent: 0, costs: [], firstEstimate: 4.5 }).ok, true);
    assert.equal(budgetAllows({ cap: 5, spent: 0, costs: [], firstEstimate: 6 }).ok, false);
    const later = budgetAllows({ cap: 5, spent: 4, costs: [1, 3], firstEstimate: 9 });
    assert.equal(later.estimate, 2);
    assert.equal(later.ok, false);
    assert.match(later.reason, /\$4\.00 spent/);
    assert.equal(
      budgetAllows({ cap: 75, spent: 10, costs: [0.5, 0.7], firstEstimate: 9 }).ok,
      true
    );
  });

  it('the baseline starts only if the pilot projects it under the cap', () => {
    const fits = projection([0.5, 0.7, 0.6], 90, 75);
    assert.equal(fits.ok, true);
    assert.ok(Math.abs(fits.total - 54) < 1e-9);
    const over = projection([1, 1], 90, 75);
    assert.equal(over.ok, false);
    assert.match(over.reason, /\$90\.00, over the \$75 cap/);
    assert.equal(projection([], 90, 75).ok, false);
  });
});

describe('summary', () => {
  const r = (task, rep, success, costUsd, extra = {}) => ({
    task,
    rep,
    success,
    costUsd,
    tokens: 1000,
    peakLeadContext: 500 * rep,
    ms: 60000,
    interventions: 0,
    outcome: 'done',
    ...extra,
  });
  it('success rates per task and per repetition, cost, peak context', () => {
    const s = summarize([
      r('a', 1, true, 0.1),
      r('b', 1, false, 0.3),
      r('a', 2, true, 0.1),
      r('b', 2, true, 0.1, { interventions: 2 }),
    ]);
    assert.equal(s.runs, 4);
    assert.equal(s.successRate, 0.75);
    assert.deepEqual(
      s.perRep.map((p) => p.successRate),
      [0.5, 1]
    );
    assert.deepEqual(s.successRateRange, [0.5, 1]);
    assert.ok(Math.abs(s.successRateStdev - Math.SQRT1_2 / 2) < 1e-9);
    assert.ok(Math.abs(s.totalCostUsd - 0.6) < 1e-9);
    assert.equal(s.maxPeakLeadContext, 1000);
    assert.equal(s.meanInterventions, 0.5);
    assert.deepEqual(
      s.tasks.map((t) => [t.task, t.successes, t.runs]),
      [
        ['a', 2, 2],
        ['b', 1, 2],
      ]
    );
    const md = summaryMarkdown(s, 'T');
    assert.match(md, /Success rate: 75% \(per repetition: 50%, 100%/);
    assert.match(md, /\| b \| 1\/2 \| \$0\.200 \|/);
  });
});

describe('G02 and G03', () => {
  const run = (task, rep, success, cost, peak) => ({
    task,
    rep,
    success,
    costUsd: cost,
    tokens: 1000,
    peakLeadContext: peak,
    ms: 60000,
    interventions: 0,
    outcome: 'done',
  });
  /** Ten tasks × 3 repetitions; `rate`: how many of each repetition's ten pass. */
  const campaign = (passing, cost, peak) =>
    summarize(
      [1, 2, 3].flatMap((rep) =>
        Array.from({ length: 10 }, (_, i) => run(`t${i}`, rep, i < passing[rep - 1], cost, peak))
      )
    );
  const base = campaign([5, 5, 6], 1, 100_000);

  it('a variant turns one kind on; the baseline has none', () => {
    const s = { profile: { id: 'x' }, config: { permissions: { allowAnyCommand: true } } };
    const v = variantSettings(s, 'scout');
    assert.deepEqual(v.config.subagents, { enabled: true, kinds: { scout: { enabled: true } } });
    assert.equal(v.config.permissions.allowAnyCommand, true);
    assert.equal(s.config.subagents, undefined, 'the original is untouched');
    assert.equal(subagentsOn(v), true);
    assert.equal(subagentsOn(variantSettings(v, null)), false);
    assert.equal(subagentsOn(s), false);
  });

  it('better success within the cost budget: on', () => {
    const d = compareVariant(base, campaign([8, 8, 9], 1.5, 100_000), { costRatioMax: 2 });
    assert.equal(d.on, true, d.reason);
    assert.equal(d.costRatio, 1.5);
  });

  it('a lower peak lead context counts as a gain', () => {
    const d = compareVariant(base, campaign([5, 6, 5], 1.2, 70_000), { costRatioMax: 2 });
    assert.equal(d.on, true);
    assert.match(d.reason, /peak context fell to 70%/);
  });

  it('within the noise, too costly, worse, or too few runs: off', () => {
    assert.match(
      compareVariant(base, campaign([6, 5, 6], 1, 100_000), { costRatioMax: 2 }).reason,
      /no measurable gain/
    );
    assert.match(
      compareVariant(base, campaign([9, 9, 9], 2.5, 100_000), { costRatioMax: 2 }).reason,
      /2\.50× the baseline, over its 2× budget/
    );
    assert.match(
      compareVariant(base, campaign([2, 2, 3], 1, 50_000), { costRatioMax: 2 }).reason,
      /worse/
    );
    assert.match(
      compareVariant(base, campaign([9, 9, 9], 1, 100_000), { costRatioMax: 2, stopped: 'cap' })
        .reason,
      /not enough runs/
    );
  });

  it('the results file has every kind, the unmeasured ones and the decision', () => {
    const d = compareVariant(base, campaign([8, 8, 9], 1.5, 100_000), { costRatioMax: 2 });
    const md = resultsMarkdown({
      date: '2026-10-11',
      model: 'M',
      fake: true,
      costRatioMax: 2,
      tasks: ['t0'],
      baseline: base,
      variants: [{ id: 'scout', milestone: 'A', label: 'Scout', decision: d }],
      unmeasured: [{ id: 'worker', milestone: 'D', label: 'Step worker', why: 'no plans' }],
      on: ['scout'],
    });
    assert.match(md, /Dry run with a scripted model/);
    assert.match(md, /\| Scout \(A\) \| 30 \| 83% \(\+30 pts/);
    assert.match(md, /Step worker \(D\): off by default; no plans\./);
    assert.match(md, /On by default: scout/);
  });

  it('the defaults: the master switch and the chosen kinds on', () => {
    const src = [
      'const DEFAULT_KIND = { enabled: false };',
      'export const DEFAULT_SUBAGENT_SETTINGS: SubagentSettings = {',
      '  enabled: false,',
      '  kinds: {',
      '    scout: { ...DEFAULT_KIND },',
      '    reviewer: { ...DEFAULT_KIND },',
      '  },',
      '};',
      'const enabled = false;',
    ].join('\n');
    const out = applyDefaults(src, ['reviewer']);
    assert.match(out, /\n {2}enabled: true,/);
    assert.match(out, /reviewer: \{ \.\.\.DEFAULT_KIND, enabled: true \},/);
    assert.match(out, /scout: \{ \.\.\.DEFAULT_KIND \},/);
    assert.match(out, /const enabled = false;/, 'only inside the defaults');
    assert.equal(applyDefaults(src, []), src);
    assert.throws(() => applyDefaults(src, ['triage']), /triage line not found/);
  });

  it('applies to the app’s real defaults', () => {
    const real = readFileSync(
      new URL('../../src/lib/subagentSettings.ts', import.meta.url),
      'utf8'
    );
    const out = applyDefaults(real, ['scout', 'triage']);
    assert.notEqual(out, real);
    assert.match(out, /scout: \{ \.\.\.DEFAULT_KIND, enabled: true \}/);
  });
});
