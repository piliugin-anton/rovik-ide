// The evaluation's pure parts: reading test runners' output, grading,
// budget decisions and the summary. eval.mjs does the I/O (but for a
// campaign's lock file, which lives here with its tests).
import { createHash } from 'node:crypto';
import { closeSync, mkdirSync, openSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

/** Results of `cargo test` (libtest's plain output): name -> pass|fail|skip. */
export function parseCargo(text) {
  const results = {};
  for (const m of text.matchAll(/^test (\S+) \.\.\. (ok|FAILED|ignored)/gm)) {
    results[m[1]] = m[2] === 'ok' ? 'pass' : m[2] === 'FAILED' ? 'fail' : 'skip';
  }
  return results;
}

/** Results of `go test -json`: "Package/Test" -> pass|fail|skip. */
export function parseGoJson(text) {
  const results = {};
  for (const line of text.split('\n')) {
    if (!line.startsWith('{')) continue;
    let e;
    try {
      e = JSON.parse(line);
    } catch {
      continue;
    }
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
  for (const line of text.split('\n')) {
    if (!line.startsWith('{"test":')) continue;
    try {
      const { test, status } = JSON.parse(line);
      results[test] = status;
    } catch {
      // not ours
    }
  }
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

/**
 * Whether the next run may start. `spent` is what the campaign's runs
 * cost so far; the next run is assumed to cost their mean (or, before
 * any run, `firstEstimate`).
 */
export function budgetAllows({ cap, spent, costs, firstEstimate }) {
  const estimate = costs.length ? costs.reduce((a, b) => a + b, 0) / costs.length : firstEstimate;
  const ok = spent + estimate <= cap;
  return {
    ok,
    estimate,
    reason: ok
      ? ''
      : `$${spent.toFixed(2)} spent; another run (~$${estimate.toFixed(2)}) would pass the $${cap} cap`,
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
  const perRun = pilotCosts.reduce((a, b) => a + b, 0) / pilotCosts.length;
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

const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const stdev = (xs) => {
  if (xs.length < 2) return 0;
  const m = mean(xs);
  return Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / (xs.length - 1));
};

/**
 * The campaign's summary from its runs' results
 * ({task, rep, success, costUsd, tokens, peakLeadContext, ms, interventions, outcome}).
 */
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
  return { spent: costs.reduce((a, c) => a + c, 0), costs, unknown: results.length - costs.length };
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
 * A campaign's runs in order. `plan` lists them, each with `done` when its
 * result is kept from an earlier pass; `runOne(item)` makes the run (or
 * reads its kept result back). A new run starts only if the budget allows
 * it (`budgetAllows` over `knownSpend`), and the campaign stops where
 * `stopAfter` says. Returns the results and why it stopped ('' if it didn't).
 */
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

export async function runCampaign(plan, { cap, firstEstimate, runOne }) {
  const results = [];
  for (const item of plan) {
    if (!item.done) {
      const { spent, costs } = knownSpend(results);
      const allowed = budgetAllows({ cap, spent, costs, firstEstimate });
      if (!allowed.ok) return { results, stopped: allowed.reason };
    }
    results.push(await runOne(item));
    const stopped = stopAfter(results);
    if (stopped) return { results, stopped };
  }
  return { results, stopped: '' };
}

export function summarize(results) {
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
    perRep,
    successRateStdev: stdev(rates),
    successRateRange: rates.length ? [Math.min(...rates), Math.max(...rates)] : [0, 0],
    totalCostUsd: knownCosts(results).reduce((a, c) => a + c, 0),
    meanCostUsd: meanCost(results),
    unknownCosts: unknownCosts(results),
    meanTokens: mean(results.map((r) => r.tokens ?? 0)),
    maxPeakLeadContext: Math.max(0, ...results.map((r) => r.peakLeadContext ?? 0)),
    meanMinutes: mean(results.map((r) => (r.ms ?? 0) / 60000)),
    meanInterventions: mean(results.map((r) => r.interventions ?? 0)),
    tasks,
  };
}

const pct = (x) => `${Math.round(x * 100)}%`;

/** The summary as Markdown. */
export function summaryMarkdown(s, title) {
  const lines = [
    `# ${title}`,
    '',
    `- Runs: ${s.runs}`,
    `- Success rate: ${pct(s.successRate)}` +
      (s.perRep.length > 1
        ? ` (per repetition: ${s.perRep.map((r) => pct(r.successRate)).join(', ')}; ` +
          `stdev ${pct(s.successRateStdev)})`
        : ''),
    `- Cost: $${s.totalCostUsd.toFixed(2)} in all, ${money(s.meanCostUsd, 3)} a run` +
      (s.unknownCosts
        ? ` (${s.unknownCosts} run${s.unknownCosts === 1 ? '' : 's'} not counted: cost unknown)`
        : ''),
    `- Tokens a run: ${Math.round(s.meanTokens).toLocaleString('en-US')}; ` +
      `peak main-agent context: ${s.maxPeakLeadContext.toLocaleString('en-US')}`,
    `- Time a run: ${s.meanMinutes.toFixed(1)} min; interventions a run: ${s.meanInterventions.toFixed(2)}`,
    '',
    '| Task | Success | Cost a run | Tokens a run | Peak context | Minutes | Outcomes |',
    '| --- | --- | --- | --- | --- | --- | --- |',
    ...s.tasks.map(
      (t) =>
        `| ${t.task} | ${t.successes}/${t.runs} | ${money(t.meanCostUsd, 3)} | ` +
        `${Math.round(t.meanTokens).toLocaleString('en-US')} | ` +
        `${t.maxPeakLeadContext.toLocaleString('en-US')} | ${t.meanMinutes.toFixed(1)} | ` +
        `${t.outcomes.join(', ')} |`
    ),
    '',
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

const width = (range) => (range?.length === 2 ? range[1] - range[0] : 0);

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
  const meanPeak = (s) =>
    s.tasks.length ? s.tasks.reduce((a, t) => a + t.maxPeakLeadContext, 0) / s.tasks.length : 0;
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
      `| ${v.label} (${v.milestone}) | ${d.runs} | ${Math.round(d.successRate * 100)}% ` +
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
    `- Runs: ${b.runs}; success ${Math.round(b.successRate * 100)}%` +
      (b.perRep.length > 1
        ? ` (per repetition: ${b.perRep.map((r) => `${Math.round(r.successRate * 100)}%`).join(', ')})`
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
