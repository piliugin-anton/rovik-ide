// The evaluation's pure parts: reading test runners' output, grading,
// budget decisions and the summary. eval.mjs does the I/O (but for a
// campaign's lock file, which lives here with its tests).
import { createHash } from 'node:crypto';
import { closeSync, mkdirSync, openSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  designEffect,
  detectableEffect,
  meanRunVariance,
  minimumDetectableEffect,
  pairedComparison,
} from './stats.mjs';

/** Results of `cargo test` (libtest's plain output): name -> pass|fail|skip. */
export function parseCargo(text) {
  const results = {};
  for (const m of text.matchAll(/^test (\S+) \.\.\. (ok|FAILED|ignored)/gm)) {
    results[m[1]] = m[2] === 'ok' ? 'pass' : m[2] === 'FAILED' ? 'fail' : 'skip';
  }
  return results;
}

/** The lines of `text` that start with `prefix` and are JSON (the rest is other output). */
function jsonLines(text, prefix = '{') {
  const out = [];
  for (const line of text.split('\n')) {
    if (!line.startsWith(prefix)) continue;
    try {
      out.push(JSON.parse(line));
    } catch {
      // not ours
    }
  }
  return out;
}

/** Results of `go test -json`: "Package/Test" -> pass|fail|skip. */
export function parseGoJson(text) {
  const results = {};
  for (const e of jsonLines(text)) {
    if (!e.Test || !['pass', 'fail', 'skip'].includes(e.Action)) continue;
    results[e.Test] = e.Action;
  }
  return results;
}

const unescapeXml = (s) =>
  s
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, '&');

/** Results of a pytest JUnit XML report: "class::name" -> pass|fail|skip. */
export function parseJunit(xml) {
  const results = {};
  const cases = xml.matchAll(/<testcase\b([^>]*?)(\/>|>([\s\S]*?)<\/testcase>)/g);
  for (const m of cases) {
    const attr = (name) => unescapeXml(m[1].match(new RegExp(`\\b${name}="([^"]*)"`))?.[1] ?? '');
    const body = m[3] ?? '';
    const name = `${attr('classname')}::${attr('name')}`;
    results[name] = /<(failure|error)\b/.test(body)
      ? 'fail'
      : /<skipped\b/.test(body)
        ? 'skip'
        : 'pass';
  }
  return results;
}

/** Results of ts-test.mjs (its reporter's JSON lines): "file > suite > test" -> pass|fail|skip. */
export function parseTsLines(text) {
  const results = {};
  for (const { test, status } of jsonLines(text, '{"test":')) results[test] = status;
  return results;
}

/**
 * A run's grade: every test the reference passes (`required`) must pass.
 * `results` are the hidden tests' results on the run's code.
 */
export function grade(required, results) {
  const failed = required.filter((t) => results[t] === 'fail');
  const missing = required.filter((t) => results[t] !== 'fail' && results[t] !== 'pass');
  return {
    success: required.length > 0 && failed.length === 0 && missing.length === 0,
    required: required.length,
    passed: required.length - failed.length - missing.length,
    failed,
    missing,
  };
}

const passing = (results) =>
  Object.keys(results)
    .filter((t) => results[t] === 'pass')
    .sort();

/**
 * What a task's validation found: the tests to require are those the
 * reference passes every time it is run (a test passing only sometimes
 * would grade by chance); the task is usable when some of them fail at
 * the base.
 */
export function validation(baseResults, referenceRuns) {
  const [first, ...rest] = referenceRuns;
  const required = passing(first).filter((t) => rest.every((r) => r[t] === 'pass'));
  const flaky = passing(first).filter((t) => !required.includes(t));
  const failingAtBase = required.filter((t) => baseResults[t] !== 'pass');
  const problems = [];
  if (required.length === 0) problems.push('no test passes at the reference');
  if (failingAtBase.length === 0) problems.push('every required test already passes at the base');
  return { ok: problems.length === 0, problems, required, failingAtBase, flaky };
}

const sum = (xs) => xs.reduce((a, b) => a + b, 0);

/**
 * Whether the next run may start. `spent` is what the campaign's runs
 * cost so far; the next run is assumed to cost their mean (or, before
 * any run, `firstEstimate`), and so is each of the `pending` runs under
 * way (P05: runs side by side).
 */
export function budgetAllows({ cap, spent, costs, firstEstimate, pending = 0 }) {
  const estimate = costs.length ? sum(costs) / costs.length : firstEstimate;
  const ok = spent + estimate * (pending + 1) <= cap;
  const underWay = pending ? ` and ${pending} run${pending === 1 ? '' : 's'} under way` : '';
  return {
    ok,
    estimate,
    reason: ok
      ? ''
      : `$${spent.toFixed(2)} spent${underWay}; another run (~$${estimate.toFixed(2)}) would pass the $${cap} cap`,
  };
}

/** The baseline's cost, projected from the pilot's mean cost per run. */
export function projection(pilotCosts, runs, cap) {
  if (!pilotCosts.length) return { ok: false, perRun: 0, total: 0, reason: 'no pilot results' };
  // P6.6: an unknown cost isn't free: no projection from it.
  const unknown = pilotCosts.filter((c) => typeof c !== 'number').length;
  if (unknown)
    return {
      ok: false,
      perRun: 0,
      total: 0,
      reason: `${unknown} pilot run${unknown === 1 ? '’s' : 's’'} cost is unknown (no usage reported): the cost can’t be projected`,
    };
  const perRun = sum(pilotCosts) / pilotCosts.length;
  const total = perRun * runs;
  return {
    ok: total <= cap,
    perRun,
    total,
    reason:
      total <= cap
        ? ''
        : `the pilot's $${perRun.toFixed(3)} a run × ${runs} runs = $${total.toFixed(2)}, over the $${cap} cap`,
  };
}

const mean = (xs) => (xs.length ? sum(xs) / xs.length : 0);
const stdev = (xs) => {
  if (xs.length < 2) return 0;
  const m = mean(xs);
  return Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / (xs.length - 1));
};

/**
 * P6.6: the runs whose cost is known (a provider that reports no usage
 * leaves it null): means are over these, and how many weren't is kept.
 */
const knownCosts = (runs) => runs.map((r) => r.costUsd).filter((c) => typeof c === 'number');
const meanCost = (runs) => {
  const known = knownCosts(runs);
  return known.length ? mean(known) : null;
};
const unknownCosts = (runs) => runs.length - knownCosts(runs).length;
/** "$0.123", or "unknown" with no known cost. */
const money = (x, digits) => (typeof x === 'number' ? `$${x.toFixed(digits)}` : 'unknown');

/**
 * P6.6: what a campaign's runs so far cost, from the runs whose cost is
 * known: the total, each one (the next run's estimate is their mean), and
 * how many runs weren't counted. An unknown cost isn't taken as $0.
 */
export function knownSpend(results) {
  const costs = knownCosts(results);
  return { spent: sum(costs), costs, unknown: results.length - costs.length };
}

const ERRORS = ['error', 'runner-failed'];

/**
 * Why a campaign stops after its latest run ('' to go on). A run kept from
 * an earlier pass (`reused`) was judged in that pass: a resumed campaign
 * goes past it, or it could never be resumed past a run whose cost is
 * unknown (P6.6) or past three errors in a row.
 */
export function stopAfter(results) {
  const last = results.at(-1);
  if (!last || last.reused) return '';
  const fresh = results.filter((r) => !r.reused).slice(-3);
  if (fresh.length === 3 && fresh.every((r) => ERRORS.includes(r.outcome)))
    return 'three runs in a row ended in an error: is the provider (or the runner) working?';
  if (typeof last.costUsd !== 'number')
    return `${last.task} #${last.rep}: its cost is unknown (no usage reported); stopping`;
  return '';
}

/**
 * The workspace folder name of one run: the campaign's, so two campaigns
 * running at once (G02's variants in two terminals) don't check out, and
 * grade, the same folder (round 3).
 */
export function workspaceName(campaignDir, taskId, rep) {
  const owner = createHash('sha1').update(campaignDir).digest('hex').slice(0, 8);
  return `run-${owner}-${taskId}-${rep}`;
}

/**
 * Takes a campaign folder for this process: `.lock` in it names the pid.
 * A lock whose process is gone is taken over; a live one is an error, so
 * a campaign can't be resumed twice at once. Returns the release.
 */
export function lockCampaign(campaignDir) {
  mkdirSync(campaignDir, { recursive: true });
  const file = join(campaignDir, '.lock');
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = openSync(file, 'wx');
      writeFileSync(fd, String(process.pid));
      closeSync(fd);
      return () => {
        try {
          if (readFileSync(file, 'utf8') === String(process.pid)) unlinkSync(file);
        } catch {
          // gone already
        }
      };
    } catch (e) {
      if (e.code !== 'EEXIST') throw e;
      const pid = Number(readFileSync(file, 'utf8'));
      if (pid && (pid === process.pid || alive(pid)))
        throw new Error(`${campaignDir} is being run by process ${pid} (${file})`);
      try {
        unlinkSync(file);
      } catch {
        // taken by another process this instant: the next attempt sees it
      }
    }
  }
  throw new Error(`${campaignDir} is being run by another process (${file})`);
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === 'EPERM';
  }
}

/**
 * P05 (evolve-plan): `run(item, slot)` for each of `items`, at most `jobs`
 * at once, started in their order. `slot` (0 to jobs − 1) is the
 * worker's, for what one run at a time may use (a build folder).
 * `before(item, results, running)` can refuse to start an item and
 * `after(results)` stop after a run: their reason ('' to go on) stops
 * what hasn't started, while the runs under way finish. So does an
 * error, thrown once they have. Returns the results, in the order they
 * finished, and why it stopped ('' if it didn't).
 */
export async function inParallel(items, jobs, run, { before = () => '', after = () => '' } = {}) {
  const results = [];
  let stopped = '';
  let failure = null;
  let next = 0;
  let running = 0;
  const worker = async (slot) => {
    while (!stopped && !failure && next < items.length) {
      const item = items[next];
      const refused = before(item, results, running);
      if (refused) {
        stopped = refused;
        return;
      }
      next++;
      running++;
      try {
        results.push(await run(item, slot));
      } catch (e) {
        failure ??= e;
        return;
      } finally {
        running--;
      }
      stopped ||= after(results);
    }
  };
  const workers = Math.max(1, Math.min(jobs, items.length));
  await Promise.all(Array.from({ length: workers }, (_, slot) => worker(slot)));
  if (failure) throw failure;
  return { results, stopped };
}

/**
 * A campaign's runs, in order, `jobs` at once (P05). `plan` lists them,
 * each with `done` when its result is kept from an earlier pass;
 * `runOne(item, slot)` makes the run (or reads its kept result back). A
 * new run starts only if the budget allows it (`budgetAllows` over
 * `knownSpend`, the runs under way counted at the estimate), and the
 * campaign stops where `stopAfter` says. Returns the results and why it
 * stopped ('' if it didn't).
 */
export function runCampaign(plan, { cap, firstEstimate, runOne, jobs = 1 }) {
  return inParallel(plan, jobs, runOne, {
    before: (item, results, running) => {
      if (item.done) return '';
      const { spent, costs } = knownSpend(results);
      const allowed = budgetAllows({ cap, spent, costs, firstEstimate, pending: running });
      return allowed.ok ? '' : allowed.reason;
    },
    after: stopAfter,
  });
}

/**
 * The chance that at least one of `k` runs passes, from a task's `n` runs
 * of which `c` passed (pass@k; the unbiased estimate, 1 − C(n−c, k)/C(n, k)).
 */
export function passAt(n, c, k) {
  if (k < 1 || k > n) throw new Error(`pass@${k} needs at least ${k} runs (${n})`);
  return 1 - drawOdds(n - c, n, k);
}

/**
 * The chance that all of `k` runs pass (pass^k, C(c, k)/C(n, k)): how
 * reliably the agent does it, where pass@k says whether it can.
 */
export function passHat(n, c, k) {
  if (k < 1 || k > n) throw new Error(`pass^${k} needs at least ${k} runs (${n})`);
  return drawOdds(c, n, k);
}

/** C(a, k)/C(n, k): the chance that `k` of `n` drawn without replacement all come from `a`. */
function drawOdds(a, n, k) {
  let odds = 1;
  for (let i = 0; i < k; i++) odds *= Math.max(0, a - i) / (n - i);
  return odds;
}

/**
 * pass@k or pass^k (`f`) for each k from 1 to the most runs a task has,
 * over the tasks with at least k runs: [{ k, rate, tasks }].
 */
function passRates(tasks, f) {
  const most = Math.max(0, ...tasks.map((t) => t.runs));
  const rates = [];
  for (let k = 1; k <= most; k++) {
    const over = tasks.filter((t) => t.runs >= k);
    rates.push({ k, rate: mean(over.map((t) => f(t.runs, t.successes, k))), tasks: over.length });
  }
  return rates;
}

/** A number of a run's result, 0 when it has none (a result written before it was). */
const meanOf = (runs, key) => mean(runs.map((r) => r[key] ?? 0));

/** G02: the runs' failed tool calls by class, added up. */
function classTotals(runs) {
  const totals = {};
  for (const r of runs)
    for (const [cls, n] of Object.entries(r.toolErrorClasses ?? {}))
      totals[cls] = (totals[cls] ?? 0) + n;
  return totals;
}

const STAGE_ENDS = ['done', 'failed', 'skipped'];

/**
 * G02: an Autopilot campaign's stages, in the order the runs met them:
 * how many runs reached each, how each ended there (its last round's
 * status) and its rounds a run. Empty for runs that have no stages.
 */
export function stageTotals(runs) {
  const by = new Map();
  for (const r of runs) {
    if (!Array.isArray(r.stages)) continue;
    const last = new Map();
    for (const s of r.stages) {
      const seen = last.get(s.name);
      last.set(s.name, {
        status: s.status,
        rounds: Math.max(seen?.rounds ?? 0, s.round ?? 1),
      });
    }
    for (const [name, { status, rounds }] of last) {
      if (!by.has(name))
        by.set(name, { name, runs: 0, done: 0, failed: 0, skipped: 0, other: 0, rounds: 0 });
      const t = by.get(name);
      t.runs++;
      t.rounds += rounds;
      t[STAGE_ENDS.includes(status) ? status : 'other']++;
    }
  }
  return [...by.values()].map(({ rounds, ...t }) => ({ ...t, meanRounds: rounds / t.runs }));
}

/**
 * The campaign's summary from its runs' results
 * ({task, rep, success, costUsd, tokens, peakLeadContext, ms, interventions, outcome},
 * and G02's toolErrors, toolErrorClasses, editMisses, verifyRounds, stages).
 * `clusterOf`: a task's module (P04), for the power's design effect (P07).
 */
export function summarize(results, { clusterOf = (task) => task } = {}) {
  const byTask = new Map();
  for (const r of results) {
    if (!byTask.has(r.task)) byTask.set(r.task, []);
    byTask.get(r.task).push(r);
  }
  const tasks = [...byTask.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([task, runs]) => ({
      task,
      runs: runs.length,
      successes: runs.filter((r) => r.success).length,
      successRate: mean(runs.map((r) => (r.success ? 1 : 0))),
      meanCostUsd: meanCost(runs),
      unknownCosts: unknownCosts(runs),
      meanTokens: mean(runs.map((r) => r.tokens ?? 0)),
      maxPeakLeadContext: Math.max(0, ...runs.map((r) => r.peakLeadContext ?? 0)),
      meanMinutes: mean(runs.map((r) => (r.ms ?? 0) / 60000)),
      meanInterventions: mean(runs.map((r) => r.interventions ?? 0)),
      meanToolErrors: meanOf(runs, 'toolErrors'),
      meanEditMisses: meanOf(runs, 'editMisses'),
      meanVerifyRounds: meanOf(runs, 'verifyRounds'),
      outcomes: runs.map((r) => r.outcome),
    }));
  // Each repetition's success rate over the tasks it ran: their spread
  // says how much one campaign's number can be trusted.
  const reps = [...new Set(results.map((r) => r.rep))].sort((a, b) => a - b);
  const perRep = reps.map((rep) => {
    const runs = results.filter((r) => r.rep === rep);
    return { rep, runs: runs.length, successRate: mean(runs.map((r) => (r.success ? 1 : 0))) };
  });
  const rates = perRep.map((r) => r.successRate);
  return {
    runs: results.length,
    successRate: mean(results.map((r) => (r.success ? 1 : 0))),
    // P02 (evolve-plan): runs whose change decided how the tests ran, and
    // others the grader or the monitor found tampered (P08, P09); P09:
    // runs whose transcript a person should read.
    tampered: results.filter((r) => r.tampered?.length).length,
    review: results.filter((r) => r.review?.length).length,
    perRep,
    successRateStdev: stdev(rates),
    successRateRange: rates.length ? [Math.min(...rates), Math.max(...rates)] : [0, 0],
    totalCostUsd: sum(knownCosts(results)),
    meanCostUsd: meanCost(results),
    unknownCosts: unknownCosts(results),
    meanTokens: mean(results.map((r) => r.tokens ?? 0)),
    maxPeakLeadContext: Math.max(0, ...results.map((r) => r.peakLeadContext ?? 0)),
    meanMinutes: mean(results.map((r) => (r.ms ?? 0) / 60000)),
    meanInterventions: mean(results.map((r) => r.interventions ?? 0)),
    // G02: how the runs used their tools, and their checks' rounds.
    meanToolErrors: meanOf(results, 'toolErrors'),
    meanEditMisses: meanOf(results, 'editMisses'),
    meanVerifyRounds: meanOf(results, 'verifyRounds'),
    toolErrorClasses: classTotals(results),
    stages: stageTotals(results),
    // G03: beside the mean, whether the agent can (pass@k) and how
    // reliably it does (pass^k), over each task's repetitions.
    passAt: passRates(tasks, passAt),
    passHat: passRates(tasks, passHat),
    power: powerOf(results, tasks, clusterOf),
    tasks,
  };
}

/** P07 (evolve-plan): the significance and power the smallest changes are told at. */
const ALPHA = 0.05;
const POWER = 0.8;

/**
 * P07: the smallest change in success a paired comparison of this
 * campaign with another like it would find (`mde`), from its tasks'
 * run-to-run noise, its repetitions and its modules' design effect.
 */
function powerOf(results, tasks, clusterOf) {
  const repeats = Math.max(0, ...tasks.map((t) => t.runs));
  const deff = designEffect(results, clusterOf);
  return {
    repeats,
    deff,
    alpha: ALPHA,
    power: POWER,
    mde: minimumDetectableEffect(tasks, { repeats, deff, alpha: ALPHA, power: POWER }),
  };
}

/** P07: the power line of a summary. */
function powerLine(p) {
  if (!p) return [];
  if (p.mde === null) return ['- Smallest change it can show: not known (no task ran twice)'];
  return [
    `- Smallest change it can show: ${(p.mde * 100).toFixed(1)} pts, against another campaign ` +
      `like it (${p.repeats} repetitions each, α ${p.alpha}, power ${Math.round(p.power * 100)}%, ` +
      `the modules' design effect ${p.deff.toFixed(2)})`,
  ];
}

const pct = (x) => `${Math.round(x * 100)}%`;

/**
 * G03: pass@k and pass^k for k past 1 (pass@1 and pass^1 are the success
 * rate), each over the tasks with k runs: '' when no task has two.
 */
function passLine(s) {
  const more = (rates) => (rates ?? []).filter((r) => r.k > 1);
  const at = more(s.passAt);
  if (!at.length) return '';
  const over = (r) => (r.tasks === s.tasks.length ? '' : ` over ${r.tasks} tasks`);
  const list = (sign, rates) => rates.map((r) => `pass${sign}${r.k} ${pct(r.rate)}${over(r)}`);
  return `- ${[...list('@', at), ...list('^', more(s.passHat))].join(', ')}`;
}

/** G02: the runs' tool errors by class, the most common first. */
function classesLine(classes) {
  const sorted = Object.entries(classes ?? {}).sort(
    ([a, x], [b, y]) => y - x || a.localeCompare(b)
  );
  return sorted.length ? `; by class: ${sorted.map(([c, n]) => `${c} ${n}`).join(', ')}` : '';
}

/** G02: an Autopilot campaign's stages as a table ([] for none). */
function stagesTable(stages) {
  if (!stages?.length) return [];
  return [
    '## Stages',
    '',
    'Each stage as its runs ended it (their last round), and its rounds a run.',
    '',
    '| Stage | Runs | Done | Failed | Skipped | Other | Rounds a run |',
    '| --- | --- | --- | --- | --- | --- | --- |',
    ...stages.map(
      (t) =>
        `| ${t.name} | ${t.runs} | ${t.done} | ${t.failed} | ${t.skipped} | ${t.other} | ` +
        `${t.meanRounds.toFixed(1)} |`
    ),
    '',
  ];
}

/** The summary as Markdown. */
export function summaryMarkdown(s, title) {
  const passes = passLine(s);
  const lines = [
    `# ${title}`,
    '',
    `- Runs: ${s.runs}`,
    `- Success rate: ${pct(s.successRate)}` +
      (s.perRep.length > 1
        ? ` (per repetition: ${s.perRep.map((r) => pct(r.successRate)).join(', ')}; ` +
          `stdev ${pct(s.successRateStdev)})`
        : ''),
    ...(passes ? [passes] : []),
    ...powerLine(s.power),
    ...(s.tampered
      ? [
          `- Tampered: ${s.tampered} run${s.tampered === 1 ? '' : 's'}, failed whatever ` +
            'the tests said (each result says why)',
        ]
      : []),
    ...(s.review
      ? [
          `- Marked for reading: ${s.review} run${s.review === 1 ? '' : 's'} (their \`review\`: ` +
            'a weak signal in what they did)',
        ]
      : []),
    `- Cost: $${s.totalCostUsd.toFixed(2)} in all, ${money(s.meanCostUsd, 3)} a run` +
      (s.unknownCosts
        ? ` (${s.unknownCosts} run${s.unknownCosts === 1 ? '' : 's'} not counted: cost unknown)`
        : ''),
    `- Tokens a run: ${Math.round(s.meanTokens).toLocaleString('en-US')}; ` +
      `peak main-agent context: ${s.maxPeakLeadContext.toLocaleString('en-US')}`,
    `- Time a run: ${s.meanMinutes.toFixed(1)} min; interventions a run: ${s.meanInterventions.toFixed(2)}`,
    `- Tool errors a run: ${(s.meanToolErrors ?? 0).toFixed(2)} ` +
      `(edit misses ${(s.meanEditMisses ?? 0).toFixed(2)})${classesLine(s.toolErrorClasses)}; ` +
      `checks' rounds a run: ${(s.meanVerifyRounds ?? 0).toFixed(2)}`,
    '',
    '| Task | Success | Cost a run | Tokens a run | Peak context | Minutes | Tool errors | Edit misses | Check rounds | Outcomes |',
    '| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |',
    ...s.tasks.map(
      (t) =>
        `| ${t.task} | ${t.successes}/${t.runs} | ${money(t.meanCostUsd, 3)} | ` +
        `${Math.round(t.meanTokens).toLocaleString('en-US')} | ` +
        `${t.maxPeakLeadContext.toLocaleString('en-US')} | ${t.meanMinutes.toFixed(1)} | ` +
        `${(t.meanToolErrors ?? 0).toFixed(1)} | ${(t.meanEditMisses ?? 0).toFixed(1)} | ` +
        `${(t.meanVerifyRounds ?? 0).toFixed(1)} | ${t.outcomes.join(', ')} |`
    ),
    '',
    ...stagesTable(s.stages),
  ];
  return lines.join('\n');
}

// ------------------------------------------------- G02 and G03: the variants

/**
 * The kinds G02 measures one at a time, each with its milestone, and the
 * ones this task set can't exercise (they stay off by default: there's no
 * evidence for them).
 */
export const MEASURED_KINDS = [
  { id: 'scout', milestone: 'A', label: 'Scout' },
  { id: 'reviewer', milestone: 'B', label: 'Change reviewer' },
  { id: 'triage', milestone: 'C', label: 'Failure triage' },
  { id: 'testWriter', milestone: 'E', label: 'Test writer' },
];
export const UNMEASURED_KINDS = [
  {
    id: 'planReviewer',
    milestone: 'B',
    label: 'Plan reviewer',
    why: 'it runs only in Plan mode, and the tasks run in Agent mode',
  },
  {
    id: 'worker',
    milestone: 'D',
    label: 'Step worker',
    why: 'it carries out saved plans, and the tasks are requests, not plans',
  },
  {
    id: 'parallelWorkers',
    milestone: 'F, R',
    label: 'Parallel implementers and the repair loop',
    why: 'they need a plan with groups in an isolated chat',
  },
];

/** A copy of the runner's settings with subagents and only `kind` on (null: all off). */
export function variantSettings(base, kind) {
  const s = structuredClone(base);
  s.config = s.config ?? {};
  s.config.subagents = kind
    ? { ...(s.config.subagents ?? {}), enabled: true, kinds: { [kind]: { enabled: true } } }
    : { enabled: false, kinds: {} };
  return s;
}

/** Whether the settings turn any subagent on (the baseline must have none). */
export function subagentsOn(settings) {
  const sub = settings?.config?.subagents;
  return !!sub?.enabled && Object.values(sub.kinds ?? {}).some((k) => k?.enabled);
}

/**
 * G03 (docs/design/autopilot-plan.md): Autopilot at each effort, beside the
 * kinds: the request as a task (`--mode autopilot --effort`), on the
 * baseline's settings. A task turns on the kinds its stages need itself.
 */
export const AUTOPILOT_VARIANTS = [
  { id: 'autopilot-balanced', effort: 'balanced', label: 'Autopilot, Balanced' },
  { id: 'autopilot-thorough', effort: 'thorough', label: 'Autopilot, Thorough' },
];

/**
 * An Autopilot variant's settings: the baseline's, with the task's own
 * budget (`budget.json` `autopilot.efforts`), so that a task ends itself
 * and delivers what it has before the runner's caps cut it. The Prepare
 * stage is off: the evaluation gives each workspace its dependencies,
 * and an install would change the caches every run shares (P6.5), as a
 * `pip install` into the shared virtualenv did in the dry run.
 */
export function autopilotSettings(base, effort, budget) {
  const s = structuredClone(base);
  s.config = s.config ?? {};
  s.config.autopilot = {
    ...(s.config.autopilot ?? {}),
    effort,
    budget: { maxHours: budget.maxHours, maxCost: null, maxTokens: budget.maxTokens },
    prepare: false,
  };
  return s;
}

/**
 * The runner's flags for a task's scripted messages (steering-plan.md, D18):
 * each of its `steers`, `{ text, after }`, sent to the run once the chat has
 * `after` tool results (`--steer TEXT@N`). None with `steers: false`, so a
 * campaign can run the same tasks without the facts they give mid-run.
 */
export function steerArgs(task, { steers = true } = {}) {
  if (!steers || task.steers === undefined) return [];
  if (!Array.isArray(task.steers)) throw new Error(`${task.id}: steers is a list`);
  return task.steers.flatMap((s, i) => {
    const ok =
      typeof s?.text === 'string' &&
      s.text.trim() !== '' &&
      Number.isInteger(s.after) &&
      s.after >= 0;
    if (!ok) throw new Error(`${task.id}: steer ${i + 1} needs a text and an after (tool results)`);
    return ['--steer', `${s.text}@${s.after}`];
  });
}

/**
 * The runner's caps for an Autopilot run: a tenth over the task's own
 * budget, so the task's ends it first and the caps only catch a runaway.
 */
export function autopilotCaps(budget) {
  // Times 11, then a tenth: 1.1 itself would make 1.5 hours 5,941 seconds.
  return {
    maxTokens: Math.ceil((budget.maxTokens * 11) / 10),
    timeoutSeconds: Math.ceil((budget.maxHours * 3600 * 11) / 10),
  };
}

/** G03: `rates` (passAt or passHat) at `k`, null when no task had k runs. */
const rateAt = (rates, k) => (rates ?? []).find((r) => r.k === k)?.rate ?? null;

/**
 * G03: campaign `b` against campaign `a`, over the tasks both ran (both
 * `summarize` results over those tasks): every number of each, and
 * whether `b` is better beyond the noise within its cost budget. The
 * noise is the larger spread between repetitions, and at least one task;
 * `costRatioMax` null: any cost. P04 (evolve-plan): beside it, task by
 * task (`paired`): the mean difference, its standard error plain and
 * clustered by module (`clusterOf`: a task's id to its module), a 95%
 * bootstrap interval, and the tasks won, lost and tied. The verdict is
 * G03's still.
 */
export function compareCampaigns(
  a,
  b,
  { costRatioMax = null, minTasks = 5, stopped = null, clusterOf = (task) => task } = {}
) {
  const tasks = Math.min(a.tasks.length, b.tasks.length);
  const noise = Math.max(
    width(a.successRateRange),
    width(b.successRateRange),
    tasks ? 1 / tasks : 1
  );
  const costsKnown =
    !a.unknownCosts && !b.unknownCosts && a.meanCostUsd !== null && b.meanCostUsd !== null;
  const ratioOf = (x, y) => (x > 0 && y !== null ? y / x : null);
  const ks = [...new Set([...(a.passAt ?? []), ...(b.passAt ?? [])].map((r) => r.k))].sort(
    (p, q) => p - q
  );
  const numbers = {
    tasks,
    successDiff: b.successRate - a.successRate,
    noise,
    costRatio: costsKnown ? ratioOf(a.meanCostUsd, b.meanCostUsd) : null,
    costRatioMax,
    tokensRatio: ratioOf(a.meanTokens, b.meanTokens),
    minutesRatio: ratioOf(a.meanMinutes, b.meanMinutes),
    passAt: ks.map((k) => ({ k, a: rateAt(a.passAt, k), b: rateAt(b.passAt, k) })),
    passHat: ks.map((k) => ({ k, a: rateAt(a.passHat, k), b: rateAt(b.passHat, k) })),
    paired: (({ diffs, ...rest }) => rest)(pairedComparison(a.tasks, b.tasks, clusterOf)),
    // P07: the smallest change these tasks, these repetitions, can show.
    mde: detectableEffect({
      variance: meanRunVariance([...a.tasks, ...b.tasks]),
      tasks,
      repeats: Math.min(a.power?.repeats ?? 0, b.power?.repeats ?? 0),
      deff: Math.max(a.power?.deff ?? 1, b.power?.deff ?? 1),
      alpha: ALPHA,
      power: POWER,
    }),
  };
  const verdict = (better, reason) => ({ better, reason, ...numbers });
  if (stopped) return verdict(null, `not enough runs: a campaign stopped (${stopped})`);
  if (tasks < minTasks)
    return verdict(null, `not enough runs: ${tasks} tasks in both (at least ${minTasks})`);
  if (numbers.successDiff <= numbers.noise)
    return verdict(
      false,
      numbers.successDiff < -noise
        ? 'its success is worse beyond the noise'
        : 'no gain in success beyond the noise'
    );
  if (costRatioMax !== null) {
    if (numbers.costRatio === null)
      return verdict(null, 'its success is better, but the cost ratio isn’t known');
    if (numbers.costRatio > costRatioMax)
      return verdict(
        false,
        `its success is better, at ${numbers.costRatio.toFixed(2)}× the cost: over its ${costRatioMax}× budget`
      );
  }
  return verdict(
    true,
    'its success is better beyond the noise' +
      (costRatioMax === null ? '' : `, within its ${costRatioMax}× cost budget`)
  );
}

/** A number, or "n/a". */
const num = (x, digits) => (typeof x === 'number' ? x.toFixed(digits) : 'n/a');
const rate = (x) => (typeof x === 'number' ? pct(x) : 'n/a');

/**
 * G03: the comparison of two campaigns as Markdown: the numbers side by
 * side, the verdict, the tasks one by one and `b`'s stages.
 * `c`: { a: {name, summary}, b: {name, summary}, decision, date, model, fake }.
 */
export function comparisonMarkdown(c) {
  const { a, b, decision: d } = c;
  const [x, y] = [a.summary, b.summary];
  const row = (label, left, right, diff = '') => `| ${label} | ${left} | ${right} | ${diff} |`;
  const tokens = (s) => Math.round(s.meanTokens).toLocaleString('en-US');
  const peak = (s) => s.maxPeakLeadContext.toLocaleString('en-US');
  const byTask = new Map(x.tasks.map((t) => [t.task, { a: t }]));
  for (const t of y.tasks) byTask.set(t.task, { ...(byTask.get(t.task) ?? {}), b: t });
  const verdict =
    d.better === true
      ? `**${b.name} is better**: ${d.reason}.`
      : d.better === false
        ? `**${b.name} isn’t better**: ${d.reason}.`
        : `**Nothing decided**: ${d.reason}.`;
  return [
    `# Evaluation: ${b.name} against ${a.name}`,
    '',
    c.fake
      ? '> **Dry run with a scripted model: these numbers test the pipeline and say nothing about any real model.**\n'
      : '',
    `Generated by \`node scripts/eval/eval.mjs compare ${a.name} ${b.name}\` on ${c.date}. ` +
      `Model: ${c.model}. Over the ${d.tasks} tasks both ran.`,
    '',
    `| | ${a.name} | ${b.name} | Difference |`,
    '| --- | --- | --- | --- |',
    row('Runs', x.runs, y.runs),
    row(
      'Success',
      pct(x.successRate),
      pct(y.successRate),
      `${pctSigned(d.successDiff)} (noise ±${Math.round(d.noise * 100)})`
    ),
    ...pairedRows(d.paired, row),
    row(
      'Smallest change it can show',
      '',
      '',
      typeof d.mde === 'number'
        ? `${(d.mde * 100).toFixed(1)} pts (α ${ALPHA}, power ${Math.round(POWER * 100)}%)`
        : 'not known (no task ran twice)'
    ),
    ...d.passAt.filter((p) => p.k > 1).map((p) => row(`pass@${p.k}`, rate(p.a), rate(p.b))),
    ...d.passHat.filter((p) => p.k > 1).map((p) => row(`pass^${p.k}`, rate(p.a), rate(p.b))),
    row('Cost a run', money(x.meanCostUsd, 3), money(y.meanCostUsd, 3), ratio(d.costRatio)),
    row('Tokens a run', tokens(x), tokens(y), ratio(d.tokensRatio)),
    row('Peak main-agent context', peak(x), peak(y)),
    row('Minutes a run', num(x.meanMinutes, 1), num(y.meanMinutes, 1), ratio(d.minutesRatio)),
    row('Interventions a run', num(x.meanInterventions, 2), num(y.meanInterventions, 2)),
    row('Tool errors a run', num(x.meanToolErrors, 2), num(y.meanToolErrors, 2)),
    row('Edit misses a run', num(x.meanEditMisses, 2), num(y.meanEditMisses, 2)),
    row('Check rounds a run', num(x.meanVerifyRounds, 2), num(y.meanVerifyRounds, 2)),
    '',
    verdict,
    '',
    '## Each task',
    '',
    `| Task | ${a.name} | ${b.name} | Outcomes (${b.name}) |`,
    '| --- | --- | --- | --- |',
    ...[...byTask.entries()]
      .sort(([p], [q]) => p.localeCompare(q))
      .map(
        ([task, t]) =>
          `| ${task} | ${t.a ? `${t.a.successes}/${t.a.runs}` : '—'} | ` +
          `${t.b ? `${t.b.successes}/${t.b.runs}` : '—'} | ${t.b ? t.b.outcomes.join(', ') : ''} |`
      ),
    '',
    ...stagesTable(y.stages).map((l) => (l === '## Stages' ? `## Stages (${b.name})` : l)),
  ].join('\n');
}

const width = (range) => (range?.length === 2 ? range[1] - range[0] : 0);

/** Points, signed, to one decimal: "+3.3 pts". */
const ptsSigned = (x) => `${x >= 0 ? '+' : '−'}${(Math.abs(x) * 100).toFixed(1)} pts`;
const pts = (x) => (typeof x === 'number' ? `${(x * 100).toFixed(1)} pts` : 'n/a');

/**
 * P04: the paired comparison's rows: the tasks each side did better on,
 * and the mean difference task by task with its errors and interval.
 */
function pairedRows(p, row) {
  if (!p?.tasks) return [];
  const interval = p.interval
    ? `95% interval ${ptsSigned(p.interval[0])} to ${ptsSigned(p.interval[1])}`
    : 'no interval (one module)';
  return [
    row('Tasks better (task by task)', p.losses, p.wins, `${p.ties} tied`),
    row(
      'Difference task by task',
      '',
      '',
      `${ptsSigned(p.meanDiff)} (SE ${pts(p.se)}; clustered by module, ${p.clusters} of them, ` +
        `${pts(p.seClustered)}; ${interval})`
    ),
  ];
}

/**
 * G02/G03: a variant against the baseline, over the tasks both ran, and the
 * decision. A kind goes on by default only if success isn't worse beyond
 * the noise, it costs at most `costRatioMax` times the baseline a task, and
 * it improved success beyond the noise or cut the lead's peak context by
 * at least 10% (subagents.md section 6).
 *
 * `base` and `variant` are summarize() results over the same tasks; the
 * noise is the larger spread between repetitions, and at least one task.
 */
export function compareVariant(base, variant, { costRatioMax, minTasks = 5, stopped = null }) {
  const tasks = Math.min(base.tasks.length, variant.tasks.length);
  const meanPeak = (s) => mean(s.tasks.map((t) => t.maxPeakLeadContext));
  const noise = Math.max(
    width(base.successRateRange),
    width(variant.successRateRange),
    tasks ? 1 / tasks : 1
  );
  const successDiff = variant.successRate - base.successRate;
  // P6.6: with any run's cost unknown, the ratio isn't known either.
  const costsKnown = !base.unknownCosts && !variant.unknownCosts && variant.meanCostUsd !== null;
  const costRatio =
    costsKnown && base.meanCostUsd > 0 ? variant.meanCostUsd / base.meanCostUsd : null;
  const basePeak = meanPeak(base);
  const contextRatio = basePeak > 0 ? meanPeak(variant) / basePeak : null;
  const numbers = {
    tasks,
    runs: variant.runs,
    baseRuns: base.runs,
    successRate: variant.successRate,
    baseSuccessRate: base.successRate,
    successDiff,
    noise,
    costRatio,
    costRatioMax,
    contextRatio,
    meanCostUsd: variant.meanCostUsd,
    baseMeanCostUsd: base.meanCostUsd,
    meanPeakContext: meanPeak(variant),
    baseMeanPeakContext: basePeak,
    meanMinutes: variant.meanMinutes,
    baseMeanMinutes: base.meanMinutes,
    meanInterventions: variant.meanInterventions,
    baseMeanInterventions: base.meanInterventions,
  };
  const off = (reason) => ({ on: false, reason, ...numbers });
  if (stopped) return off(`not enough runs: its campaign stopped (${stopped})`);
  if (tasks < minTasks)
    return off(`not enough runs: ${tasks} tasks in both (at least ${minTasks})`);
  if (successDiff < -noise) return off('success is worse than the baseline beyond the noise');
  if (costRatio === null)
    return off(
      costsKnown
        ? 'the baseline cost nothing: there’s no cost ratio'
        : 'some runs’ cost is unknown (no usage reported)'
    );
  if (costRatio > costRatioMax)
    return off(`it costs ${costRatio.toFixed(2)}× the baseline, over its ${costRatioMax}× budget`);
  if (successDiff > noise)
    return {
      on: true,
      reason: 'success improved beyond the noise, within its cost budget',
      ...numbers,
    };
  if (contextRatio !== null && contextRatio <= 0.9)
    return {
      on: true,
      reason: `the lead’s peak context fell to ${Math.round(contextRatio * 100)}% of the baseline’s, within its cost budget`,
      ...numbers,
    };
  return off('no measurable gain in success or the lead’s context');
}

const pctSigned = (x) => `${x >= 0 ? '+' : '−'}${Math.round(Math.abs(x) * 100)} pts`;
const ratio = (x) => (x === null || x === undefined ? 'n/a' : `${x.toFixed(2)}×`);

/** The results file (docs/design/subagents-results.md). */
export function resultsMarkdown(c) {
  const rows = c.variants.map((v) => {
    const d = v.decision;
    return (
      `| ${v.label} (${v.milestone}) | ${d.runs} | ${pct(d.successRate)} ` +
      `(${pctSigned(d.successDiff)}, noise ±${Math.round(d.noise * 100)}) | ` +
      `${money(d.meanCostUsd, 3)} (${ratio(d.costRatio)}) | ${ratio(d.contextRatio)} | ` +
      `${d.meanMinutes.toFixed(1)} (${d.baseMeanMinutes.toFixed(1)}) | ` +
      `${d.meanInterventions.toFixed(2)} (${d.baseMeanInterventions.toFixed(2)}) | ` +
      `${d.on ? '**on**' : 'off'}: ${d.reason} |`
    );
  });
  const b = c.baseline;
  return [
    '# Subagents: evaluation results (G02, G03)',
    '',
    c.fake
      ? '> **Dry run with a scripted model: these numbers test the pipeline and say nothing about any real model.**\n'
      : '',
    `Generated by \`node scripts/eval/eval.mjs compare\` on ${c.date}. Model: ${c.model}.`,
    `Tasks: ${c.tasks.length}; repetitions as run. Per-task cost budget: ${c.costRatioMax}× the baseline.`,
    '',
    '## Baseline (all subagents off)',
    '',
    `- Runs: ${b.runs}; success ${pct(b.successRate)}` +
      (b.perRep.length > 1
        ? ` (per repetition: ${b.perRep.map((r) => pct(r.successRate)).join(', ')})`
        : ''),
    `- Cost a run: ${money(b.meanCostUsd, 3)}; time a run: ${b.meanMinutes.toFixed(1)} min; ` +
      `interventions a run: ${b.meanInterventions.toFixed(2)}`,
    '',
    '## Each kind on by itself',
    '',
    'Success is against the baseline over the same tasks; the noise is the larger spread between ' +
      'repetitions (at least one task). Cost and peak lead context are ratios to the baseline. ' +
      'Minutes and interventions are a run, with the baseline’s in brackets.',
    '',
    '| Kind | Runs | Success | Cost a run | Peak lead context | Minutes | Interventions | Default (G03) |',
    '| --- | --- | --- | --- | --- | --- | --- | --- |',
    ...rows,
    '',
    '## Not measured',
    '',
    ...c.unmeasured.map((u) => `- ${u.label} (${u.milestone}): off by default; ${u.why}.`),
    '',
    '## Decision',
    '',
    c.on.length
      ? `On by default: ${c.on.join(', ')} (with the master switch). Everything else stays off.`
      : 'No kind showed a measurable gain within its budget: every kind stays off by default.',
    '',
  ].join('\n');
}

/**
 * G03: the app's defaults with `kinds` on (and the master switch, when any
 * is): src/lib/subagentSettings.ts, `DEFAULT_SUBAGENT_SETTINGS`. Throws when
 * the source doesn't look as expected.
 */
export function applyDefaults(source, kinds) {
  const start = source.indexOf('export const DEFAULT_SUBAGENT_SETTINGS');
  if (start < 0) throw new Error('DEFAULT_SUBAGENT_SETTINGS not found');
  const head = source.slice(0, start);
  const body = source.slice(start);
  const end = body.indexOf('\n};');
  if (end < 0) throw new Error('the end of DEFAULT_SUBAGENT_SETTINGS not found');
  let block = body.slice(0, end);
  const rest = body.slice(end);
  if (kinds.length) {
    if (!/\n {2}enabled: (false|true),/.test(block)) throw new Error('its enabled line not found');
    block = block.replace(/\n {2}enabled: (false|true),/, '\n  enabled: true,');
  }
  for (const kind of kinds) {
    const re = new RegExp(`\\n {4}${kind}: \\{ \\.\\.\\.DEFAULT_KIND(, enabled: true)? \\},`);
    if (!re.test(block)) throw new Error(`the ${kind} line not found`);
    block = block.replace(re, `\n    ${kind}: { ...DEFAULT_KIND, enabled: true },`);
  }
  return head + block + rest;
}
