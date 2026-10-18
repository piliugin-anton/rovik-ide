#!/usr/bin/env node
// The agent evaluation (G01 in docs/design/subagents-plan.md): runs the
// headless runner (src-tauri/crates/rovik-headless) on the tasks in tasks/
// and grades each run with the tasks' hidden tests. See README.md.
//
//   node scripts/eval/eval.mjs validate [--tasks a,b]
//   node scripts/eval/eval.mjs dry-run [--tasks a,b]
//   node scripts/eval/eval.mjs pilot --settings FILE
//   node scripts/eval/eval.mjs run --settings FILE
//   node scripts/eval/eval.mjs report DIR
//   node scripts/eval/eval.mjs variants --settings FILE [--kinds a,b | --variants a,b]   (G02)
//   node scripts/eval/eval.mjs compare [--out FILE]                     (G02, G03)
//   node scripts/eval/eval.mjs compare A B [--out FILE]                 (autopilot-plan G03)
//   node scripts/eval/eval.mjs apply-defaults [--comparison FILE]       (G03)
//   node scripts/eval/eval.mjs dry-run-variants [--tasks a,b] [--variants a,b]
//
// The variants are each kind on by itself (`kind-<id>`) and, from the
// Autopilot plan's G03, Autopilot at each effort (`autopilot-balanced`,
// `autopilot-thorough`).
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
  appendFileSync,
  copyFileSync,
} from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  AUTOPILOT_VARIANTS,
  MEASURED_KINDS,
  UNMEASURED_KINDS,
  applyDefaults,
  autopilotCaps,
  autopilotSettings,
  compareCampaigns,
  compareVariant,
  comparisonMarkdown,
  grade,
  parseCargo,
  parseGoJson,
  parseJunit,
  parseTsLines,
  projection,
  resultsMarkdown,
  runCampaign,
  lockCampaign,
  workspaceName,
  subagentsOn,
  summarize,
  summaryMarkdown,
  validation,
  variantSettings,
} from './lib.mjs';
import { run, startServer } from './proc.mjs';
import { changedCaches, copyTree, fingerprint, protect } from './caches.mjs';

const EVAL = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(EVAL, '../..');
const CACHE = resolve(process.env.ROVIK_EVAL_CACHE ?? join(ROOT, '.eval-cache'));
const RESULTS = resolve(process.env.ROVIK_EVAL_RESULTS ?? join(ROOT, 'eval-results'));
const BUDGET = JSON.parse(readFileSync(join(EVAL, 'budget.json'), 'utf8'));

// ---------------------------------------------------------------- helpers

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, {
    encoding: 'utf8',
    maxBuffer: 512 * 1024 * 1024,
    ...opts,
    env: { ...process.env, ...(opts.env ?? {}) },
  });
  if (opts.check !== false && r.status !== 0) {
    throw new Error(
      `${cmd} ${args.join(' ')} failed (${r.status ?? r.signal}):\n${(r.stderr ?? '').slice(-4000)}`
    );
  }
  return r;
}

const readJson = (p) => JSON.parse(readFileSync(p, 'utf8'));
const writeJson = (p, v) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, JSON.stringify(v, null, 2) + '\n');
};

function parseFlags(argv) {
  const flags = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith('--')) {
      const next = argv[i + 1];
      if (next === undefined || next.startsWith('--')) flags[a.slice(2)] = true;
      else flags[a.slice(2)] = argv[++i];
    } else flags._.push(a);
  }
  return flags;
}

function loadTasks(only) {
  const dir = join(EVAL, 'tasks');
  const all = readdirSync(dir)
    .filter((f) => f.endsWith('.json'))
    .sort()
    .map((f) => readJson(join(dir, f)));
  if (!only) return all;
  const ids = String(only).split(',');
  const unknown = ids.filter((id) => !all.some((t) => t.id === id));
  if (unknown.length) throw new Error(`unknown tasks: ${unknown.join(', ')}`);
  return all.filter((t) => ids.includes(t.id));
}

const expectedPath = (task) => join(EVAL, 'expected', `${task.id}.json`);

function expectedOf(task) {
  const p = expectedPath(task);
  if (!existsSync(p)) return null;
  const e = readJson(p);
  return e.reference === task.reference && e.ok ? e : null;
}

// ------------------------------------------------------------- workspaces

/** The task's repository: this one, or a clone in the cache. */
function repoOf(task) {
  if (task.repo === 'self') return ROOT;
  const dir = join(CACHE, 'repos', task.repo.name);
  if (!existsSync(dir)) {
    mkdirSync(dirname(dir), { recursive: true });
    sh('git', ['clone', '-q', task.repo.url, dir]);
  }
  const has = sh('git', ['-C', dir, 'cat-file', '-e', `${task.reference}^{commit}`], {
    check: false,
  });
  if (has.status !== 0) sh('git', ['-C', dir, 'fetch', '-q', 'origin']);
  return dir;
}

const repoName = (task) => (task.repo === 'self' ? 'self' : task.repo.name);

/** The node_modules for a package lock, installed once into the cache. */
function nodeModules(ws) {
  const lock = ['package-lock.json', 'package.json'].find((f) => existsSync(join(ws, f)));
  if (!lock) return null;
  const hash = createHash('sha256')
    .update(readFileSync(join(ws, lock)))
    .digest('hex')
    .slice(0, 16);
  const dir = join(CACHE, 'node', hash);
  if (!existsSync(join(dir, 'node_modules'))) {
    mkdirSync(dir, { recursive: true });
    copyFileSync(join(ws, 'package.json'), join(dir, 'package.json'));
    if (lock === 'package-lock.json') copyFileSync(join(ws, lock), join(dir, lock));
    const args =
      lock === 'package-lock.json'
        ? ['ci', '--ignore-scripts', '--no-audit', '--no-fund']
        : ['install', '--ignore-scripts', '--no-audit', '--no-fund'];
    sh('npm', args, { cwd: dir });
  }
  return join(dir, 'node_modules');
}

/** A virtualenv with pytest, shared by the Python tasks. */
function pythonVenv() {
  const dir = join(CACHE, 'venv');
  if (!existsSync(join(dir, 'bin', 'pytest'))) {
    sh('python3', ['-m', 'venv', dir]);
    sh(join(dir, 'bin', 'pip'), ['install', '-q', 'pytest']);
  }
  // P6.5: shared by every run: no run may change it (one made before
  // this was protected too).
  protect(dir);
  return dir;
}

/**
 * A fresh workspace at `rev`: the files only, in a new git repository
 * with one commit, so the agent can't read the task's answer from the
 * history. Returns the environment its commands and checks run with.
 */
function prepare(task, rev, ws) {
  rmSync(ws, { recursive: true, force: true });
  mkdirSync(ws, { recursive: true });
  const repo = repoOf(task);
  sh('bash', ['-c', 'git -C "$1" archive "$2" | tar -x -C "$3"', '_', repo, rev, ws]);
  const git = (...a) => sh('git', ['-C', ws, ...a]);
  git('init', '-q');
  git('config', 'user.name', 'eval');
  git('config', 'user.email', 'eval@localhost');
  git('add', '-A');
  git('commit', '-q', '--no-verify', '-m', 'Task start');
  const env = {};
  const exclude = [];
  /** Shared caches the run uses, to check after it (P6.5). */
  const caches = [];
  for (const s of task.setup ?? []) {
    if (s === 'node') {
      const nm = nodeModules(ws);
      if (nm) {
        // P6.5: its own copy (a clone where it can be): what the agent
        // does to node_modules stays in this run.
        copyTree(nm, join(ws, 'node_modules'));
        caches.push({ dir: nm, print: fingerprint(nm) });
        exclude.push('node_modules');
      }
    } else if (s === 'cargo') {
      env.CARGO_TARGET_DIR = join(CACHE, 'cargo-target', repoName(task));
      // Incremental caches of many old commits fill a disk quickly.
      env.CARGO_INCREMENTAL = '0';
    } else if (s === 'python') {
      const venv = pythonVenv();
      caches.push({ dir: venv, print: fingerprint(venv) });
      env.VIRTUAL_ENV = venv;
      env.PATH = `${join(venv, 'bin')}:${process.env.PATH}`;
    } else if (s === 'go') {
      env.GOFLAGS = '-mod=mod';
    }
  }
  if (exclude.length)
    appendFileSync(join(ws, '.git', 'info', 'exclude'), exclude.join('\n') + '\n');
  return { env, caches, start: git('rev-parse', 'HEAD').stdout.trim() };
}

/** Puts the reference's version of the hidden test files in the workspace. */
function restoreHidden(task, ws) {
  const repo = repoOf(task);
  for (const f of task.hidden) {
    const r = sh('git', ['-C', repo, 'show', `${task.reference}:${f}`], { encoding: 'buffer' });
    mkdirSync(dirname(join(ws, f)), { recursive: true });
    writeFileSync(join(ws, f), r.stdout);
  }
}

/**
 * Drops what each commit builds anew (the crate's own test binaries and
 * objects, incremental data), keeping the dependencies: a disk fills up
 * with old commits' builds otherwise.
 */
function pruneCargoTarget(dir, manifest) {
  if (!dir) return;
  const toml = readFileSync(manifest, 'utf8');
  const crates = [
    toml.match(/\[package\][^[]*?\bname\s*=\s*"([^"]+)"/)?.[1],
    toml.match(/\[lib\][^[]*?\bname\s*=\s*"([^"]+)"/)?.[1],
  ]
    .filter(Boolean)
    .map((n) => n.replace(/-/g, '_'));
  const debug = join(dir, 'debug');
  rmSync(join(debug, 'incremental'), { recursive: true, force: true });
  const deps = join(debug, 'deps');
  if (!existsSync(deps)) return;
  for (const f of readdirSync(deps)) {
    if (crates.some((c) => f.startsWith(`${c}-`) || f.startsWith(`lib${c}-`))) {
      rmSync(join(deps, f), { recursive: true, force: true });
    }
  }
}

/** Runs the task's checks in the workspace: test name -> pass|fail|skip. */
async function runChecks(task, ws, env, logFile) {
  const results = {};
  for (const check of task.checks) {
    let r;
    const limit = 45 * 60 * 1000;
    if (check.kind === 'cargo') {
      const manifest = join(ws, check.dir, 'Cargo.toml');
      r = await run(
        'cargo',
        ['test', '--manifest-path', manifest, '--lib', '--', ...check.filters],
        {
          env,
          cwd: ws,
          timeoutMs: limit,
          logFile,
        }
      );
      Object.assign(results, parseCargo(r.out));
      pruneCargoTarget(env.CARGO_TARGET_DIR, manifest);
    } else if (check.kind === 'ts') {
      r = await run(process.execPath, [join(EVAL, 'ts-test.mjs'), ws, ...check.files], {
        env,
        cwd: ws,
        timeoutMs: limit,
        logFile,
      });
      Object.assign(results, parseTsLines(r.out));
    } else if (check.kind === 'pytest') {
      const xml = join(ws, '..', `${task.id}-junit.xml`);
      rmSync(xml, { force: true });
      r = await run(
        'python',
        ['-m', 'pytest', '-p', 'no:cacheprovider', '-q', `--junitxml=${xml}`, ...check.files],
        { env, cwd: ws, timeoutMs: limit, logFile }
      );
      if (existsSync(xml)) Object.assign(results, parseJunit(readFileSync(xml, 'utf8')));
      rmSync(xml, { force: true });
    } else if (check.kind === 'go') {
      r = await run('go', ['test', '-json', ...check.packages], {
        env,
        cwd: ws,
        timeoutMs: limit,
        logFile,
      });
      Object.assign(results, parseGoJson(r.out));
    } else {
      throw new Error(`${task.id}: unknown check kind ${check.kind}`);
    }
  }
  return results;
}

// --------------------------------------------------------------- validate

async function validate(flags) {
  const tasks = loadTasks(flags.tasks);
  const referenceRuns = Number(flags['reference-runs'] ?? 2);
  const rows = [];
  for (const task of tasks) {
    const work = join(CACHE, 'work', `validate-${task.id}`);
    const logFile = join(CACHE, 'logs', `validate-${task.id}.log`);
    mkdirSync(dirname(logFile), { recursive: true });
    writeFileSync(logFile, '');
    process.stdout.write(`${task.id}: base… `);
    const base = prepare(task, task.base, work);
    restoreHidden(task, work);
    const atBase = await runChecks(task, work, base.env, logFile);
    process.stdout.write('reference… ');
    const ref = prepare(task, task.reference, work);
    restoreHidden(task, work);
    const atReference = [];
    for (let i = 0; i < referenceRuns; i++)
      atReference.push(await runChecks(task, work, ref.env, logFile));
    rmSync(work, { recursive: true, force: true });
    const v = validation(atBase, atReference);
    writeJson(expectedPath(task), {
      id: task.id,
      reference: task.reference,
      ok: v.ok,
      problems: v.problems,
      failingAtBase: v.failingAtBase,
      flaky: v.flaky,
      required: v.required,
    });
    console.log(
      v.ok
        ? `ok (${v.failingAtBase.length} of ${v.required.length} required tests fail at the base)`
        : `NOT USABLE: ${v.problems.join('; ')} (log: ${logFile})`
    );
    rows.push({ id: task.id, ...v });
  }
  const bad = rows.filter((r) => !r.ok);
  console.log(`\n${rows.length - bad.length} of ${rows.length} tasks usable.`);
  if (bad.length) process.exitCode = 1;
}

// -------------------------------------------------------------------- runs

function runnerPath(flags) {
  if (flags.runner) return resolve(flags.runner);
  const bin = join(ROOT, 'src-tauri', 'target', 'debug', 'rovik-headless');
  if (!flags['no-build']) {
    console.log('Building the headless runner…');
    sh('cargo', ['build', '-p', 'rovik-headless'], {
      cwd: join(ROOT, 'src-tauri'),
      stdio: ['ignore', 'ignore', 'inherit'],
    });
  }
  if (!existsSync(bin)) throw new Error(`no runner at ${bin}`);
  return bin;
}

/** One run of one task: the agent, then the hidden tests. Resumable: a finished run is kept. */
async function runOne(task, rep, campaignDir, opts) {
  const dir = join(campaignDir, task.id, String(rep));
  const resultFile = join(dir, 'result.json');
  if (existsSync(resultFile)) return { ...readJson(resultFile), reused: true };
  const expected = expectedOf(task);
  if (!expected)
    throw new Error(`${task.id} isn't validated: run \`eval.mjs validate --tasks ${task.id}\``);
  mkdirSync(dir, { recursive: true });
  const ws = join(CACHE, 'work', workspaceName(campaignDir, task.id, rep));
  const { env, caches, start } = prepare(task, task.base, ws);
  const report = join(dir, 'report.json');
  // G03: an Autopilot variant's caps are its own (`autopilotCaps`).
  const caps = opts.perRun ?? BUDGET.perRun;
  const args = [
    '--project',
    ws,
    '--settings',
    opts.settings,
    '--policy',
    opts.policy,
    '--prompt',
    task.request,
    '--out',
    report,
    '--data',
    join(dir, 'data'),
    '--timeout',
    String(caps.timeoutSeconds),
    '--max-tokens',
    String(caps.maxTokens),
    // G03: `--mode autopilot --effort …` for an Autopilot variant.
    ...(opts.args ?? []),
  ];
  const started = Date.now();
  const r = await run(opts.runner, args, {
    env: { ...env, ...(opts.env ?? {}) },
    cwd: ws,
    timeoutMs: (caps.timeoutSeconds + 300) * 1000,
    logFile: join(dir, 'runner.log'),
  });
  const rep_ = existsSync(report) ? readJson(report) : null;
  sh('git', ['-C', ws, 'add', '-A']);
  writeFileSync(join(dir, 'changes.diff'), sh('git', ['-C', ws, 'diff', '--cached', start]).stdout);
  restoreHidden(task, ws);
  const results = await runChecks(task, ws, env, join(dir, 'checks.log'));
  rmSync(ws, { recursive: true, force: true });
  const touched = changedCaches(caches);
  for (const c of touched)
    console.warn(`warning: ${task.id} #${rep} changed the shared cache ${c}; later runs see it.`);
  const g = grade(expected.required, results);
  const result = {
    task: task.id,
    rep,
    success: g.success,
    outcome: rep_?.outcome ?? (r.timedOut ? 'runner-timeout' : 'runner-failed'),
    costUsd: rep_?.costUsd ?? null,
    costSource: rep_?.costSource ?? 'unknown',
    tokens: rep_?.usage?.totalTokens ?? 0,
    peakLeadContext: rep_?.usage?.peakLeadContext ?? 0,
    ms: rep_?.ms ?? Date.now() - started,
    interventions: rep_?.interventions ?? 0,
    toolCalls: rep_?.toolCalls ?? 0,
    // G02: how the run used its tools (its subagents' included), its
    // checks' rounds, and an Autopilot task's stages.
    toolErrors: rep_?.toolErrors ?? 0,
    toolErrorClasses: rep_?.toolErrorClasses ?? {},
    editMisses: rep_?.editMisses ?? 0,
    verifyRounds: rep_?.verifyRounds ?? 0,
    mode: rep_?.mode ?? null,
    effort: rep_?.effort ?? null,
    stages: Array.isArray(rep_?.stages)
      ? rep_.stages.map(({ name, status, round, outcome }) => ({ name, status, round, outcome }))
      : null,
    grade: g,
    ...(touched.length ? { changedCaches: touched } : {}),
  };
  writeJson(join(dir, 'grade.json'), { ...g, results });
  writeJson(resultFile, result);
  return result;
}

/**
 * Permissions every evaluation run needs: any command (the tasks' own test
 * runners, pytest and go included, aren't on the app's default list), and
 * no pause every 50 tool calls (a headless run would end there; the token
 * and time caps bound a run instead).
 */
const EVAL_PERMISSIONS = { allowAnyCommand: true, toolCallCheckpoint: null };

function settingsFor(flags) {
  if (!flags.settings) throw new Error('--settings FILE is required (see settings.example.json)');
  const path = resolve(flags.settings);
  const s = readJson(path);
  const price = s.price ?? {};
  if (!(price.inputPerMillion > 0 && price.outputPerMillion > 0)) {
    throw new Error(
      `${path}: set "price": {"inputPerMillion", "outputPerMillion"} (US dollars) so every run's ` +
        `cost is known; the provider's own figure is used when it reports one`
    );
  }
  const permissions = s.config?.permissions ?? {};
  for (const [key, value] of Object.entries(EVAL_PERMISSIONS)) {
    if (permissions[key] !== value) {
      throw new Error(`${path}: set config.permissions.${key} to ${value} (see README.md)`);
    }
  }
  if (s.apiKeyEnv && !process.env[s.apiKeyEnv]) {
    throw new Error(`the environment variable ${s.apiKeyEnv} (the API key) isn't set`);
  }
  return { path, price };
}

/**
 * Runs a campaign's runs in order, within its budget cap. `variant`: an
 * Autopilot variant's caps a run and the runner's flags (G03).
 */
async function campaign(name, tasks, repeats, cap, flags, variant = {}) {
  const { path: settings, price } = settingsFor(flags);
  const runner = runnerPath(flags);
  const policy = resolve(flags.policy ?? join(EVAL, 'policy.json'));
  const campaignDir = resolve(flags.out ?? join(RESULTS, name));
  const unlock = lockCampaign(campaignDir);
  try {
    return await campaignIn(name, tasks, repeats, cap, campaignDir, {
      settings,
      price,
      policy,
      runner,
      ...variant,
    });
  } finally {
    unlock();
  }
}

async function campaignIn(
  name,
  tasks,
  repeats,
  cap,
  campaignDir,
  { settings, price, policy, runner, perRun, args }
) {
  // The first run's estimate: every token it may use at the input price.
  const caps = perRun ?? BUDGET.perRun;
  const firstEstimate = (caps.maxTokens / 1e6) * price.inputPerMillion;
  const plan = [];
  for (let rep = 1; rep <= repeats; rep++)
    for (const task of tasks)
      plan.push({
        task,
        rep,
        done: existsSync(join(campaignDir, task.id, String(rep), 'result.json')),
      });
  const { results, stopped } = await runCampaign(plan, {
    cap,
    firstEstimate,
    runOne: async ({ task, rep }) => {
      process.stdout.write(`[${name}] ${task.id} #${rep}: `);
      const r = await runOne(task, rep, campaignDir, { settings, policy, runner, perRun, args });
      const cost = typeof r.costUsd === 'number' ? `$${r.costUsd.toFixed(3)}` : 'cost unknown';
      console.log(
        `${r.success ? 'PASS' : 'fail'} (${r.outcome}, ${r.grade.passed}/${r.grade.required} tests, ` +
          `${cost}, ${(r.tokens / 1000).toFixed(0)}k tokens)` +
          (r.reused ? ' [kept from before]' : '')
      );
      return r;
    },
  });
  const s = summarize(results);
  writeJson(join(campaignDir, 'summary.json'), { ...s, stopped: stopped || null, cap });
  writeFileSync(join(campaignDir, 'summary.md'), summaryMarkdown(s, `Evaluation: ${name}`));
  console.log(`\n${summaryMarkdown(s, `Evaluation: ${name}`)}`);
  if (stopped) {
    console.log(`Stopped: ${stopped}`);
    process.exitCode = 1;
  }
  return { results, campaignDir };
}

function usableTasks(only) {
  const tasks = loadTasks(only);
  const unvalidated = tasks.filter((t) => !expectedOf(t));
  if (unvalidated.length) {
    throw new Error(
      `not validated (or not usable): ${unvalidated.map((t) => t.id).join(', ')}; run \`eval.mjs validate\``
    );
  }
  return tasks;
}

async function pilot(flags) {
  const p = BUDGET.pilot;
  settingsFor(flags);
  const tasks = usableTasks(flags.tasks ?? p.tasks.join(','));
  await campaign('pilot', tasks, p.repeats, p.capUsd, flags);
}

async function baseline(flags) {
  const b = BUDGET.baseline;
  const { path } = settingsFor(flags);
  if (subagentsOn(readJson(path)))
    throw new Error(`${path}: the baseline runs with every subagent off (G02)`);
  const tasks = usableTasks(flags.tasks);
  const repeats = Number(flags.repeats ?? b.repeats);
  const pilotDir = resolve(flags.pilot ?? join(RESULTS, 'pilot'));
  const pilotFile = join(pilotDir, 'summary.json');
  if (!existsSync(pilotFile))
    throw new Error(`no pilot results in ${pilotDir}: run \`eval.mjs pilot\` first`);
  const pilotCosts = campaignResults(pilotDir).map((r) => r.costUsd ?? null);
  const proj = projection(pilotCosts, tasks.length * repeats, b.capUsd);
  if (!proj.ok && !proj.perRun) throw new Error(`not starting: ${proj.reason}`);
  console.log(
    `Pilot: $${proj.perRun.toFixed(3)} a run; ${tasks.length} tasks × ${repeats} = ` +
      `${tasks.length * repeats} runs ≈ $${proj.total.toFixed(2)} (cap $${b.capUsd}).`
  );
  if (!proj.ok) throw new Error(`not starting: ${proj.reason}`);
  await campaign(flags.name ?? 'baseline', tasks, repeats, b.capUsd, flags);
}

/** A campaign's results, from its directory (<task>/<repetition>/result.json). */
function campaignResults(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .flatMap((d) =>
      readdirSync(join(dir, d.name))
        .map((rep) => join(dir, d.name, rep, 'result.json'))
        .filter(existsSync)
        .map(readJson)
    );
}

/** A run's settings on the fake model (fake-model.mjs) at `url`: no checks, no checklist. */
const fakeSettings = (url) => ({
  profile: {
    id: 'fake::model',
    label: 'Fake/model',
    model: 'fake-model',
    baseUrl: url,
    stream: true,
    contextTokens: 200000,
  },
  mode: 'agent',
  price: { inputPerMillion: 1, outputPerMillion: 10 },
  config: { permissions: { ...EVAL_PERMISSIONS, verifyChanges: false, checklist: 'off' } },
});

// ------------------------------------------------- G02 and G03: variants

const kindsWanted = (flags) => {
  const ids = flags.kinds ? String(flags.kinds).split(',') : MEASURED_KINDS.map((k) => k.id);
  const unknown = ids.filter((id) => !MEASURED_KINDS.some((k) => k.id === id));
  if (unknown.length)
    throw new Error(
      `not measured by this task set: ${unknown.join(', ')} (measured: ${MEASURED_KINDS.map((k) => k.id).join(', ')})`
    );
  return MEASURED_KINDS.filter((k) => ids.includes(k.id));
};

/**
 * Every variant, each its own campaign: each measured kind on by itself
 * (G02, `kind-<id>`) and Autopilot at each effort (G03, `autopilot-<effort>`).
 */
const ALL_VARIANTS = [
  ...MEASURED_KINDS.map((k) => ({ ...k, campaign: `kind-${k.id}`, kind: k.id })),
  ...AUTOPILOT_VARIANTS.map((v) => ({ ...v, campaign: v.id })),
];

/** The variants asked for: `--variants` (campaign or kind ids), `--kinds` (kinds only), or all. */
function variantsWanted(flags) {
  if (flags.kinds && flags.variants) throw new Error('give --variants or --kinds, not both');
  if (flags.kinds) {
    const kinds = kindsWanted(flags).map((k) => k.id);
    return ALL_VARIANTS.filter((v) => kinds.includes(v.kind));
  }
  if (!flags.variants) return ALL_VARIANTS;
  const ids = String(flags.variants).split(',');
  const of = (id) => ALL_VARIANTS.find((v) => v.campaign === id || v.kind === id);
  const unknown = ids.filter((id) => !of(id));
  if (unknown.length)
    throw new Error(
      `unknown variants: ${unknown.join(', ')} (variants: ${ALL_VARIANTS.map((v) => v.campaign).join(', ')})`
    );
  return ALL_VARIANTS.filter((v) => ids.some((id) => of(id) === v));
}

/**
 * A variant's campaign, as `variants` and `dry-run-variants` run it: its
 * settings from the baseline's, its cost budget against the baseline, its
 * cap, and for Autopilot its caps a run and the runner's flags.
 */
function variantPlan(variant, base) {
  if (variant.kind) {
    const v = BUDGET.variants;
    return {
      settings: variantSettings(base, variant.kind),
      costRatioMax: v.costRatioMax[variant.kind] ?? v.costRatioMax.default,
      capUsd: v.capUsd,
      repeats: v.repeats,
      run: {},
    };
  }
  const effort = BUDGET.autopilot.efforts[variant.effort];
  return {
    settings: autopilotSettings(base, variant.effort, effort),
    costRatioMax: effort.costRatioMax,
    capUsd: effort.capUsd,
    repeats: BUDGET.autopilot.repeats,
    run: {
      perRun: autopilotCaps(effort),
      args: ['--mode', 'autopilot', '--effort', variant.effort],
    },
  };
}

/**
 * G02 and G03: each variant on the baseline's tasks, each its own campaign
 * within its cap: a kind on by itself, or Autopilot at an effort. A
 * campaign starts only if the baseline's cost a run, times the variant's
 * cost budget, times the runs, fits the cap.
 */
async function variants(flags) {
  const { path } = settingsFor(flags);
  const base = readJson(path);
  if (subagentsOn(base))
    throw new Error(
      `${path}: give the baseline's settings (subagents off); each variant turns one kind on`
    );
  const baseDir = resolve(flags.baseline ?? join(RESULTS, 'baseline'));
  const baseResults = campaignResults(baseDir);
  if (!baseResults.length)
    throw new Error(`no baseline results in ${baseDir}: run \`eval.mjs run\` first`);
  const baseTasks = [...new Set(baseResults.map((r) => r.task))].join(',');
  const tasks = usableTasks(flags.tasks ?? baseTasks);
  const baseSummary = summarize(baseResults);
  // P6.6: a budget from unknown costs would be made up.
  if (baseSummary.unknownCosts || baseSummary.meanCostUsd === null)
    throw new Error(
      `${baseSummary.unknownCosts} baseline run(s) have no known cost: variants can't be budgeted`
    );
  const perRun = baseSummary.meanCostUsd;
  for (const variant of variantsWanted(flags)) {
    const name = variant.campaign;
    const dir = resolve(join(RESULTS, name));
    const settings = join(dir, 'settings.json');
    const plan = variantPlan(variant, base);
    writeJson(settings, plan.settings);
    const repeats = Number(flags.repeats ?? plan.repeats);
    const budget = plan.costRatioMax;
    const proj = projection([perRun * budget], tasks.length * repeats, plan.capUsd);
    console.log(
      `\n${variant.label}: up to $${(perRun * budget).toFixed(3)} a run (${budget}× the baseline's ` +
        `$${perRun.toFixed(3)}); ${tasks.length} tasks × ${repeats} ≈ $${proj.total.toFixed(2)} (cap $${plan.capUsd}).`
    );
    if (!proj.ok) {
      console.log(`Skipped: ${proj.reason}. Run fewer tasks (--tasks) or repetitions (--repeats).`);
      process.exitCode = 1;
      continue;
    }
    await campaign(name, tasks, repeats, plan.capUsd, { ...flags, settings, out: dir }, plan.run);
  }
}

/** A campaign's settings (its settings.json), if it kept them. */
const settingsOf = (d) => {
  const f = join(d, 'settings.json');
  return existsSync(f) ? readJson(f) : null;
};

/** Why a campaign stopped early, from its summary; null if it didn't. */
const stoppedOf = (d) => {
  const f = join(d, 'summary.json');
  return existsSync(f) ? (readJson(f).stopped ?? null) : 'no summary: it never finished';
};

/**
 * G02/G03: every kind's campaign against the baseline, over the tasks both
 * ran; the decision for each; the results file and its data. With two
 * campaigns named (G03 in the Autopilot plan), the second against the
 * first instead (`compareTwo`).
 */
function compareAll(flags) {
  const named = (flags._ ?? []).slice(1);
  if (named.length === 2) return compareTwo(named[0], named[1], flags);
  if (named.length) throw new Error('compare takes two campaigns, or none');
  const dir = resolve(flags.results ?? RESULTS);
  const baseDir = join(dir, 'baseline');
  const baseResults = campaignResults(baseDir);
  if (!baseResults.length) throw new Error(`no baseline results in ${baseDir}`);
  const v = BUDGET.variants;
  const rows = [];
  let model = null;
  for (const kind of MEASURED_KINDS) {
    const kdir = join(dir, `kind-${kind.id}`);
    const results = campaignResults(kdir);
    if (!results.length) continue;
    model = model ?? settingsOf(kdir)?.profile?.label ?? null;
    const common = new Set(results.map((r) => r.task));
    const baseCommon = baseResults.filter((r) => common.has(r.task));
    const baseTasks = new Set(baseCommon.map((r) => r.task));
    const variant = summarize(results.filter((r) => baseTasks.has(r.task)));
    const decision = compareVariant(summarize(baseCommon), variant, {
      costRatioMax: v.costRatioMax[kind.id] ?? v.costRatioMax.default,
      stopped: stoppedOf(kdir),
    });
    rows.push({ ...kind, decision });
  }
  if (!rows.length)
    throw new Error(`no kind's results in ${dir} (kind-<id>/): run \`eval.mjs variants\``);
  const fake = /fake/i.test(model ?? '') || !!flags.fake;
  const comparison = {
    date: new Date().toISOString().slice(0, 10),
    model: model ?? 'unknown',
    fake,
    costRatioMax: v.costRatioMax.default,
    tasks: [...new Set(baseResults.map((r) => r.task))],
    baseline: summarize(baseResults),
    variants: rows,
    unmeasured: UNMEASURED_KINDS,
    on: rows.filter((r) => r.decision.on).map((r) => r.id),
  };
  const out = resolve(flags.out ?? join(ROOT, 'docs/design/subagents-results.md'));
  writeFileSync(out, resultsMarkdown(comparison));
  writeJson(join(dir, 'comparison.json'), comparison);
  console.log(resultsMarkdown(comparison));
  console.log(`Written: ${out} and ${join(dir, 'comparison.json')}`);
  return comparison;
}

/** A campaign named on the command line: a folder, or a campaign of the results folder. */
function campaignDir(arg, flags) {
  const dir = resolve(flags.results ?? RESULTS);
  return arg.includes('/') || arg.startsWith('.') ? resolve(arg) : join(dir, arg);
}

/**
 * G03 (autopilot-plan): any campaign against any other, over the tasks
 * both ran: every number side by side, pass@k and pass^k, and whether the
 * second is better beyond the noise within its cost budget (a variant's
 * own, or `--cost-ratio`; none for other campaigns). Writes
 * `compare-<a>-<b>.md` and `.json` in the results folder, or `--out`.
 */
function compareTwo(aArg, bArg, flags) {
  const [aDir, bDir] = [campaignDir(aArg, flags), campaignDir(bArg, flags)];
  const [aAll, bAll] = [campaignResults(aDir), campaignResults(bDir)];
  if (!aAll.length) throw new Error(`no results in ${aDir}`);
  if (!bAll.length) throw new Error(`no results in ${bDir}`);
  const common = new Set(aAll.map((r) => r.task).filter((t) => bAll.some((r) => r.task === t)));
  if (!common.size) throw new Error(`${aDir} and ${bDir} have no task in common`);
  const [a, b] = [aAll, bAll].map((all) => summarize(all.filter((r) => common.has(r.task))));
  const [aName, bName] = [basename(aDir), basename(bDir)];
  const variant = ALL_VARIANTS.find((v) => v.campaign === bName);
  const costRatioMax = flags['cost-ratio']
    ? Number(flags['cost-ratio'])
    : variant
      ? variantPlan(variant, {}).costRatioMax
      : null;
  const decision = compareCampaigns(a, b, {
    costRatioMax,
    stopped: stoppedOf(aDir) ?? stoppedOf(bDir),
    minTasks: Number(flags['min-tasks'] ?? 5),
  });
  const model = settingsOf(bDir)?.profile?.label ?? settingsOf(aDir)?.profile?.label ?? 'unknown';
  const comparison = {
    date: new Date().toISOString().slice(0, 10),
    model,
    fake: /fake/i.test(model) || !!flags.fake,
    a: { name: aName, summary: a },
    b: { name: bName, summary: b },
    decision,
  };
  const out = resolve(flags.out ?? join(dirname(bDir), `compare-${aName}-${bName}.md`));
  writeFileSync(out, comparisonMarkdown(comparison));
  writeJson(out.replace(/\.md$/, '') + '.json', comparison);
  console.log(comparisonMarkdown(comparison));
  console.log(`Written: ${out}`);
  return comparison;
}

/** G03: the decision into the app's defaults (src/lib/subagentSettings.ts). */
function applyDefaultsCommand(flags) {
  const file = resolve(flags.comparison ?? join(RESULTS, 'comparison.json'));
  if (!existsSync(file)) throw new Error(`no ${file}: run \`eval.mjs compare\` first`);
  const c = readJson(file);
  if (c.fake) throw new Error('these results come from a scripted model: they decide nothing');
  if (!c.on.length) {
    console.log('No kind goes on by default: the defaults stay as they are.');
    return;
  }
  const target = resolve(flags.target ?? join(ROOT, 'src/lib/subagentSettings.ts'));
  writeFileSync(target, applyDefaults(readFileSync(target, 'utf8'), c.on));
  console.log(
    `On by default now: ${c.on.join(', ')}, with the master switch (${target}). ` +
      'Update the defaults test in subagentSettings.test.ts, and link docs/design/subagents-results.md in the PR.'
  );
}

/**
 * G02's and G03's pipeline without a provider: the baseline and each
 * variant (`--variants`: some of them), once per task, with the scripted
 * model that makes the reference change (for Autopilot, its own mode:
 * the task's plan, each stage's answer, and the change as writes); then
 * the comparisons, into a scratch directory. Every run must pass, and the
 * results must be marked as a dry run.
 */
async function dryRunVariants(flags) {
  const tasks = usableTasks(flags.tasks ?? 'mi-subfactorial,lo-filter-map');
  const runner = runnerPath(flags);
  const policy = join(EVAL, 'policy.json');
  const dir = resolve(flags.out ?? join(RESULTS, `dry-run-variants-${Date.now()}`));
  const wanted = variantsWanted(flags);
  let bad = 0;
  for (const variant of [null, ...wanted]) {
    const name = variant?.campaign ?? 'baseline';
    const autopilot = !!variant && !variant.kind;
    for (const task of tasks) {
      const model = await fakeModel(
        autopilot ? ['autopilot', referenceFiles(task)] : ['oracle', referencePatch(task)]
      );
      const settings = join(dir, name, `settings-${task.id}.json`);
      const base = fakeSettings(model.url);
      const plan = variant
        ? variantPlan(variant, base)
        : { settings: variantSettings(base, null), run: {} };
      writeJson(settings, plan.settings);
      writeJson(join(dir, name, 'settings.json'), { ...plan.settings, profile: base.profile });
      try {
        const r = await runOne(task, 1, join(dir, name), {
          settings,
          policy,
          runner,
          ...plan.run,
        });
        const stages = r.stages ?? [];
        const whole = !autopilot || reachedDelivery(stages);
        if (!r.success || !whole) bad++;
        const failing = autopilot
          ? failingChecks(join(dir, name, task.id, '1', 'report.json'))
          : [];
        console.log(
          `${name} ${task.id}: ${r.success && whole ? 'pass' : 'FAIL'} (${r.outcome}, ` +
            `$${(r.costUsd ?? 0).toFixed(4)}, ${r.toolErrors} tool errors)` +
            (autopilot ? ` — ${stages.map((st) => `${st.name} ${st.status}`).join(', ')}` : '') +
            (failing.length ? `; the project's own checks failed: ${failing.join(', ')}` : '')
        );
      } finally {
        model.stop();
      }
    }
    writeJson(join(dir, name, 'summary.json'), { stopped: null });
  }
  const comparisons = [];
  if (wanted.some((v) => v.kind))
    comparisons.push(
      compareAll({ results: dir, out: join(dir, 'subagents-results.md'), fake: true })
    );
  for (const v of wanted.filter((v) => !v.kind))
    comparisons.push(compareTwo('baseline', v.campaign, { results: dir, 'min-tasks': 1 }));
  bad += comparisons.filter((c) => !c.fake).length;
  console.log(bad ? `\n${bad} unexpected results.` : '\nThe G02/G03 pipeline works.');
  if (bad) process.exitCode = 1;
}

/**
 * Whether an Autopilot task ran to its delivery with none of the stages
 * the scripted model answers failed. Its checks are the project's own: a
 * lint that fails with or without the change (`go vet` on lo's tests,
 * `ruff` on more-itertools) fails them, the task ends there, and the
 * delivery says so.
 */
function reachedDelivery(stages) {
  const last = stages.at(-1);
  const theProjects = ['checks', 'deliver'];
  return (
    last?.name === 'deliver' &&
    last.status !== 'running' &&
    !stages.some((st) => st.status === 'failed' && !theProjects.includes(st.name))
  );
}

/** The checks that failed in a run's last round, by command (its report's run log). */
function failingChecks(reportFile) {
  if (!existsSync(reportFile)) return [];
  const rounds = (readJson(reportFile).runLog ?? []).filter((l) => l.event === 'verify');
  return (rounds.at(-1)?.checks ?? [])
    .filter((c) => c.status === 'failed')
    .map((c) => `\`${c.command}\``);
}

/**
 * The reference's version of each file its change touches, the hidden
 * tests aside, as JSON (path -> content): what Autopilot's scripted model
 * writes. A file the reference deletes isn't in it (a write can't delete).
 */
function referenceFiles(task) {
  const repo = repoOf(task);
  const exclude = task.hidden.map((f) => `:(exclude)${f}`);
  const changed = sh('git', [
    '-C',
    repo,
    'diff',
    '--name-status',
    '--no-renames',
    task.base,
    task.reference,
    '--',
    '.',
    ...exclude,
  ]);
  const files = {};
  for (const line of changed.stdout.split('\n').filter(Boolean)) {
    const [status, path] = line.split('\t');
    if (status === 'D') continue;
    files[path] = sh('git', ['-C', repo, 'show', `${task.reference}:${path}`]).stdout;
  }
  if (!Object.keys(files).length)
    throw new Error(`${task.id}: its reference writes no file, only deletes: no Autopilot dry run`);
  const file = join(CACHE, 'patches', `${task.id}.files.json`);
  writeJson(file, files);
  return file;
}

// ----------------------------------------------------------------- dry run

/** The reference change without the hidden tests, as a patch. */
function referencePatch(task) {
  const repo = repoOf(task);
  const exclude = task.hidden.map((f) => `:(exclude)${f}`);
  const diff = sh('git', [
    '-C',
    repo,
    'diff',
    '--binary',
    task.base,
    task.reference,
    '--',
    '.',
    ...exclude,
  ]);
  const file = join(CACHE, 'patches', `${task.id}.patch`);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, diff.stdout);
  return file;
}

/** Starts fake-model.mjs; resolves to its URL and a stop function. */
function fakeModel(args) {
  return startServer(join(EVAL, 'fake-model.mjs'), args);
}

/**
 * The pipeline end to end without a provider: each task once with a model
 * that applies the reference change (must pass) and once with one that
 * changes nothing (must fail).
 */
async function dryRun(flags) {
  const defaults = ['self-chat-title-markers', 'mi-subfactorial', 'lo-filter-map'];
  const tasks = usableTasks(flags.tasks ?? defaults.join(','));
  const runner = runnerPath(flags);
  const policy = join(EVAL, 'policy.json');
  const campaignDir = resolve(flags.out ?? join(RESULTS, `dry-run-${Date.now()}`));
  const rows = [];
  for (const task of tasks) {
    for (const mode of ['oracle', 'idle']) {
      const model = await fakeModel(mode === 'oracle' ? [mode, referencePatch(task)] : [mode]);
      const settings = join(campaignDir, `settings-${task.id}-${mode}.json`);
      writeJson(settings, fakeSettings(model.url));
      try {
        const r = await runOne(task, mode === 'oracle' ? 1 : 2, join(campaignDir, mode), {
          settings,
          policy,
          runner,
        });
        const want = mode === 'oracle';
        rows.push({ task: task.id, mode, ok: r.success === want, r });
        console.log(
          `${task.id} (${mode}): ${r.success ? 'pass' : 'fail'}, ${r.outcome}, ` +
            `${r.grade.passed}/${r.grade.required} tests, $${(r.costUsd ?? 0).toFixed(4)} ` +
            `(${r.costSource}) — ${r.success === want ? 'as expected' : 'NOT AS EXPECTED'}`
        );
      } finally {
        model.stop();
      }
    }
  }
  const bad = rows.filter((r) => !r.ok);
  console.log(bad.length ? `\n${bad.length} unexpected results.` : '\nThe pipeline works.');
  if (bad.length) process.exitCode = 1;
}

// ------------------------------------------------------------------ report

function report(flags) {
  const dir = resolve(flags._[1] ?? join(RESULTS, 'baseline'));
  if (!existsSync(dir)) throw new Error(`no results in ${dir}`);
  const s = summarize(campaignResults(dir));
  writeJson(join(dir, 'summary.json'), s);
  writeFileSync(join(dir, 'summary.md'), summaryMarkdown(s, `Evaluation: ${dir}`));
  console.log(summaryMarkdown(s, `Evaluation: ${dir}`));
}

// -------------------------------------------------------------------- main

const flags = parseFlags(process.argv.slice(2));
const commands = {
  validate,
  'dry-run': dryRun,
  pilot,
  run: baseline,
  report,
  variants,
  compare: compareAll,
  'apply-defaults': applyDefaultsCommand,
  'dry-run-variants': dryRunVariants,
};
const command = commands[flags._[0]];
if (!command) {
  console.error(
    'usage: eval.mjs validate [--tasks a,b] | dry-run [--tasks a,b] | pilot --settings FILE | ' +
      'run --settings FILE [--repeats N] [--tasks a,b] | report [DIR] | ' +
      'variants --settings FILE [--kinds a,b | --variants a,b] | compare [A B] [--out FILE] | apply-defaults | ' +
      'dry-run-variants [--variants a,b]'
  );
  process.exit(2);
}
try {
  await command(flags);
} catch (e) {
  console.error(`eval: ${e.message}`);
  process.exitCode = 1;
}
