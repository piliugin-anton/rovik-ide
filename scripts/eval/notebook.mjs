// N01 (docs/design/evolve-plan.md, 5.7 C, D18): the notebook, for people.
// What the evaluated runs got wrong, within and across campaigns: the
// commonest failure classes (`tools::error_class`), edit misses, the
// checks' verdicts and regressions per task, each run linked to a
// transcript a person can read; and, if the user opts in, the same from
// ordinary chats' agent logs, which hold what runs did, not what was said.
// Nothing here changes the agent: a person reads it, and fixes go in as
// ordinary pull requests. eval.mjs writes the files; this module only
// reads them (`findCampaigns`, `campaignRuns`, `readChatLog`).
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

/** `tools::CUT_CLASSES`: failures a Stop, a closed app or a budget made, not the model. */
export const CUT_CLASSES = ['stopped', 'interrupted', 'not run'];

/** `tools::EDIT_MISS_CLASSES`: an edit whose old text didn't pick one place in its file. */
export const EDIT_MISS_CLASSES = ['old_text not found', 'old_text not unique'];

/** The checks' verdicts on a round against the rounds before (`repair::Verdict`, kebab-case). */
export const VERDICTS = ['first', 'progress', 'no-progress', 'regression'];

/** How a chat's run may end with nothing to look at: done, or waiting on an approval. */
export const FINE_OUTCOMES = ['done', 'verify-needs-approval'];

/** The checks' last word that counts as failing (a round's status, verify.rs). */
const CHECKS_FAILED = ['failed', 'gave-up', 'rolled-back'];

/** Folders' campaigns are compared with the one of these names in them. */
const REFERENCES = ['current', 'baseline'];

/** Links to a task's failed runs in a campaign's table, and chats' runs listed. */
const LINKS_A_TASK = 5;
const CHAT_RUNS_LISTED = 30;

/** Text in a transcript longer than this is cut in the middle. */
const TEXT_CHARS = 8000;
const RESULT_CHARS = 4000;
const ARGUMENT_CHARS = 3000;
/** A reason in a table (why a run was tampered, or marked for reading). */
const WHY_CHARS = 240;

const add = (map, key, n = 1) => {
  map[key] = (map[key] ?? 0) + n;
  return map;
};
/** A count's entries, the largest first. */
const sorted = (map) =>
  Object.entries(map ?? {}).sort(([a, x], [b, y]) => y - x || a.localeCompare(b));
const plural = (n, word, many = `${word}s`) => `${n} ${n === 1 ? word : many}`;

function groupBy(xs, keyOf) {
  const groups = new Map();
  for (const x of xs) {
    const key = keyOf(x);
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(x);
  }
  return groups;
}

// ---------------------------------------------------------- a run's lines

/**
 * A run's tool calls from its log's `tool` lines (its subagents' too),
 * counted as the runner counts them (rovik-headless `tool_counts`): each
 * failure by its class, those the run cut short left out, and the edit
 * misses among them; and each tool's calls, and how its commands ended.
 */
export function toolTally(lines) {
  const t = {
    calls: 0,
    errors: 0,
    classes: {},
    editMisses: 0,
    tools: {},
    commands: 0,
    commandsFailed: 0,
    timedOut: 0,
  };
  for (const l of lines) {
    if (l?.event !== 'tool') continue;
    t.calls++;
    const tool = (t.tools[l.tool ?? 'unknown'] ??= { calls: 0, errors: 0 });
    tool.calls++;
    if (l.tool === 'bash' && 'exitCode' in l) {
      t.commands++;
      if (l.exitCode !== 0) t.commandsFailed++;
    }
    if (l.timedOut === true) t.timedOut++;
    if (l.failed !== true) continue;
    const cls = l.error ?? 'other';
    if (CUT_CLASSES.includes(cls)) continue;
    t.errors++;
    tool.errors++;
    add(t.classes, cls);
    if (EDIT_MISS_CLASSES.includes(cls)) t.editMisses++;
  }
  return t;
}

/**
 * The rounds of the project's checks a run's lead went through, from its
 * log's `verify` lines (verify.rs `log_round`): how each ended, its
 * verdict against the rounds before (`repair::Verdict`: first, progress,
 * no-progress, regression) and the kinds of check that failed in it.
 */
export function verifyRounds(lines, leadId = null) {
  return lines
    .filter((l) => l?.event === 'verify' && (leadId === null || l.dialogId === leadId))
    .map((l) => ({
      round: l.round ?? null,
      status: l.status ?? 'unknown',
      verdict: l.progress ?? null,
      newFailures: l.newFailures ?? 0,
      fixed: l.fixed ?? 0,
      failing: (l.checks ?? []).filter((c) => c?.status === 'failed').map((c) => c.kind),
    }));
}

/** A run with no log lines (its runner wrote no report): its result's counts. */
const resultTally = (result) => ({
  calls: result.toolCalls ?? 0,
  errors: result.toolErrors ?? 0,
  classes: { ...(result.toolErrorClasses ?? {}) },
  editMisses: result.editMisses ?? 0,
  tools: {},
  commands: 0,
  commandsFailed: 0,
  timedOut: 0,
});

// ------------------------------------------------------- evaluated runs

/** A run's key: its campaign, task, repetition and attempt. Its transcript is `transcripts/<key>.md`. */
export const runKey = ({ campaign, task, rep, attempt = null }) =>
  `${campaign}/${task}/${rep}${attempt ? `-${attempt}` : ''}`;

/**
 * One evaluated run as the notebook reads it: its result, graded by the
 * hidden tests, and its report's own log lines (`runLog`). `picked`: for
 * an attempt of P06's same-money arm, whether the checks picked it.
 */
export function evaluatedRun({
  campaign,
  task,
  rep,
  attempt = null,
  picked = null,
  result,
  report = null,
  dir = null,
}) {
  const lines = Array.isArray(report?.runLog) ? report.runLog : null;
  const key = runKey({ campaign, task, rep, attempt });
  const identity = result.identity ?? null;
  return {
    source: 'eval',
    key,
    link: `transcripts/${key}.md`,
    campaign,
    task,
    rep,
    attempt,
    picked,
    dir,
    success: result.success === true,
    outcome: result.outcome ?? null,
    checks: result.verification ?? null,
    tampered: result.tampered ?? [],
    review: result.review ?? [],
    tests: {
      passed: result.grade?.passed ?? null,
      required: result.grade?.required ?? null,
      failed: result.grade?.failed ?? [],
      missing: result.grade?.missing ?? [],
    },
    costUsd: typeof result.costUsd === 'number' ? result.costUsd : null,
    tokens: result.tokens ?? null,
    model: report?.model ?? null,
    scripted: report?.model === 'fake-model',
    // P03: what made it (its runner, settings and policy); null when unknown.
    configuration: identity
      ? [identity.runnerHash, identity.settingsHash, identity.policyHash].join(':')
      : null,
    tally: lines ? toolTally(lines) : resultTally(result),
    rounds: lines ? verifyRounds(lines, report.dialogId ?? null) : [],
  };
}

/** The run a repetition is scored by: itself, or the attempt its checks picked. */
const scored = (r) => r.attempt === null || r.picked === true;

const isDir = (p) => {
  try {
    return statSync(p).isDirectory();
  } catch {
    return false;
  }
};
const subdirs = (p) => {
  try {
    return readdirSync(p, { withFileTypes: true })
      .filter((d) => d.isDirectory())
      .map((d) => d.name)
      .sort();
  } catch {
    return [];
  }
};
const readJson = (p) => {
  try {
    return JSON.parse(readFileSync(p, 'utf8'));
  } catch {
    return null;
  }
};
const REP = /^\d+$/;
const ATTEMPT = /^attempt-(\d+)$/;
const attemptsIn = (repDir) =>
  subdirs(repDir)
    .filter((a) => ATTEMPT.test(a))
    .map((a) => Number(a.match(ATTEMPT)[1]))
    .sort((a, b) => a - b);

/** Whether `dir` is a campaign's: a task's folder in it holds a finished repetition. */
export function isCampaign(dir) {
  return subdirs(dir).some((task) =>
    subdirs(join(dir, task)).some((rep) => {
      if (!REP.test(rep)) return false;
      const repDir = join(dir, task, rep);
      return (
        existsSync(join(repDir, 'result.json')) ||
        attemptsIn(repDir).some((a) => existsSync(join(repDir, `attempt-${a}`, 'result.json')))
      );
    })
  );
}

/** The campaigns' folders in `root` (itself, or those in it: a race's, the variants'), but `skip`'s. */
export function findCampaigns(root, { skip = [], depth = 3 } = {}) {
  if (skip.includes(root) || !isDir(root)) return [];
  if (isCampaign(root)) return [root];
  if (depth === 0) return [];
  return subdirs(root).flatMap((d) => findCampaigns(join(root, d), { skip, depth: depth - 1 }));
}

/**
 * A campaign's finished runs, task by task: each repetition's, or each of
 * its attempts' (P06's same-money arm), the one its checks picked marked.
 */
export function campaignRuns(dir, campaign) {
  const runs = [];
  for (const task of subdirs(dir)) {
    const reps = subdirs(join(dir, task))
      .filter((r) => REP.test(r))
      .sort((a, b) => Number(a) - Number(b));
    for (const rep of reps) {
      const repDir = join(dir, task, rep);
      const attempts = attemptsIn(repDir);
      const picked = attempts.length
        ? (readJson(join(repDir, 'result.json'))?.picked ?? null)
        : null;
      const each = attempts.length
        ? attempts.map((a) => ({ runDir: join(repDir, `attempt-${a}`), attempt: a }))
        : [{ runDir: repDir, attempt: null }];
      for (const { runDir, attempt } of each) {
        const result = readJson(join(runDir, 'result.json'));
        if (!result) continue; // not finished
        runs.push(
          evaluatedRun({
            campaign,
            task,
            rep: Number(rep),
            attempt,
            picked: attempt === null || picked === null ? null : picked === attempt - 1,
            result,
            report: readJson(join(runDir, 'report.json')),
            dir: runDir,
          })
        );
      }
    }
  }
  return runs;
}

// ---------------------------------------------------------------- chats

/** The app's data folder (Tauri's `app_data_dir` for `rovik-ide`), which holds its agent log. */
export function appDataDir({
  platform = process.platform,
  env = process.env,
  home = homedir(),
} = {}) {
  if (platform === 'darwin') return join(home, 'Library', 'Application Support', 'rovik-ide');
  if (platform === 'win32')
    return join(env.APPDATA || join(home, 'AppData', 'Roaming'), 'rovik-ide');
  return join(env.XDG_DATA_HOME || join(home, '.local', 'share'), 'rovik-ide');
}

/** The agent log's lines in `dir`, oldest first: the rotated file's, then the current one's. */
export function readChatLog(dir) {
  const files = ['agent-log.1.jsonl', 'agent-log.jsonl']
    .map((f) => join(dir, f))
    .filter((f) => existsSync(f));
  if (!files.length) throw new Error(`no agent log (agent-log.jsonl) in ${dir}`);
  return files.flatMap((f) =>
    readFileSync(f, 'utf8')
      .split('\n')
      .flatMap((line) => {
        try {
          return line.trim() ? [JSON.parse(line)] : [];
        } catch {
          return []; // a line being written, or cut by the rotation
        }
      })
  );
}

/**
 * Ordinary chats' runs from the app's agent log, oldest line first: a
 * chat's lines up to its `run` line are one run, its subagents' lines
 * (they name their lead, `parentDialogId`) in its lead's. `since`: the
 * lines from then on only (an ISO date or time).
 */
export function chatRuns(lines, { since = null } = {}) {
  const parent = new Map();
  for (const l of lines)
    if (l?.dialogId && l.parentDialogId) parent.set(l.dialogId, l.parentDialogId);
  const leadOf = (id) => {
    for (let i = 0; i < 8 && parent.has(id); i++) id = parent.get(id);
    return id;
  };
  const open = new Map();
  const runs = [];
  for (const l of lines) {
    if (!l?.dialogId || (since && !(String(l.t ?? '') >= since))) continue;
    const lead = leadOf(l.dialogId);
    if (!open.has(lead)) open.set(lead, []);
    open.get(lead).push(l);
    if (l.event === 'run' && l.dialogId === lead) {
      runs.push(chatRun(lead, open.get(lead), l));
      open.delete(lead);
    }
  }
  // A run the log has no end for: the app closed, or it's still going.
  for (const [lead, ls] of open)
    if (ls.some((l) => l.event === 'tool' || l.event === 'reply'))
      runs.push(chatRun(lead, ls, null));
  return runs;
}

function chatRun(lead, lines, end) {
  return {
    source: 'chat',
    dialogId: lead,
    start: lines[0]?.t ?? null,
    end: end?.t ?? null,
    model: end?.model ?? lines.find((l) => l.model && !l.parentDialogId)?.model ?? null,
    outcome: end?.outcome ?? 'no end in the log',
    autopilot: end?.autopilot === true,
    stage: end?.stage ?? null,
    tally: toolTally(lines),
    rounds: verifyRounds(lines, lead),
  };
}

// ------------------------------------------------------------ the notes

/** What runs did, added up: their tools, how they ended, their checks' rounds. */
function tallies(runs) {
  const t = {
    calls: 0,
    errors: 0,
    editMisses: 0,
    commands: 0,
    commandsFailed: 0,
    timedOut: 0,
    classes: {},
    classRuns: {},
    tools: {},
    outcomes: {},
    rounds: 0,
    statuses: {},
    verdicts: {},
  };
  for (const r of runs) {
    for (const k of ['calls', 'errors', 'editMisses', 'commands', 'commandsFailed', 'timedOut'])
      t[k] += r.tally[k] ?? 0;
    for (const [cls, n] of Object.entries(r.tally.classes)) {
      add(t.classes, cls, n);
      add(t.classRuns, cls);
    }
    for (const [name, v] of Object.entries(r.tally.tools)) {
      const e = (t.tools[name] ??= { calls: 0, errors: 0 });
      e.calls += v.calls;
      e.errors += v.errors;
    }
    add(t.outcomes, r.outcome ?? 'none');
    for (const round of r.rounds) {
      t.rounds++;
      add(t.statuses, round.status);
      if (round.verdict) add(t.verdicts, round.verdict);
    }
  }
  return t;
}

/** How each run's checks ended (its last round's status), for runs that had any. */
const endings = (runs) =>
  runs.reduce((m, r) => (r.rounds.length ? add(m, r.rounds.at(-1).status) : m), {});

/** A run as the notes name it, with the path of its transcript. */
const ref = (r) => ({
  key: r.key,
  link: r.link,
  campaign: r.campaign,
  task: r.task,
  rep: r.rep,
  attempt: r.attempt,
});

const rateOf = (x) => (x.runs ? x.passed / x.runs : 1);

function taskNotes(task, runs) {
  const counted = runs.filter(scored);
  const failing = {};
  for (const r of runs) for (const name of new Set(r.tests.failed)) add(failing, name);
  // Tests that didn't run (a build that broke, a crash): too many to name.
  const notRun = runs.filter((r) => r.tests.missing.length);
  const t = tallies(runs);
  return {
    task,
    runs: counted.length,
    passed: counted.filter((r) => r.success).length,
    agentRuns: runs.length,
    errors: t.errors,
    classes: t.classes,
    editMisses: t.editMisses,
    timedOut: t.timedOut,
    rounds: t.rounds,
    verdicts: t.verdicts,
    endings: endings(runs),
    failingTests: sorted(failing).map(([name, n]) => ({ name, runs: n })),
    notRun: {
      runs: notRun.length,
      most: Math.max(0, ...notRun.map((r) => r.tests.missing.length)),
    },
    failed: runs.filter((r) => !r.success).map(ref),
  };
}

/** Whether a task has anything to read: a failed run, a failed tool call, checks that went backwards. */
const worthReading = (t) =>
  t.passed < t.runs ||
  t.failed.length > 0 ||
  t.errors > 0 ||
  t.timedOut > 0 ||
  (t.verdicts.regression ?? 0) > 0 ||
  (t.verdicts['no-progress'] ?? 0) > 0;

function campaignNotes({ name, group = '', runs }) {
  const counted = runs.filter(scored);
  const tasks = [...groupBy(runs, (r) => r.task)]
    .map(([task, rs]) => taskNotes(task, rs))
    .sort((a, b) => rateOf(a) - rateOf(b) || b.errors - a.errors || a.task.localeCompare(b.task));
  return {
    name,
    group,
    scripted: runs.length > 0 && runs.every((r) => r.scripted),
    runs: counted.length,
    agentRuns: runs.length,
    passed: counted.filter((r) => r.success).length,
    tampered: runs.filter((r) => r.tampered.length).length,
    marked: runs.filter((r) => r.review.length).length,
    withChecks: runs.filter((r) => r.rounds.length).length,
    ...tallies(runs),
    tasks,
  };
}

function overallNotes(campaigns) {
  const runs = campaigns.flatMap((c) => c.runs);
  const t = tallies(runs);
  const tasksOf = {};
  const campaignsOf = {};
  for (const r of runs)
    for (const cls of Object.keys(r.tally.classes)) {
      (tasksOf[cls] ??= new Set()).add(r.task);
      (campaignsOf[cls] ??= new Set()).add(r.campaign);
    }
  const counted = runs.filter(scored);
  // Tasks no campaign passed, of those two campaigns or more ran.
  const neverPassed = [...groupBy(counted, (r) => r.task)]
    .map(([task, rs]) => ({
      task,
      runs: rs.length,
      campaigns: new Set(rs.map((r) => r.campaign)).size,
      rs,
    }))
    .filter((x) => x.campaigns > 1 && !x.rs.some((r) => r.success))
    .map(({ rs: _, ...x }) => x)
    .sort((a, b) => a.task.localeCompare(b.task));
  return {
    runs: counted.length,
    agentRuns: runs.length,
    passed: counted.filter((r) => r.success).length,
    calls: t.calls,
    errors: t.errors,
    editMisses: t.editMisses,
    commands: t.commands,
    commandsFailed: t.commandsFailed,
    timedOut: t.timedOut,
    classes: sorted(t.classes).map(([name, calls]) => ({
      name,
      calls,
      runs: t.classRuns[name],
      tasks: tasksOf[name].size,
      campaigns: campaignsOf[name].size,
    })),
    tools: Object.entries(t.tools)
      .map(([name, v]) => ({ name, ...v }))
      .sort((a, b) => b.calls - a.calls || a.name.localeCompare(b.name)),
    outcomes: t.outcomes,
    rounds: t.rounds,
    statuses: t.statuses,
    verdicts: t.verdicts,
    withChecks: runs.filter((r) => r.rounds.length).length,
    neverPassed,
  };
}

const lastPart = (name) => name.split('/').at(-1);

function findNamed(campaigns, name) {
  const exact = campaigns.find((c) => c.name === name);
  if (exact) return exact;
  const named = campaigns.filter((c) => lastPart(c.name) === name);
  if (named.length > 1)
    throw new Error(
      `--against ${name}: more than one campaign is called that (${named.map((c) => c.name).join(', ')})`
    );
  if (!named.length)
    throw new Error(
      `--against ${name}: no such campaign (${campaigns.map((c) => c.name).join(', ')})`
    );
  return named[0];
}

/** Whether two campaigns ran on one runner, settings and policy (P03's identity): then they differ by chance only. */
function sameConfiguration(a, b) {
  const of = (c) => new Set(c.runs.map((r) => r.configuration));
  const x = of(a);
  const y = of(b);
  return x.size === 1 && y.size === 1 && !x.has(null) && [...x][0] === [...y][0];
}

function compareTasks(reference, campaign) {
  const before = groupBy(reference.runs.filter(scored), (r) => r.task);
  const after = groupBy(campaign.runs.filter(scored), (r) => r.task);
  const same = sameConfiguration(reference, campaign);
  const regressions = [];
  const fixes = [];
  let shared = 0;
  for (const [task, rs] of after) {
    const bs = before.get(task);
    if (!bs) continue;
    shared++;
    const b = { passed: bs.filter((r) => r.success).length, runs: bs.length };
    const a = { passed: rs.filter((r) => r.success).length, runs: rs.length };
    const change = a.passed / a.runs - b.passed / b.runs;
    if (change < 0)
      regressions.push({
        task,
        before: b,
        after: a,
        change,
        failed: rs
          .filter((r) => !r.success)
          .slice(0, 3)
          .map(ref),
        passedBefore: bs
          .filter((r) => r.success)
          .slice(0, 1)
          .map(ref),
      });
    else if (change > 0) fixes.push({ task, before: b, after: a, change });
  }
  regressions.sort((x, y) => x.change - y.change || x.task.localeCompare(y.task));
  fixes.sort((x, y) => y.change - x.change || x.task.localeCompare(y.task));
  return {
    reference: reference.name,
    campaign: campaign.name,
    group: campaign.group ?? '',
    shared,
    sameConfiguration: same,
    regressions: same ? [] : regressions,
    fixes: same ? [] : fixes,
  };
}

/**
 * Tasks a campaign passes less often than its reference does, and those it
 * passes more often: in each folder of campaigns (a race's, the
 * variants'), against the one called `current` or `baseline` there; with
 * `against`, every campaign against that one. Two campaigns that ran on
 * one runner, settings and policy differ by chance only: they're said to,
 * and not compared (`eval.mjs power` measures that chance).
 */
export function regressionsOf(campaigns, { against = null } = {}) {
  const pairs = [];
  if (against) {
    const reference = findNamed(campaigns, against);
    for (const c of campaigns) if (c !== reference) pairs.push([reference, c]);
  } else {
    for (const [, group] of groupBy(campaigns, (c) => c.group ?? '')) {
      const reference = group.find((c) => REFERENCES.includes(lastPart(c.name)));
      if (reference) for (const c of group) if (c !== reference) pairs.push([reference, c]);
    }
  }
  return pairs.map(([reference, c]) => compareTasks(reference, c));
}

/** Where a run's own checks and the hidden tests disagree, if they do. A tampered run is listed as that. */
export function disagreement(r) {
  if (r.tampered.length || !r.checks) return null;
  if (r.checks === 'passed' && !r.success) return 'the checks passed; the hidden tests failed';
  if (CHECKS_FAILED.includes(r.checks) && r.success)
    return 'the checks failed; the hidden tests passed';
  return null;
}

/** The hidden tests' word on a run. */
function testsText(r) {
  const { passed, required, failed, missing } = r.tests;
  if (required === null) return 'not graded';
  const some = failed.slice(0, 3).join(', ') + (failed.length > 3 ? ', …' : '');
  return (
    `${passed}/${required}` +
    (failed.length ? `; failed: ${some}` : '') +
    (missing.length ? `; ${missing.length} didn't run` : '')
  );
}

/**
 * What the chats' runs did (`chatRuns`), and the ones to look at: those
 * that didn't end done (or waiting on an approval) or whose checks went
 * backwards, the newest first.
 */
export function chatNotes(runs, { source = null, since = null } = {}) {
  const t = tallies(runs);
  const look = runs
    .filter(
      (r) => !FINE_OUTCOMES.includes(r.outcome) || r.rounds.some((x) => x.verdict === 'regression')
    )
    .sort((a, b) => String(b.end ?? b.start).localeCompare(String(a.end ?? a.start)));
  const times = runs
    .flatMap((r) => [r.start, r.end])
    .filter(Boolean)
    .sort();
  return {
    source,
    since,
    runs: runs.length,
    chats: new Set(runs.map((r) => r.dialogId)).size,
    from: times[0] ?? null,
    to: times.at(-1) ?? null,
    calls: t.calls,
    errors: t.errors,
    editMisses: t.editMisses,
    commands: t.commands,
    commandsFailed: t.commandsFailed,
    timedOut: t.timedOut,
    classes: sorted(t.classes).map(([name, calls]) => ({ name, calls, runs: t.classRuns[name] })),
    outcomes: t.outcomes,
    rounds: t.rounds,
    withChecks: runs.filter((r) => r.rounds.length).length,
    statuses: t.statuses,
    verdicts: t.verdicts,
    toLookAt: look.slice(0, CHAT_RUNS_LISTED).map((r) => ({
      dialogId: r.dialogId,
      when: r.end ?? r.start,
      model: r.model,
      outcome: r.outcome,
      autopilot: r.autopilot,
      stage: r.stage,
      rounds: r.rounds.length,
      verdicts: r.rounds.reduce((m, x) => (x.verdict ? add(m, x.verdict) : m), {}),
      ending: r.rounds.at(-1)?.status ?? null,
      errors: r.tally.errors,
      classes: r.tally.classes,
    })),
    moreToLookAt: Math.max(0, look.length - CHAT_RUNS_LISTED),
  };
}

/**
 * The notebook's data: each campaign's notes (`campaigns`: [{ name, group,
 * runs }], their runs `evaluatedRun`s), what they add up to, the
 * regressions, the runs whose checks and hidden tests disagree, those
 * marked for reading (P09) and those tampered; and the chats' notes
 * (`chatNotes`), if the user opted in.
 */
export function notebook(campaigns, { chats = null, against = null } = {}) {
  const runs = campaigns.flatMap((c) => c.runs);
  return {
    campaigns: campaigns.map(campaignNotes),
    overall: overallNotes(campaigns),
    regressions: regressionsOf(campaigns, { against }),
    disagreements: runs.flatMap((r) => {
      const why = disagreement(r);
      return why ? [{ ...ref(r), why, checks: r.checks, tests: testsText(r) }] : [];
    }),
    marked: runs.filter((r) => r.review.length).map((r) => ({ ...ref(r), why: r.review })),
    tampered: runs.filter((r) => r.tampered.length).map((r) => ({ ...ref(r), why: r.tampered })),
    chats,
  };
}

/** The runs the notebook links to: their transcripts are written beside it. */
export function linkedKeys(data) {
  const keys = new Set();
  const take = (refs) => refs?.forEach((r) => keys.add(r.key));
  for (const c of data.campaigns) for (const t of c.tasks) take(t.failed);
  for (const g of data.regressions)
    for (const x of g.regressions) {
      take(x.failed);
      take(x.passedBefore);
    }
  take(data.disagreements);
  take(data.marked);
  take(data.tampered);
  return keys;
}

// -------------------------------------------------------------- Markdown

const cell = (s) =>
  String(s ?? '')
    .replace(/\|/g, '\\|')
    .replace(/\s*\n\s*/g, ' ');
/** A relative path as a link's target: each part escaped, so spaces and brackets keep the link whole. */
const href = (path) =>
  String(path)
    .split(/[\\/]/)
    .map((p) => encodeURIComponent(p).replace(/\(/g, '%28').replace(/\)/g, '%29'))
    .join('/');
const linkTo = (text, path) => `[${text}](${href(path)})`;
const runName = (r) => `${r.task} ${r.rep}${r.attempt ? `.${r.attempt}` : ''}`;
const runLink = (r, text = runName(r)) => linkTo(text, r.link);
/** A reason in a table, cut past `max` characters: its transcript has it whole. */
const cutShort = (text, max) => {
  const s = String(text ?? '');
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
};
const pct = (n, d) => (d ? `${Math.round((100 * n) / d)}%` : 'n/a');
const countsText = (map, top = Infinity) => {
  const s = sorted(map);
  if (!s.length) return '—';
  const shown = s.slice(0, top).map(([k, n]) => `${k} ${n}`);
  return shown.join(', ') + (s.length > top ? ', …' : '');
};
const verdictWord = (v) => v.replace('-', ' ');
/** Verdicts counted, the worst first. */
const verdictsText = (verdicts) =>
  [...VERDICTS]
    .reverse()
    .filter((v) => verdicts?.[v])
    .map((v) => `${verdictWord(v)} ${verdicts[v]}`)
    .join(', ');

/** A task's checks in a cell: its rounds, the verdicts that went badly, how its runs' checks ended. */
function checksCell({ rounds, verdicts, endings: ends }) {
  if (!rounds) return '—';
  const bad = ['regression', 'no-progress']
    .filter((v) => verdicts[v])
    .map((v) => `${verdictWord(v)} ${verdicts[v]}`);
  return (
    plural(rounds, 'round') +
    (bad.length ? ` (${bad.join(', ')})` : '') +
    (Object.keys(ends ?? {}).length ? `; ended ${countsText(ends)}` : '')
  );
}

function checksLine(o) {
  if (!o.rounds) return "- The project's checks: no run went through a round of them.";
  return (
    `- The project's checks: ${plural(o.rounds, 'round')} in ${plural(o.withChecks, 'run')} ` +
    `(${countsText(o.statuses)}); against the round before: ${verdictsText(o.verdicts) || 'nothing to compare'} ` +
    '(a regression is a failure in what passed before).'
  );
}

function toolsLines(o) {
  if (!o.calls) return ['- Tools: no tool call.'];
  return [
    `- Tools: ${plural(o.calls, 'call')}, ${o.errors} failed (${pct(o.errors, o.calls)}), ` +
      `${plural(o.editMisses, 'edit miss', 'edit misses')}; ` +
      `commands: ${o.commands} ran, ${o.commandsFailed} exited non-zero, ${o.timedOut} timed out.`,
  ];
}

function campaignsTable(data) {
  return [
    '| Campaign | Runs | Passed | Tampered | Marked for reading | Failed tool calls | Edit misses | Check rounds | Regressions in the checks |',
    '| --- | --- | --- | --- | --- | --- | --- | --- | --- |',
    ...data.campaigns.map(
      (c) =>
        `| ${cell(c.name)}${c.scripted ? ' (scripted)' : ''} | ${c.agentRuns}` +
        `${c.agentRuns !== c.runs ? ` (${c.runs} repetitions)` : ''} | ${c.passed} (${pct(c.passed, c.runs)}) | ` +
        `${c.tampered} | ${c.marked} | ${c.errors} | ${c.editMisses} | ${c.rounds} | ${c.verdicts.regression ?? 0} |`
    ),
    '',
  ];
}

function failuresSection(o) {
  const lines = [
    '## The commonest failures',
    '',
    'Failed tool calls by class (`tools::error_class`), over every campaign here; those a Stop, a closed app or a budget cut short are left out.',
    '',
  ];
  if (!o.classes.length) lines.push('No tool call failed.', '');
  else
    lines.push(
      '| Class | Calls | Runs | Tasks | Campaigns |',
      '| --- | --- | --- | --- | --- |',
      ...o.classes
        .slice(0, 15)
        .map((c) => `| ${cell(c.name)} | ${c.calls} | ${c.runs} | ${c.tasks} | ${c.campaigns} |`),
      ...(o.classes.length > 15 ? [`| … ${o.classes.length - 15} more classes | | | | |`] : []),
      ''
    );
  if (o.tools.length)
    lines.push(
      '| Tool | Calls | Failed |',
      '| --- | --- | --- |',
      ...o.tools.map(
        (t) => `| ${cell(t.name)} | ${t.calls} | ${t.errors} (${pct(t.errors, t.calls)}) |`
      ),
      ''
    );
  lines.push(...toolsLines(o), checksLine(o), `- Outcomes: ${countsText(o.outcomes)}.`, '');
  if (o.neverPassed.length)
    lines.push(
      `Tasks no campaign passed (of those two campaigns or more ran): ${o.neverPassed
        .map((t) => `${t.task} (${plural(t.runs, 'run')} in ${t.campaigns})`)
        .join(', ')}. A task that fails everywhere may be broken, or out of reach.`,
      ''
    );
  return lines;
}

function regressionsSection(data) {
  const lines = [
    '## Regressions',
    '',
    "Tasks a campaign passes less often than its reference does: in each folder of campaigns, against the one called `current` or `baseline` (or `--against`). The clearest first, with its failed runs and one of the reference's passes, to read side by side.",
    '',
  ];
  if (!data.regressions.length) {
    lines.push(
      "No campaign here has a reference: a folder's `current` or `baseline`, or one named with `--against`.",
      ''
    );
    return lines;
  }
  for (const g of data.regressions) {
    const inGroup = g.group && g.reference.startsWith(`${g.group}/`);
    const short = (name) => (inGroup ? name.slice(g.group.length + 1) : name);
    lines.push(`### ${short(g.campaign)} against ${short(g.reference)}`, '');
    const where = inGroup ? `In ${g.group}; ` : '';
    if (g.sameConfiguration) {
      lines.push(
        `_${where}${plural(g.shared, 'task')} in both. The same runner, settings and policy as the reference: they differ by chance only, so nothing is called a regression (\`eval.mjs power\` measures that chance)._`,
        ''
      );
      continue;
    }
    lines.push(`_${where}${plural(g.shared, 'task')} in both._`, '');
    if (!g.regressions.length) lines.push('No task passes less often than in the reference.', '');
    else
      lines.push(
        '| Task | The reference | This campaign | Its failed runs | A pass in the reference |',
        '| --- | --- | --- | --- | --- |',
        ...g.regressions.map(
          (x) =>
            `| ${cell(x.task)} | ${x.before.passed}/${x.before.runs} | ${x.after.passed}/${x.after.runs} | ` +
            `${x.failed.map((r) => runLink(r, `${r.rep}${r.attempt ? `.${r.attempt}` : ''}`)).join(' ')} | ` +
            `${x.passedBefore.map((r) => runLink(r, `${r.rep}${r.attempt ? `.${r.attempt}` : ''}`)).join(' ') || '—'} |`
        ),
        ''
      );
    if (g.fixes.length)
      lines.push(
        `Passed more often: ${g.fixes
          .map(
            (x) =>
              `${x.task} (${x.before.passed}/${x.before.runs} → ${x.after.passed}/${x.after.runs})`
          )
          .join(', ')}.`,
        ''
      );
  }
  return lines;
}

function runsTable(title, intro, rows, empty, whyHeader = 'Why') {
  const lines = [`## ${title}`, '', intro, ''];
  if (!rows.length) return [...lines, empty, ''];
  return [
    ...lines,
    `| Run | Campaign | ${whyHeader} |`,
    '| --- | --- | --- |',
    ...rows.map(
      (r) => `| ${runLink(r)} | ${cell(r.campaign)} | ${cell(cutShort(r.why, WHY_CHARS))} |`
    ),
    '',
  ];
}

function disagreementsSection(data) {
  const lines = [
    '## Where the checks and the hidden tests disagree',
    '',
    "Runs whose own checks (the project's, as the agent ran them) said one thing and the hidden tests another: checks that pass a change the tests fail are too weak, or were fooled; checks that fail a change the tests pass cost the run its rounds.",
    '',
  ];
  const withChecks = data.campaigns.reduce((n, c) => n + c.withChecks, 0);
  if (!data.disagreements.length)
    return [
      ...lines,
      withChecks
        ? `None: in the ${plural(withChecks, 'run')} whose checks ran, they agree.`
        : 'No run here went through a round of the checks.',
      '',
    ];
  return [
    ...lines,
    '| Run | Campaign | The checks | The hidden tests |',
    '| --- | --- | --- | --- |',
    ...data.disagreements.map(
      (r) => `| ${runLink(r)} | ${cell(r.campaign)} | ${cell(r.checks)} | ${cell(r.tests)} |`
    ),
    '',
  ];
}

function perCampaignSection(data) {
  const lines = ['## Per campaign', ''];
  for (const c of data.campaigns) {
    lines.push(
      `### ${c.name}`,
      '',
      `${c.scripted ? '_Scripted._ ' : ''}${plural(c.agentRuns, 'run')}` +
        `${c.agentRuns !== c.runs ? ` (${plural(c.runs, 'repetition')}: an attempt counts where the checks picked it)` : ''}, ` +
        `${c.passed} passed (${pct(c.passed, c.runs)}). Outcomes: ${countsText(c.outcomes)}.`,
      ''
    );
    const shown = c.tasks.filter(worthReading);
    const rest = c.tasks.length - shown.length;
    if (shown.length)
      lines.push(
        '| Task | Passed | Failure classes | Edit misses | The checks | Hidden tests | Failed runs |',
        '| --- | --- | --- | --- | --- | --- | --- |',
        ...shown.map((t) => {
          const links = t.failed
            .slice(0, LINKS_A_TASK)
            .map((r) => runLink(r, `${r.rep}${r.attempt ? `.${r.attempt}` : ''}`));
          const more = t.failed.length - links.length;
          // Each test by the runs it failed in, of the task's runs, when it had more than one.
          const many = t.agentRuns > 1;
          const named = t.failingTests
            .slice(0, 3)
            .map((x) => (many ? `${x.name} (${x.runs}/${t.agentRuns})` : x.name));
          const failedText = named.length
            ? `failed: ${named.join(', ')}${t.failingTests.length > 3 ? ', …' : ''}`
            : '';
          const notRun = t.notRun.runs
            ? many
              ? `up to ${t.notRun.most} didn't run (${t.notRun.runs}/${t.agentRuns})`
              : `${t.notRun.most} didn't run`
            : '';
          const testsCell = [failedText, notRun].filter(Boolean).join('; ') || '—';
          return (
            `| ${cell(t.task)} | ${t.passed}/${t.runs} | ${cell(countsText(t.classes, 3))} | ${t.editMisses} | ` +
            `${cell(checksCell(t))} | ${cell(testsCell)} | ` +
            `${links.join(' ')}${more ? ` and ${more} more` : ''}${links.length ? '' : '—'} |`
          );
        }),
        ''
      );
    if (!shown.length) lines.push('Every task passed every run, with no failed tool call.', '');
    else if (rest)
      lines.push(`${plural(rest, 'other task')} passed every run, with no failed tool call.`, '');
  }
  return lines;
}

function chatsSection(ch) {
  const when = (t) => (t ? String(t).slice(0, 16).replace('T', ' ') : '?');
  const lines = [
    '## Ordinary chats',
    '',
    `From the app's agent log in \`${ch.source}\`, opted in with \`--chats\`${ch.since ? `, since ${ch.since}` : ''}: what each run did (its tools and their error classes, the checks' rounds, how it ended), not what was said; the transcripts are in the app. ` +
      `${plural(ch.runs, 'run')} in ${plural(ch.chats, 'chat')}, ${when(ch.from)} to ${when(ch.to)} (UTC).`,
    '',
    ...toolsLines(ch),
    checksLine(ch),
    `- Outcomes: ${countsText(ch.outcomes)}.`,
    `- Failed tool calls by class: ${
      ch.classes.length
        ? ch.classes
            .slice(0, 10)
            .map((c) => `${c.name} ${c.calls}`)
            .join(', ')
        : 'none'
    }.`,
    '',
  ];
  if (!ch.toLookAt.length)
    return [
      ...lines,
      'Every run ended done, or waiting on an approval, and no checks went backwards.',
      '',
    ];
  return [
    ...lines,
    '### Runs to look at',
    '',
    "The runs that didn't end done (or waiting on an approval), or whose checks went backwards; the newest first. A chat is named by its id's start; the time is when its run ended (UTC).",
    '',
    '| When | Chat | Model | Outcome | The checks | Failed tool calls |',
    '| --- | --- | --- | --- | --- | --- |',
    ...ch.toLookAt.map(
      (r) =>
        `| ${when(r.when)} | ${cell(String(r.dialogId).slice(0, 8))} | ${cell(r.model ?? '?')} | ` +
        `${cell(r.outcome)}${r.autopilot ? ` (Autopilot${r.stage ? `, ${r.stage}` : ''})` : ''} | ` +
        `${r.rounds ? cell(`${plural(r.rounds, 'round')}${verdictsText(r.verdicts) ? ` (${verdictsText(r.verdicts)})` : ''}; ended ${r.ending}`) : '—'} | ` +
        `${r.errors ? cell(countsText(r.classes, 3)) : '—'} |`
    ),
    ...(ch.moreToLookAt
      ? [
          '',
          `And ${plural(ch.moreToLookAt, 'older run')}: \`notebook.json\` has them counted, not listed.`,
        ]
      : []),
    '',
  ];
}

/** The notebook as Markdown, for a person (`notebook`'s data; links relative to its folder). */
export function notebookMarkdown(data) {
  const o = data.overall;
  const scripted = data.campaigns.length > 0 && data.campaigns.every((c) => c.scripted);
  const lines = ['# Notebook', ''];
  if (scripted)
    lines.push(
      "> **Every campaign here ran on the scripted model (fake-model.mjs): these failures are the script's, and say nothing about any real model.**",
      ''
    );
  lines.push(
    'Generated by `node scripts/eval/eval.mjs notebook` (N01 in docs/design/evolve-plan.md, 5.7 C): what the runs got wrong, for a person to read. Each run named links to its transcript; fixes go in as ordinary pull requests.',
    '',
    `${plural(o.agentRuns, 'run')} in ${plural(data.campaigns.length, 'campaign')}` +
      (data.chats ? `, and ${plural(data.chats.runs, 'run')} of ordinary chats.` : '.'),
    ''
  );
  if (data.campaigns.length) {
    lines.push(
      ...campaignsTable(data),
      ...failuresSection(o),
      ...regressionsSection(data),
      ...disagreementsSection(data),
      ...runsTable(
        'Marked for reading',
        "The monitor's weak signals (P09): nothing shows the run cheated, but a person should read what it did.",
        data.marked.map((r) => ({ ...r, why: r.why.join('; ') })),
        'None.'
      ),
      ...runsTable(
        'Tampered',
        'Runs failed whatever their tests said (P02, P08, P09), each with why.',
        data.tampered.map((r) => ({ ...r, why: r.why.join('; ') })),
        'None.'
      ),
      ...perCampaignSection(data)
    );
  }
  if (data.chats) lines.push(...chatsSection(data.chats));
  return lines.join('\n').replace(/\n{3,}/g, '\n\n');
}

// ----------------------------------------------------------- transcripts

/** `text` cut in the middle past `max` characters, saying how much is left out. */
export function clip(text, max) {
  const s = String(text ?? '');
  if (s.length <= max) return s;
  const head = Math.floor(max * 0.7);
  const tail = max - head;
  return `${s.slice(0, head)}\n… ${s.length - max} characters left out …\n${s.slice(s.length - tail)}`;
}

/** `text` in a code fence longer than any run of backticks in it. */
export function fenced(text, lang = '') {
  const s = String(text ?? '');
  const longest = Math.max(2, ...[...s.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = '`'.repeat(longest + 1);
  return [`${fence}${lang}`, s, fence];
}

/** `text` as inline code, its fence longer than any run of backticks in it. */
function code(text) {
  const s = String(text).replace(/\s*\n\s*/g, ' ');
  const longest = Math.max(0, ...[...s.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = '`'.repeat(longest + 1);
  const pad = s.startsWith('`') || s.endsWith('`') ? ' ' : '';
  return `${fence}${pad}${s}${pad}${fence}`;
}

/**
 * A tool call's arguments, each by its name: short ones on a line, long or
 * many-lined ones (a file's content, an edit's old text) in a block; a
 * command as shell. Arguments that aren't an object of JSON as they are.
 */
function callLines(call) {
  let args = null;
  try {
    args = JSON.parse(call.arguments ?? 'null');
  } catch {
    // shown as they are
  }
  if (args === null || typeof args !== 'object' || Array.isArray(args))
    return fenced(clip(String(call.arguments ?? ''), ARGUMENT_CHARS));
  const short = [];
  const long = [];
  for (const [key, value] of Object.entries(args)) {
    const text = typeof value === 'string' ? value : JSON.stringify(value);
    if (call.name === 'bash' && key === 'command')
      long.push(fenced(clip(text, ARGUMENT_CHARS), 'sh'));
    else if (text.includes('\n') || text.length > 120)
      long.push([`${code(key)}:`, '', ...fenced(clip(text, ARGUMENT_CHARS))]);
    else short.push(`- ${code(key)}: ${code(text)}`);
  }
  return [short, ...long]
    .filter((part) => part.length)
    .flatMap((part, i) => (i ? ['', ...part] : part));
}

function messageLines(messages) {
  const names = new Map();
  const out = [];
  for (const m of messages) {
    if (m.role === 'assistant') {
      out.push('**Assistant**', '');
      if (String(m.content ?? '').trim()) out.push(clip(m.content, TEXT_CHARS), '');
      for (const c of m.toolCalls ?? []) {
        names.set(c.id, c.name);
        out.push(`→ \`${c.name}\``, '', ...callLines(c), '');
      }
    } else if (m.role === 'tool') {
      out.push(
        `← \`${names.get(m.toolCallId) ?? 'tool'}\``,
        '',
        ...fenced(clip(m.content, RESULT_CHARS)),
        ''
      );
    } else {
      const who = m.role === 'user' ? 'User' : String(m.role ?? 'message');
      out.push(`**${who}**`, '', clip(m.content, TEXT_CHARS), '');
    }
    if (m.error) out.push(`**Error:** ${cell(m.error)}`, '');
  }
  return out;
}

/**
 * A run's transcript for a person: what the hidden tests and its checks
 * said, what it cost, links to its files (`files`: { label: path }, paths
 * relative to the transcript), then its conversation, its subagents' after
 * its lead's, long tool results and arguments cut in the middle.
 */
export function transcriptMarkdown(run, report, files = {}) {
  const attempt = run.attempt
    ? `, attempt ${run.attempt}${run.picked === true ? ' (picked by the checks)' : ''}`
    : '';
  const t = run.tally;
  const lines = [`# ${run.task} · ${run.campaign} · run ${run.rep}${attempt}`, ''];
  if (run.scripted)
    lines.push("> Scripted model (fake-model.mjs): the script's moves, not a model's.", '');
  lines.push(
    `- Passed: ${run.success ? 'yes' : 'no'}; the hidden tests: ${testsText(run)}`,
    `- Outcome: ${run.outcome ?? 'none'}; the checks: ${
      run.rounds.length
        ? `${run.rounds.map((r) => r.status).join(' → ')}${verdictsText(countVerdicts(run.rounds)) ? ` (${verdictsText(countVerdicts(run.rounds))})` : ''}`
        : (run.checks ?? 'no round')
    }`,
    ...(run.tampered.length ? [`- Tampered: ${run.tampered.join('; ')}`] : []),
    ...(run.review.length ? [`- Marked for reading: ${run.review.join('; ')}`] : []),
    `- Tools: ${plural(t.calls, 'call')}, ${t.errors} failed${t.errors ? ` (${countsText(t.classes)})` : ''}` +
      (t.commands
        ? `; commands: ${t.commands} ran, ${t.commandsFailed} exited non-zero, ${t.timedOut} timed out`
        : ''),
    ...(run.costUsd !== null || run.tokens !== null
      ? [
          `- Cost: ${run.costUsd === null ? 'unknown' : `$${run.costUsd.toFixed(4)}`}` +
            (run.tokens !== null ? `, ${Number(run.tokens).toLocaleString('en-US')} tokens` : ''),
        ]
      : [])
  );
  const links = Object.entries(files).map(([label, path]) => linkTo(label, path));
  if (links.length) lines.push(`- Files: ${links.join(' · ')}`);
  lines.push('');
  const transcript = Array.isArray(report?.transcript) ? report.transcript : [];
  if (!transcript.length) {
    lines.push('_No transcript: the runner wrote no report._', '');
    return lines.join('\n');
  }
  const lead = report.dialogId ?? transcript[0].dialogId;
  const subs = new Map(
    (report.subagents ?? []).filter((s) => s?.childDialogId).map((s) => [s.childDialogId, s])
  );
  const chats = groupBy(transcript, (m) => m.dialogId);
  for (const [id, messages] of [...chats].sort(([a], [b]) =>
    a === lead ? -1 : b === lead ? 1 : 0
  )) {
    if (id === lead) lines.push('## The conversation', '');
    else {
      const s = subs.get(id);
      lines.push(
        `## Subagent${s?.kind ? `: ${s.kind}` : ''}${s?.status ? ` (${s.status})` : ''} · ${String(id).slice(0, 8)}`,
        ''
      );
    }
    lines.push(...messageLines(messages));
  }
  return lines.join('\n');
}

const countVerdicts = (rounds) => rounds.reduce((m, r) => (r.verdict ? add(m, r.verdict) : m), {});
