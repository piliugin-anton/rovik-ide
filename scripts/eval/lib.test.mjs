// lib.mjs tests. Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  applyDefaults,
  budgetAllows,
  compareVariant,
  resultsMarkdown,
  subagentsOn,
  variantSettings,
  grade,
  knownSpend,
  lockCampaign,
  workspaceName,
  parseCargo,
  parseGoJson,
  parseJunit,
  parseTsLines,
  projection,
  runCampaign,
  stopAfter,
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

describe('unknown costs (P6.6)', () => {
  const run = (task, costUsd) => ({ task, rep: 1, success: true, costUsd, tokens: 0, ms: 0 });
  it('are left out of means, counted, and shown as unknown', () => {
    const s = summarize([run('a', 0.2), run('a', null), run('b', null)]);
    assert.equal(s.meanCostUsd, 0.2);
    assert.equal(s.totalCostUsd, 0.2);
    assert.equal(s.unknownCosts, 2);
    assert.equal(s.tasks.find((t) => t.task === 'b').meanCostUsd, null);
    const md = summaryMarkdown(s, 'x');
    assert.match(md, /\$0\.200 a run \(2 runs not counted: cost unknown\)/);
    assert.match(md, /\| b \| 1\/1 \| unknown \|/);
  });
  it('stop a projection', () => {
    const p = projection([0.5, null], 90, 75);
    assert.equal(p.ok, false);
    assert.match(p.reason, /cost is unknown/);
  });
  it('make a variant’s cost ratio unknown', () => {
    const base = summarize([run('a', 0.2), run('b', 0.2)]);
    const variant = summarize([run('a', null), run('b', 0.1)]);
    const d = compareVariant(base, variant, { costRatioMax: 2, minTasks: 1 });
    assert.equal(d.costRatio, null);
    assert.equal(d.on, false);
  });

  it('one unknown among known costs: the mean and total of the others, "1 run"', () => {
    const s = summarize([run('a', 0.1), run('a', null), run('a', 0.3)]);
    assert.ok(Math.abs(s.meanCostUsd - 0.2) < 1e-9);
    assert.ok(Math.abs(s.totalCostUsd - 0.4) < 1e-9);
    assert.equal(s.unknownCosts, 1);
    assert.ok(Math.abs(s.tasks[0].meanCostUsd - 0.2) < 1e-9);
    assert.match(
      summaryMarkdown(s, 'x'),
      /Cost: \$0\.40 in all, \$0\.200 a run \(1 run not counted: cost unknown\)/
    );
  });

  it('a pilot of unknown costs only is no projection', () => {
    const one = projection([null], 90, 75);
    assert.deepEqual([one.ok, one.perRun, one.total], [false, 0, 0]);
    assert.equal(
      one.reason,
      '1 pilot run’s cost is unknown (no usage reported): the cost can’t be projected'
    );
    assert.match(projection([null, 0.2, null], 90, 75).reason, /^2 pilot runs’ cost is unknown/);
  });

  it('compareVariant: an unknown cost on either side is no ratio, and no gain', () => {
    const known = (cost, n = 5) =>
      summarize(Array.from({ length: n }, (_, i) => run(`t${i}`, cost)));
    const someUnknown = summarize([
      ...Array.from({ length: 4 }, (_, i) => run(`t${i}`, 0.1)),
      run('t4', null),
    ]);
    const allUnknown = known(null);
    for (const [base, variant] of [
      [someUnknown, known(0.1)],
      [known(0.1), someUnknown],
      [known(0.1), allUnknown],
      [allUnknown, allUnknown],
    ]) {
      const d = compareVariant(base, variant, { costRatioMax: 2 });
      assert.equal(d.on, false);
      assert.equal(d.costRatio, null);
      assert.equal(d.reason, 'some runs’ cost is unknown (no usage reported)');
    }
    assert.equal(compareVariant(known(0.1), allUnknown, { costRatioMax: 2 }).meanCostUsd, null);
    // A baseline that cost nothing gives no ratio either.
    assert.equal(
      compareVariant(known(0), known(0.1), { costRatioMax: 2 }).reason,
      'the baseline cost nothing: there’s no cost ratio'
    );
    // Worse success is said first: the cost doesn't matter then.
    const worse = summarize(
      Array.from({ length: 5 }, (_, i) => ({ ...run(`t${i}`, null), success: false }))
    );
    assert.match(compareVariant(known(0.1), worse, { costRatioMax: 2 }).reason, /worse/);
  });

  it('knownSpend adds the known costs and counts the others', () => {
    assert.deepEqual(knownSpend([]), { spent: 0, costs: [], unknown: 0 });
    const k = knownSpend([run('a', 0.5), run('b', null), run('c', 0.25), run('d', undefined)]);
    assert.deepEqual(k, { spent: 0.75, costs: [0.5, 0.25], unknown: 2 });
  });
});

describe('campaign (P6.6)', () => {
  const result = (task, rep, costUsd, extra = {}) => ({
    task,
    rep,
    success: true,
    costUsd,
    tokens: 0,
    outcome: 'done',
    ...extra,
  });
  /** A campaign over `plan` whose runs come from `made` (by "task#rep"); returns what ran. */
  const campaign = async (plan, made, { cap = 10, firstEstimate = 1 } = {}) => {
    const ran = [];
    const out = await runCampaign(plan, {
      cap,
      firstEstimate,
      runOne: async ({ task, rep, done }) => {
        ran.push(`${task}#${rep}`);
        const r = made[`${task}#${rep}`];
        return done ? { ...r, reused: true } : r;
      },
    });
    return { ...out, ran };
  };

  it('a_campaign_resumes_past_a_stored_null_cost_run', async () => {
    const plan = [
      { task: 'a', rep: 1, done: true },
      { task: 'b', rep: 1, done: true },
      { task: 'c', rep: 1, done: false },
      { task: 'd', rep: 1, done: false },
    ];
    const made = {
      'a#1': result('a', 1, 0.5),
      // The first pass stopped here: no usage reported.
      'b#1': result('b', 1, null, { outcome: 'runner-failed' }),
      'c#1': result('c', 1, 0.5),
      'd#1': result('d', 1, 0.5),
    };
    const { results, stopped, ran } = await campaign(plan, made);
    assert.equal(stopped, '');
    assert.deepEqual(ran, ['a#1', 'b#1', 'c#1', 'd#1']);
    assert.deepEqual(knownSpend(results), { spent: 1.5, costs: [0.5, 0.5, 0.5], unknown: 1 });
  });

  it('a new run of unknown cost still stops the campaign', async () => {
    const plan = ['a', 'b', 'c'].map((task) => ({ task, rep: 1, done: false }));
    const made = {
      'a#1': result('a', 1, 0.5),
      'b#1': result('b', 1, null),
      'c#1': result('c', 1, 0.5),
    };
    const { stopped, ran } = await campaign(plan, made);
    assert.equal(stopped, 'b #1: its cost is unknown (no usage reported); stopping');
    assert.deepEqual(ran, ['a#1', 'b#1']);
  });

  it('three errors in a row stop it, unless they were kept from before', async () => {
    const err = (task) => result(task, 1, 0.1, { outcome: 'error' });
    const made = { 'a#1': err('a'), 'b#1': err('b'), 'c#1': err('c'), 'd#1': err('d') };
    const fresh = ['a', 'b', 'c', 'd'].map((task) => ({ task, rep: 1, done: false }));
    const first = await campaign(fresh, made);
    assert.match(first.stopped, /three runs in a row ended in an error/);
    assert.deepEqual(first.ran, ['a#1', 'b#1', 'c#1']);
    // Resumed after the provider was fixed: the kept errors don't stop it.
    const resumed = await campaign(
      fresh.map((p, i) => ({ ...p, done: i < 3 })),
      { ...made, 'd#1': result('d', 1, 0.1) }
    );
    assert.equal(resumed.stopped, '');
    assert.equal(resumed.ran.length, 4);
    // ...but a new error counts with the ones after the resume.
    assert.equal(stopAfter([{ ...err('a'), reused: true }, err('b'), err('c')]), '');
    assert.match(stopAfter([err('b'), err('c'), err('d')]), /three runs/);
  });

  it('the budget counts known costs only, and spares the kept runs', async () => {
    const plan = [
      { task: 'a', rep: 1, done: true },
      { task: 'b', rep: 1, done: true },
      { task: 'c', rep: 1, done: false },
      { task: 'd', rep: 1, done: false },
    ];
    const made = {
      'a#1': result('a', 1, 3),
      'b#1': result('b', 1, null),
      'c#1': result('c', 1, 3),
      'd#1': result('d', 1, 3),
    };
    // $3 spent, the next estimated at $3 (not the $1.50 mean with b as $0).
    const { stopped, ran } = await campaign(plan, made, { cap: 8 });
    assert.deepEqual(ran, ['a#1', 'b#1', 'c#1']);
    assert.match(stopped, /\$6\.00 spent; another run \(~\$3\.00\) would pass the \$8 cap/);
  });
});

describe('two campaigns at once (round 3)', () => {
  it('a workspace is its campaign’s: two campaigns, two folders for one run', () => {
    const a = workspaceName('/results/base', 'mi-subfactorial', 1);
    const b = workspaceName('/results/variant-scout', 'mi-subfactorial', 1);
    assert.notEqual(a, b);
    assert.equal(a, workspaceName('/results/base', 'mi-subfactorial', 1));
    assert.match(a, /^run-[0-9a-f]{8}-mi-subfactorial-1$/);
    assert.notEqual(a, workspaceName('/results/base', 'mi-subfactorial', 2));
  });

  it('a campaign folder is locked while it runs, and free after', () => {
    const dir = mkdtempSync(join(tmpdir(), 'eval-lock-'));
    const unlock = lockCampaign(dir);
    assert.equal(readFileSync(join(dir, '.lock'), 'utf8'), String(process.pid));
    assert.throws(() => lockCampaign(dir), /is being run by process/);
    unlock();
    assert.ok(!existsSync(join(dir, '.lock')));
    lockCampaign(dir)();
  });

  it('a lock a dead process left is taken over', () => {
    const dir = mkdtempSync(join(tmpdir(), 'eval-lock-'));
    // A pid no process has: the largest allowed, long past the range in use.
    writeFileSync(join(dir, '.lock'), '4194304');
    const unlock = lockCampaign(dir);
    assert.equal(readFileSync(join(dir, '.lock'), 'utf8'), String(process.pid));
    unlock();
  });
});
