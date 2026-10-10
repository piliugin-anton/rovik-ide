// The evaluation's pure parts: reading test runners' output, grading,
// budget decisions and the summary. eval.mjs does the I/O.

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
      meanCostUsd: mean(runs.map((r) => r.costUsd ?? 0)),
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
    totalCostUsd: results.reduce((a, r) => a + (r.costUsd ?? 0), 0),
    meanCostUsd: mean(results.map((r) => r.costUsd ?? 0)),
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
    `- Cost: $${s.totalCostUsd.toFixed(2)} in all, $${s.meanCostUsd.toFixed(3)} a run`,
    `- Tokens a run: ${Math.round(s.meanTokens).toLocaleString('en-US')}; ` +
      `peak main-agent context: ${s.maxPeakLeadContext.toLocaleString('en-US')}`,
    `- Time a run: ${s.meanMinutes.toFixed(1)} min; interventions a run: ${s.meanInterventions.toFixed(2)}`,
    '',
    '| Task | Success | Cost a run | Tokens a run | Peak context | Minutes | Outcomes |',
    '| --- | --- | --- | --- | --- | --- | --- |',
    ...s.tasks.map(
      (t) =>
        `| ${t.task} | ${t.successes}/${t.runs} | $${t.meanCostUsd.toFixed(3)} | ` +
        `${Math.round(t.meanTokens).toLocaleString('en-US')} | ` +
        `${t.maxPeakLeadContext.toLocaleString('en-US')} | ${t.meanMinutes.toFixed(1)} | ` +
        `${t.outcomes.join(', ')} |`
    ),
    '',
  ];
  return lines.join('\n');
}
