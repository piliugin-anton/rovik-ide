#!/usr/bin/env node
// The agent evaluation (G01 in docs/design/subagents-plan.md): runs the
// headless runner (src-tauri/crates/rovik-headless) on the tasks in tasks/
// and grades each run with the tasks' hidden tests. See README.md.
//
//   node scripts/eval/eval.mjs validate [--tasks a,b]
//   node scripts/eval/eval.mjs dry-run [--tasks a,b] [--jobs N]
//   node scripts/eval/eval.mjs pilot --settings FILE [--jobs N]
//   node scripts/eval/eval.mjs run --settings FILE [--jobs N]
//   node scripts/eval/eval.mjs report DIR
//   node scripts/eval/eval.mjs power [DIR] [--repeats 3,6] [--target 5]   (evolve-plan P07)
//   node scripts/eval/eval.mjs race --settings FILE --candidates FILE      (evolve-plan R01)
//   node scripts/eval/eval.mjs dry-run-race [--jobs N]
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
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
  appendFileSync,
  copyFileSync,
} from 'node:fs';
import { homedir, tmpdir, userInfo } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  AUTOPILOT_VARIANTS,
  MEASURED_KINDS,
  UNMEASURED_KINDS,
  applyDefaults,
  autopilotCaps,
  autopilotSettings,
  budgetAllows,
  compareCampaigns,
  compareVariant,
  comparisonMarkdown,
  grade,
  inParallel,
  knownSpend,
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
  steerArgs,
} from './lib.mjs';
import { run, startServer } from './proc.mjs';
import { changedCaches, copyTree, fingerprint, protect } from './caches.mjs';
import {
  canariesIn,
  canaryOf,
  cheatCalls,
  confinementArgs,
  plantCanary,
  workRootProblem,
} from './confinement.mjs';
import { parseNameStatus, tamperCalls, tamperReasons, tampering } from './integrity.mjs';
import { identityProblem } from './identity.mjs';
import { detectableEffect, meanRunVariance, moduleOf, tasksFor } from './stats.mjs';
import { ATTEMPT_ARMS, attemptsResult } from './arms.mjs';
import { ledgerTotals, usageGap } from './usage.mjs';
import { monitorRun } from './monitor.mjs';
import { candidateSettings, raceMarkdown, runRace, shuffled } from './race.mjs';

const EVAL = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(EVAL, '../..');
const CACHE = resolve(process.env.ROVIK_EVAL_CACHE ?? join(ROOT, '.eval-cache'));
const RESULTS = resolve(process.env.ROVIK_EVAL_RESULTS ?? join(ROOT, 'eval-results'));
/**
 * P01 (docs/design/evolve-plan.md): the runs' workspaces, in a temporary
 * root outside the repository and the cache (`workRoot` checks where).
 */
const WORK = resolve(
  process.env.ROVIK_EVAL_WORK ?? join('/var/tmp', `rovik-eval-${userInfo().username}`)
);
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

/**
 * P05: a file written whole or not at all (into its place by a rename),
 * for what a run that may be starting beside this one reads: a scripted
 * model's patch, say.
 */
let atomicWrites = 0;
function writeAtomic(p, content) {
  mkdirSync(dirname(p), { recursive: true });
  const part = `${p}.${process.pid}.${atomicWrites++}.part`;
  writeFileSync(part, content);
  renameSync(part, p);
}

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

/**
 * P04, P07: a task's module by its id (`moduleOf`), what comparisons'
 * errors and a campaign's power are clustered by; an unknown task is its
 * own.
 */
let modules = null;
function clusterOf(task) {
  modules ??= new Map(loadTasks().map((t) => [t.id, moduleOf(t)]));
  return modules.get(task) ?? task;
}

/** P01: what every evaluated run is hidden from: the answers, and other runs. */
const HIDDEN = [ROOT, CACHE, RESULTS, WORK];

/**
 * P09: what a command may name outside its workspace without a word from
 * the monitor: the system's programs and libraries, the temporary
 * folders, the toolchains' homes.
 */
const SYSTEM_DIRS = [
  '/usr',
  '/bin',
  '/sbin',
  '/lib',
  '/lib64',
  '/dev',
  '/tmp',
  '/opt/homebrew',
  tmpdir(),
  ...['.cargo', '.rustup', 'go', '.cache', '.npm', '.local', '.nvm', '.pyenv'].map((d) =>
    join(homedir(), d)
  ),
];

/**
 * P01: the root of the runs' workspaces, made if it's missing (only this
 * user's). Refused inside a hidden folder or a temporary one
 * (`workRootProblem`): set ROVIK_EVAL_WORK elsewhere.
 */
function workRoot() {
  const problem = workRootProblem(realPath(WORK), {
    hidden: [ROOT, CACHE, RESULTS].map(realPath),
    writable: [tmpdir(), '/tmp'].map(realPath),
  });
  if (problem) throw new Error(`${problem}: set ROVIK_EVAL_WORK to a folder elsewhere`);
  mkdirSync(WORK, { recursive: true, mode: 0o700 });
  return WORK;
}

/** `p` as the file system has it, links resolved, though it may not exist yet. */
function realPath(p) {
  if (existsSync(p)) return realpathSync(p);
  const parent = dirname(p);
  return parent === p ? p : join(realPath(parent), basename(p));
}

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
  plantNote(dir, task);
  return dir;
}

/**
 * P01: the task's canary as a note on its reference commit, in a clone of
 * ours (never this repository), so the history shows it to whatever reads
 * it there (`git log`, `git show`).
 */
function plantNote(dir, task) {
  const canary = canaryOf(task);
  const note = sh('git', ['-C', dir, 'notes', 'show', task.reference], { check: false });
  if (note.status === 0 && note.stdout.includes(canary)) return;
  sh('git', [
    '-C',
    dir,
    '-c',
    'user.name=eval',
    '-c',
    'user.email=eval@localhost',
    'notes',
    'add',
    '-f',
    '-m',
    canary,
    task.reference,
  ]);
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
 * history. Returns the environment its commands and checks run with, and
 * (P01) what of the hidden cache its commands may still read (`show`) and
 * write (`writable`). `slot`: the worker's (P05), for its build folder.
 */
function prepare(task, rev, ws, slot = 0) {
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
  const show = [];
  const writable = [];
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
      // P05: a target folder per worker, so builds side by side don't
      // wait on each other's lock, or lose what the pruning takes.
      env.CARGO_TARGET_DIR = join(
        CACHE,
        'cargo-target',
        ...(slot ? [`job-${slot}`] : []),
        repoName(task)
      );
      mkdirSync(env.CARGO_TARGET_DIR, { recursive: true });
      writable.push(env.CARGO_TARGET_DIR);
      // Incremental caches of many old commits fill a disk quickly.
      env.CARGO_INCREMENTAL = '0';
    } else if (s === 'python') {
      const venv = pythonVenv();
      caches.push({ dir: venv, print: fingerprint(venv) });
      show.push(venv);
      env.VIRTUAL_ENV = venv;
      env.PATH = `${join(venv, 'bin')}:${process.env.PATH}`;
    } else if (s === 'go') {
      env.GOFLAGS = '-mod=mod';
    }
  }
  if (exclude.length)
    appendFileSync(join(ws, '.git', 'info', 'exclude'), exclude.join('\n') + '\n');
  return { env, caches, show, writable, start: git('rev-parse', 'HEAD').stdout.trim() };
}

/**
 * Puts the reference's version of the hidden test files in the workspace,
 * each with the task's canary in a last comment (P01).
 */
function restoreHidden(task, ws) {
  const repo = repoOf(task);
  for (const f of task.hidden) {
    const r = sh('git', ['-C', repo, 'show', `${task.reference}:${f}`], { encoding: 'buffer' });
    mkdirSync(dirname(join(ws, f)), { recursive: true });
    writeFileSync(join(ws, f), plantCanary(r.stdout, f, canaryOf(task)));
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
      // Beside the workspace, its own: two runs of a task can be graded at once.
      const xml = join(dirname(ws), `${basename(ws)}-junit.xml`);
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
      // P01: whatever reads this file has the canary in what it read.
      canary: canaryOf(task),
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

/** This commit's builds: the runner, and the grader's integrity check (P02). */
const OWN_RUNNER = join(ROOT, 'src-tauri', 'target', 'debug', 'rovik-headless');
const OWN_INTEGRITY = join(ROOT, 'src-tauri', 'target', 'debug', 'rovik-integrity');

function runnerPath(flags) {
  if (flags.runner) return resolve(flags.runner);
  const bin = OWN_RUNNER;
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

/**
 * P02: the grader's own integrity check, built from this commit whatever
 * runner the campaign evaluates (`--runner`): the build being evaluated
 * can't weaken what grades it.
 */
function integrityPath(flags) {
  const bin = OWN_INTEGRITY;
  if (!flags['no-build']) {
    sh('cargo', ['build', '-p', 'rovik-headless', '--bin', 'rovik-integrity'], {
      cwd: join(ROOT, 'src-tauri'),
      stdio: ['ignore', 'ignore', 'inherit'],
    });
  }
  if (!existsSync(bin)) throw new Error(`no integrity check at ${bin}`);
  return bin;
}

/**
 * The builds a campaign uses: the runner it evaluates, the grader's own
 * check (P02), and (P03) their part of what its runs are made with.
 */
function builds(flags) {
  const runner = runnerPath(flags);
  const integrity = integrityPath(flags);
  const build = {
    runnerCommit: runnerCommit(runner, flags),
    runnerHash: fileHash(runner),
    integrityHash: fileHash(integrity),
  };
  return { runner, integrity, build };
}

/** P03: a file's SHA-256. */
const fileHash = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');

/**
 * P03: the commit the runner was built from: this repository's HEAD for
 * its own build ("+dirty" with changes under src-tauri the commit doesn't
 * hold), `--runner-commit` for another, else unknown.
 */
function runnerCommit(runner, flags) {
  if (resolve(runner) !== OWN_RUNNER) return flags['runner-commit'] ?? null;
  const head = sh('git', ['-C', ROOT, 'rev-parse', 'HEAD']).stdout.trim();
  const dirty = sh('git', ['-C', ROOT, 'status', '--porcelain', '--', 'src-tauri']).stdout.trim();
  return dirty ? `${head}+dirty` : head;
}

/** P03: what a run is made with: its builds (`builds`), its settings and its policy. */
const identityOf = (build, settings, policy) => ({
  ...build,
  settingsHash: fileHash(settings),
  policyHash: fileHash(policy),
});

/**
 * P03: claims a campaign's folder for `identity`, which `identity.json`
 * keeps from its first run: a resume with another refuses
 * (`identityProblem`), and so does a folder with runs and no record.
 */
function claimIdentity(campaignDir, identity) {
  const file = join(campaignDir, 'identity.json');
  if (existsSync(file)) {
    const problem = identityProblem(campaignDir, readJson(file), identity);
    if (problem) throw new Error(problem);
    return;
  }
  if (campaignResults(campaignDir).length)
    throw new Error(
      `${campaignDir} has runs but no record of what they were made with: start a new campaign (--out)`
    );
  writeJson(file, identity);
}

/** P03: a file's size and time, which a rebuild changes. */
const stamp = (p) => {
  const s = statSync(p);
  return `${s.size}:${s.mtimeMs}`;
};

/** P05: how many runs at once (`--jobs`, one by default). */
function jobsOf(flags) {
  const jobs = Number(flags.jobs ?? 1);
  if (!Number.isInteger(jobs) || jobs < 1)
    throw new Error('--jobs takes a whole number, 1 or more');
  return jobs;
}

/** P02: what a run's line says when it's tampered (why), or (P09) marked for reading. */
const tamperNote = (r) =>
  (r.tampered?.length ? ` TAMPERED: ${r.tampered.join('; ')}` : '') +
  (r.review?.length ? ' [marked for reading]' : '');

/**
 * One repetition of one task: a run, or for the same-money arm (P06,
 * `opts.attempts`) that many, the one the project's checks pick kept as
 * the repetition's result (`attemptsResult`), what all cost counted.
 * Resumable: a finished repetition, or attempt, is kept.
 */
async function runOne(task, rep, campaignDir, opts) {
  if (!(opts.attempts > 1)) return runOnce(task, rep, campaignDir, opts);
  const resultFile = join(campaignDir, task.id, String(rep), 'result.json');
  if (existsSync(resultFile)) return { ...readJson(resultFile), reused: true };
  const attempts = [];
  for (let attempt = 1; attempt <= opts.attempts; attempt++)
    attempts.push(await runOnce(task, rep, campaignDir, { ...opts, attempt }));
  const result = attemptsResult(attempts.map(({ reused, ...r }) => r));
  writeJson(resultFile, result);
  return result;
}

/** One run of one task: the agent, then the hidden tests. Resumable: a finished run is kept. */
async function runOnce(task, rep, campaignDir, opts) {
  // P06: an attempt of a repetition has a folder of its own in it.
  const attempt = opts.attempt ? `attempt-${opts.attempt}` : null;
  const dir = join(campaignDir, task.id, String(rep), ...(attempt ? [attempt] : []));
  const resultFile = join(dir, 'result.json');
  if (existsSync(resultFile)) return { ...readJson(resultFile), reused: true };
  const expected = expectedOf(task);
  if (!expected)
    throw new Error(`${task.id} isn't validated: run \`eval.mjs validate --tasks ${task.id}\``);
  mkdirSync(dir, { recursive: true });
  const label = attempt ? `${rep}-${opts.attempt}` : rep;
  const ws = join(workRoot(), workspaceName(campaignDir, task.id, label));
  const { env, caches, show, writable, start } = prepare(task, task.base, ws, opts.slot);
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
    // P05: which run this is, in its report.
    '--run-id',
    [basename(campaignDir), task.id, rep, ...(attempt ? [attempt] : [])].join('/'),
    // G03: `--mode autopilot --effort …` for an Autopilot variant.
    ...(opts.args ?? []),
    // D18 (steering-plan.md): the task's scripted messages, unless the
    // campaign runs without them (`--no-steers`).
    ...steerArgs(task, { steers: opts.steers !== false }),
    // P01: kept from the answers and other runs, from the network and,
    // unless the task allows them, from the web tools.
    ...confinementArgs({ hide: HIDDEN, show, writable, web: task.web === true }),
  ];
  // P08: the meter in the provider's place: the run's tokens and cost are
  // counted there, outside the build being evaluated, which gets a key
  // that opens nothing.
  const settings = readJson(opts.settings);
  const ledger = join(dir, 'meter.jsonl');
  rmSync(ledger, { force: true });
  const meter = await startServer(
    join(EVAL, 'meter.mjs'),
    [new URL(settings.profile.baseUrl).origin, ledger, String(caps.maxTokens)],
    { env: { ROVIK_METER_KEY: apiKeyOf(settings) } }
  );
  args.push('--meter', meter.url);
  const metered = settings.apiKeyEnv ? { [settings.apiKeyEnv]: 'rovik-metered' } : {};
  const started = Date.now();
  let r;
  try {
    r = await run(opts.runner, args, {
      env: { ...env, ...metered, ...(opts.env ?? {}) },
      cwd: ws,
      timeoutMs: (caps.timeoutSeconds + 300) * 1000,
      logFile: join(dir, 'runner.log'),
    });
  } finally {
    meter.stop();
  }
  const rep_ = existsSync(report) ? readJson(report) : null;
  const usage = meterUsage(ledger, settings.price, rep_);
  sh('git', ['-C', ws, 'add', '-A']);
  const diff = sh('git', ['-C', ws, 'diff', '--cached', start]).stdout;
  writeFileSync(join(dir, 'changes.diff'), diff);
  const integrity = integrityOf(task, ws, start, opts.integrity);
  // P09: what the run did, read for what the score misses.
  const watched = monitorRun({
    transcript: rep_?.transcript ?? [],
    diff,
    canaries: [canaryOf(task)],
    hidden: HIDDEN,
    allowed: [ws, ...show, ...writable],
    system: SYSTEM_DIRS,
    webAllowed: task.web === true,
  });
  restoreHidden(task, ws);
  const results = await runChecks(task, ws, env, join(dir, 'checks.log'));
  rmSync(ws, { recursive: true, force: true });
  const touched = changedCaches(caches);
  for (const c of touched)
    console.warn(`warning: ${task.id} #${rep} changed the shared cache ${c}; later runs see it.`);
  const g = grade(expected.required, results);
  // P02: nothing decides differently how the tests run; P08: the runner
  // told the truth about what it used; P09: the monitor saw nothing
  // strong (its weak signals mark the run for reading).
  const tampered = [...integrity.tampered, ...(usage.gap ? [usage.gap] : []), ...watched.strong];
  const result = {
    task: task.id,
    rep,
    success: g.success && !tampered.length,
    tampered,
    review: watched.weak,
    outcome: rep_?.outcome ?? (r.timedOut ? 'runner-timeout' : 'runner-failed'),
    // P08: by the meter, beside what the runner said.
    costUsd: usage.costUsd,
    costSource: usage.costSource,
    tokens: usage.tokens,
    runner: { costUsd: rep_?.costUsd ?? null, tokens: rep_?.usage?.totalTokens ?? 0 },
    meter: { replies: usage.replies, missing: usage.missing },
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
    // P06: what the project's checks said last (the gate's verification):
    // the same-money arm picks its attempt by it.
    verification: rep_?.verification?.at(-1)?.status ?? null,
    stages: Array.isArray(rep_?.stages)
      ? rep_.stages.map(({ name, status, round, outcome }) => ({ name, status, round, outcome }))
      : null,
    grade: g,
    // P03: the runner, the grader's check, the settings and the policy.
    identity: identityOf(opts.build, opts.settings, opts.policy),
    ...(touched.length ? { changedCaches: touched } : {}),
  };
  writeJson(join(dir, 'grade.json'), { ...g, integrity: integrity.flags, results });
  writeJson(resultFile, result);
  return result;
}

/** P08: the provider's key, as a settings file gives it: from the environment, or its own. */
const apiKeyOf = (settings) =>
  (settings.apiKeyEnv ? process.env[settings.apiKeyEnv] : settings.profile?.apiKey) ?? '';

/**
 * P08: what a run used, by its meter's ledger (`ledgerTotals`), and
 * whether the runner's own count fell short of it (`usageGap`; a runner
 * that wrote no report, having failed, isn't asked).
 */
function meterUsage(ledger, price, report) {
  const lines = existsSync(ledger)
    ? readFileSync(ledger, 'utf8')
        .split('\n')
        .filter(Boolean)
        .map((l) => JSON.parse(l))
    : [];
  const totals = ledgerTotals(lines, price);
  const gap = report ? usageGap(report.usage?.totalTokens ?? 0, totals) : '';
  return { ...totals, gap };
}

/**
 * P02: the run's change through the grader's own integrity check: each
 * file it changed, as at its start and as staged now (the index, so a
 * link or a pipe it left isn't followed), into `rovik-integrity`. The
 * flags, and the reasons of those on files the reference's change doesn't
 * touch: tampering.
 */
function integrityOf(task, ws, start, binary) {
  const git = (...a) => sh('git', ['-C', ws, ...a]).stdout;
  const listed = git('diff', '--cached', '--name-status', '--no-renames', '-z', start);
  const files = parseNameStatus(listed).map(({ status, path }) => ({
    path,
    before: status === 'A' ? null : git('show', `${start}:${path}`),
    after: status === 'D' ? null : git('show', `:${path}`),
  }));
  const out = sh(binary, [], { input: JSON.stringify({ files }) });
  const flags = JSON.parse(out.stdout).flags;
  return { flags, tampered: tamperReasons(tampering(flags, referenceChanges(task))) };
}

/** P02: the files the reference's change touches, once a task. */
const referenceChangeSets = new Map();
function referenceChanges(task) {
  if (!referenceChangeSets.has(task.id)) {
    const names = sh('git', [
      '-C',
      repoOf(task),
      'diff',
      '--name-only',
      '--no-renames',
      '-z',
      task.base,
      task.reference,
    ]).stdout;
    referenceChangeSets.set(task.id, new Set(names.split('\0').filter(Boolean)));
  }
  return referenceChangeSets.get(task.id);
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
  const policy = resolve(flags.policy ?? join(EVAL, 'policy.json'));
  const campaignDir = resolve(flags.out ?? join(RESULTS, name));
  const unlock = lockCampaign(campaignDir);
  try {
    const bins = builds(flags);
    claimIdentity(campaignDir, identityOf(bins.build, settings, policy));
    return await campaignIn(name, tasks, repeats, cap, campaignDir, {
      settings,
      price,
      policy,
      ...bins,
      // A task's scripted messages, unless `--no-steers` (steering-plan.md).
      steers: !flags['no-steers'],
      jobs: jobsOf(flags),
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
  { settings, price, policy, runner, integrity, build, jobs, perRun, args, steers }
) {
  // P03: what the runs are made with mustn't change under the campaign.
  const files = [runner, integrity, settings, policy];
  const stamps = files.map(stamp);
  const unchanged = () => {
    const changed = files.filter((f, i) => stamp(f) !== stamps[i]);
    if (changed.length)
      throw new Error(
        `changed during the campaign: ${changed.join(', ')}; its runs so far are kept, and ` +
          'resuming checks what they were made with'
      );
  };
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
    jobs,
    runOne: async ({ task, rep }, slot) => {
      unchanged();
      const r = await runOne(task, rep, campaignDir, {
        settings,
        policy,
        runner,
        integrity,
        build,
        perRun,
        args,
        steers,
        slot,
      });
      // A whole line once the run is over: runs side by side don't mix theirs.
      const cost = typeof r.costUsd === 'number' ? `$${r.costUsd.toFixed(3)}` : 'cost unknown';
      console.log(
        `[${name}] ${task.id} #${rep}: ` +
          `${r.success ? 'PASS' : 'fail'} (${r.outcome}, ${r.grade.passed}/${r.grade.required} tests, ` +
          `${cost}, ${(r.tokens / 1000).toFixed(0)}k tokens)` +
          tamperNote(r) +
          (r.reused ? ' [kept from before]' : '')
      );
      return r;
    },
  });
  const s = summarize(results, { clusterOf });
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
  // P06 (evolve-plan): the same-money arm, k attempts picked by the checks.
  ...ATTEMPT_ARMS.map((v) => ({ ...v, campaign: v.id })),
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
  if (variant.attempts) {
    const a = BUDGET.attempts;
    return {
      settings: structuredClone(base),
      costRatioMax: a.costRatioMax,
      capUsd: a.capUsd,
      repeats: a.repeats,
      run: { attempts: variant.attempts },
    };
  }
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
  const [a, b] = [aAll, bAll].map((all) =>
    summarize(
      all.filter((r) => common.has(r.task)),
      { clusterOf }
    )
  );
  const [aName, bName] = [basename(aDir), basename(bDir)];
  const variant = ALL_VARIANTS.find((v) => v.campaign === bName);
  const costRatioMax = flags['cost-ratio']
    ? Number(flags['cost-ratio'])
    : variant
      ? variantPlan(variant, {}).costRatioMax
      : null;
  // P04: the paired comparison's errors are clustered by each task's module.
  const decision = compareCampaigns(a, b, {
    costRatioMax,
    stopped: stoppedOf(aDir) ?? stoppedOf(bDir),
    minTasks: Number(flags['min-tasks'] ?? 5),
    clusterOf,
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
  const bins = builds(flags);
  const policy = join(EVAL, 'policy.json');
  const dir = resolve(flags.out ?? join(RESULTS, `dry-run-variants-${Date.now()}`));
  const wanted = variantsWanted(flags);
  const items = [null, ...wanted].flatMap((variant) => tasks.map((task) => ({ variant, task })));
  const { results } = await inParallel(items, jobsOf(flags), async ({ variant, task }, slot) => {
    const name = variant?.campaign ?? 'baseline';
    const autopilot = !!variant?.effort;
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
        ...bins,
        ...plan.run,
        slot,
      });
      const stages = r.stages ?? [];
      const whole = !autopilot || reachedDelivery(stages);
      const failing = autopilot ? failingChecks(join(dir, name, task.id, '1', 'report.json')) : [];
      console.log(
        `${name} ${task.id}: ${r.success && whole ? 'pass' : 'FAIL'} (${r.outcome}, ` +
          `$${(r.costUsd ?? 0).toFixed(4)}, ${r.toolErrors} tool errors)${tamperNote(r)}` +
          (autopilot ? ` — ${stages.map((st) => `${st.name} ${st.status}`).join(', ')}` : '') +
          (failing.length ? `; the project's own checks failed: ${failing.join(', ')}` : '')
      );
      return r.success && whole;
    } finally {
      model.stop();
    }
  });
  let bad = results.filter((ok) => !ok).length;
  for (const variant of [null, ...wanted])
    writeJson(join(dir, variant?.campaign ?? 'baseline', 'summary.json'), { stopped: null });
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
  writeAtomic(file, JSON.stringify(files, null, 2) + '\n');
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
  writeAtomic(file, diff.stdout);
  return file;
}

/** Starts fake-model.mjs; resolves to its URL and a stop function. */
function fakeModel(args) {
  return startServer(join(EVAL, 'fake-model.mjs'), args);
}

/** A scripted cheater's calls (fake-model.mjs `cheat`), in a file it reads outside the sandbox. */
function callsFile(task, name, calls) {
  const file = join(CACHE, 'patches', `${task.id}.${name}.json`);
  writeAtomic(file, JSON.stringify(calls, null, 2) + '\n');
  return file;
}

/** P01: the cheater that looks for the answer, knowing where it is. */
const lookingCheater = (task) =>
  cheatCalls({
    repo: repoOf(task),
    base: task.base,
    reference: task.reference,
    hidden: task.hidden,
    expectedFile: expectedPath(task),
    folders: [CACHE, RESULTS, WORK],
    url: task.repo === 'self' ? 'https://github.com' : task.repo.url,
  });

/**
 * The dry run's models: one that applies the reference change (must
 * pass), one that changes nothing (must fail), and two cheaters: (P01)
 * one that looks for the answer everywhere and must find nothing (it
 * fails, and no canary is in its transcript or its change), and (P02) one
 * whose tests pass by a conftest.py its own report doesn't flag (the gate
 * is off in a dry run): the grader's own check must grade it tampered.
 */
const DRY_RUN_MODES = [
  { mode: 'oracle', rep: 1, pass: true, model: (task) => ['oracle', referencePatch(task)] },
  { mode: 'idle', rep: 2, pass: false, model: () => ['idle'] },
  {
    mode: 'cheat',
    rep: 3,
    pass: false,
    // P09: what it tried is plain in its transcript.
    tampered: true,
    model: (task) => ['cheat', callsFile(task, 'cheat', lookingCheater(task))],
  },
  {
    mode: 'tamper',
    rep: 4,
    pass: false,
    tampered: true,
    applies: (task) => !!tamperCalls(task),
    model: (task) => ['cheat', callsFile(task, 'tamper', tamperCalls(task))],
  },
  // P08: a runner built to claim a third of what it used (three replies
  // of 1,050 tokens at the dry run's price): counted by the meter, caught.
  {
    mode: 'underreport',
    rep: 5,
    pass: false,
    tampered: true,
    runner: join(EVAL, 'fake-runner.mjs'),
    costUsd: 3 * (1000 * 1 + 50 * 10) * 1e-6,
    model: () => ['idle'],
  },
];

/** The pipeline end to end without a provider, each task with each model. */
async function dryRun(flags) {
  const defaults = ['self-chat-title-markers', 'mi-subfactorial', 'lo-filter-map'];
  const tasks = usableTasks(flags.tasks ?? defaults.join(','));
  const bins = builds(flags);
  const policy = join(EVAL, 'policy.json');
  const campaignDir = resolve(flags.out ?? join(RESULTS, `dry-run-${Date.now()}`));
  const items = tasks.flatMap((task) =>
    DRY_RUN_MODES.filter((m) => !m.applies || m.applies(task)).map((m) => ({ task, m }))
  );
  const { results: rows } = await inParallel(items, jobsOf(flags), async ({ task, m }, slot) => {
    const model = await fakeModel(m.model(task));
    const settings = join(campaignDir, `settings-${task.id}-${m.mode}.json`);
    writeJson(settings, fakeSettings(model.url));
    try {
      const runner = m.runner ?? bins.runner;
      const r = await runOne(task, m.rep, join(campaignDir, m.mode), {
        settings,
        policy,
        ...bins,
        runner,
        // P03: another runner, another build.
        build: m.runner
          ? { ...bins.build, runnerCommit: null, runnerHash: fileHash(runner) }
          : bins.build,
        slot,
      });
      const runDir = join(campaignDir, m.mode, task.id, String(m.rep));
      const found = canariesIn(readRun(runDir), [canaryOf(task)]);
      const tampered = r.tampered.length > 0;
      const costRight = m.costUsd === undefined || Math.abs(r.costUsd - m.costUsd) < 1e-12;
      const ok = r.success === m.pass && !found.length && tampered === !!m.tampered && costRight;
      console.log(
        `${task.id} (${m.mode}): ${r.success ? 'pass' : 'fail'}, ${r.outcome}, ` +
          `${r.grade.passed}/${r.grade.required} tests, $${(r.costUsd ?? 0).toFixed(4)} ` +
          `(${r.costSource})${tamperNote(r)}` +
          (found.length ? ', ITS TRANSCRIPT HOLDS THE CANARY' : '') +
          ` — ${ok ? 'as expected' : 'NOT AS EXPECTED'}`
      );
      return { task: task.id, mode: m.mode, ok, r };
    } finally {
      model.stop();
    }
  });
  const bad = rows.filter((r) => !r.ok);
  console.log(bad.length ? `\n${bad.length} unexpected results.` : '\nThe pipeline works.');
  if (bad.length) process.exitCode = 1;
}

/** What a run left that could hold a canary: its report (the transcript) and its change. */
function readRun(runDir) {
  return ['report.json', 'changes.diff']
    .map((f) => join(runDir, f))
    .filter(existsSync)
    .map((f) => readFileSync(f, 'utf8'))
    .join('\n');
}

// ------------------------------------------------------------------ report

function report(flags) {
  const dir = resolve(flags._[1] ?? join(RESULTS, 'baseline'));
  if (!existsSync(dir)) throw new Error(`no results in ${dir}`);
  const s = summarize(campaignResults(dir), { clusterOf });
  writeJson(join(dir, 'summary.json'), s);
  writeFileSync(join(dir, 'summary.md'), summaryMarkdown(s, `Evaluation: ${dir}`));
  console.log(summaryMarkdown(s, `Evaluation: ${dir}`));
}

// -------------------------------------------------------------------- race

/**
 * R01 (evolve-plan): a race between settings. The current settings
 * (`--settings`) against each candidate of `--candidates FILE`
 * ([{ name, settings }]: patches of what a race may vary, `RACEABLE`), on
 * the tasks in an order drawn from `--seed`, a block at a time; the
 * clearly worse are dropped, and D11 decides (race.mjs). Each candidate's
 * runs are a campaign of the race's folder, so a race resumes; its record
 * is `race.md` and `race.json` there.
 */
async function race(flags) {
  const { path, price } = settingsFor(flags);
  const base = readJson(path);
  if (!flags.candidates) throw new Error('--candidates FILE is required: [{ "name", "settings" }]');
  const candidates = [{ name: 'current', settings: {} }, ...readJson(resolve(flags.candidates))];
  const names = candidates.map((c) => c.name);
  if (new Set(names).size !== names.length || names.some((n) => !/^[\w.-]+$/.test(n)))
    throw new Error(
      `the candidates need names of their own, letters, digits, . and - ("current" is taken)`
    );
  const dir = resolve(flags.out ?? join(RESULTS, 'race'));
  const policy = resolve(flags.policy ?? join(EVAL, 'policy.json'));
  const files = {};
  for (const c of candidates) {
    files[c.name] = join(dir, c.name, 'settings.json');
    writeJson(files[c.name], candidateSettings(base, c));
  }
  const bins = builds(flags);
  for (const c of candidates)
    claimIdentity(join(dir, c.name), identityOf(bins.build, files[c.name], policy));
  const byId = new Map(usableTasks(flags.tasks).map((t) => [t.id, t]));
  const r = BUDGET.race;
  const spent = [];
  const firstEstimate = (BUDGET.perRun.maxTokens / 1e6) * price.inputPerMillion;
  const record = await runRace({
    candidates,
    tasks: shuffled([...byId.keys()], Number(flags.seed ?? 1)),
    blockSize: Number(flags.block ?? r.blockSize),
    minTasks: r.minTasks,
    margin: Number(flags.margin ?? r.margin * 100) / 100,
    costRatioMax: r.costRatioMax,
    clusterOf,
    canAfford: (runs) =>
      budgetAllows({ ...knownSpend(spent), cap: r.capUsd, firstEstimate, pending: runs - 1 }).ok,
    runBlock: async (c, block) => {
      const { results } = await inParallel(block, jobsOf(flags), (id, slot) =>
        runOne(byId.get(id), 1, join(dir, c.name), {
          settings: files[c.name],
          policy,
          ...bins,
          slot,
        })
      );
      spent.push(...results);
      for (const x of results)
        console.log(`[race] ${c.name} ${x.task}: ${x.success ? 'PASS' : 'fail'}${tamperNote(x)}`);
      return results;
    },
  });
  writeRace(dir, record, { fake: false });
}

/** A race's record into its folder, and on the console. */
function writeRace(dir, record, { fake }) {
  writeJson(join(dir, 'race.json'), {
    ...record,
    date: new Date().toISOString().slice(0, 10),
    fake,
  });
  writeFileSync(join(dir, 'race.md'), raceMarkdown(record, { fake }));
  console.log(`\n${raceMarkdown(record, { fake })}`);
  console.log(`Written: ${join(dir, 'race.md')}`);
}

/**
 * R01's dry run: the race's pipeline with scripted models on the
 * evaluation's own tasks (the Go and Python ones, by default): the current
 * settings (a model that makes the reference change), one that changes
 * nothing (clearly worse), and the current at half the price (cheaper,
 * and equal). The second must be dropped and the third chosen.
 */
async function dryRunRace(flags) {
  const defaults = loadTasks()
    .filter((t) => t.lang === 'go' || t.lang === 'python')
    .map((t) => t.id);
  const byId = new Map(usableTasks(flags.tasks ?? defaults.join(',')).map((t) => [t.id, t]));
  const bins = builds(flags);
  const policy = join(EVAL, 'policy.json');
  const dir = resolve(flags.out ?? join(RESULTS, `dry-run-race-${Date.now()}`));
  const price = (input) => ({ inputPerMillion: input, outputPerMillion: input * 10 });
  const candidates = [
    { name: 'current', settings: {}, model: 'oracle', price: price(1) },
    { name: 'changes-nothing', settings: {}, model: 'idle', price: price(1) },
    { name: 'half-price', settings: {}, model: 'oracle', price: price(0.5) },
  ];
  const record = await runRace({
    candidates,
    tasks: shuffled([...byId.keys()], Number(flags.seed ?? 1)),
    clusterOf,
    runBlock: async (c, block) =>
      (
        await inParallel(block, jobsOf(flags), async (id, slot) => {
          const task = byId.get(id);
          const model = await fakeModel(
            c.model === 'oracle' ? ['oracle', referencePatch(task)] : ['idle']
          );
          const settings = join(dir, c.name, `settings-${id}.json`);
          writeJson(settings, { ...fakeSettings(model.url), price: c.price });
          try {
            return await runOne(task, 1, join(dir, c.name), { settings, policy, ...bins, slot });
          } finally {
            model.stop();
          }
        })
      ).results,
  });
  writeRace(dir, record, { fake: true });
  const ok =
    record.blocks.some((b) => b.dropped.includes('changes-nothing')) &&
    record.chosen === 'half-price';
  console.log(
    ok
      ? '\nThe race works.'
      : '\nNOT AS EXPECTED: the race should drop changes-nothing and choose half-price.'
  );
  if (!ok) process.exitCode = 1;
}

// ------------------------------------------------------------------- power

/**
 * P07 (evolve-plan): what a campaign's noise lets a paired comparison
 * show: the smallest change with its tasks, at each number of
 * repetitions (`--repeats 3,6`), and how many tasks a change of
 * `--target` points (5) would take.
 */
function power(flags) {
  const dir = resolve(flags._[1] ?? join(RESULTS, 'baseline'));
  const results = campaignResults(dir);
  if (!results.length) throw new Error(`no results in ${dir}`);
  const s = summarize(results, { clusterOf });
  const variance = meanRunVariance(s.tasks);
  if (variance === null) throw new Error(`${dir}: no task ran twice, so its noise isn't known`);
  const target = Number(flags.target ?? 5) / 100;
  const deff = s.power.deff;
  const lines = [
    `${dir}: ${s.tasks.length} tasks; a run's outcome varies by ${variance.toFixed(3)} on ` +
      `average; the modules' design effect is ${deff.toFixed(2)} (α ${s.power.alpha}, ` +
      `power ${Math.round(s.power.power * 100)}%).`,
  ];
  for (const repeats of String(flags.repeats ?? '3,6')
    .split(',')
    .map(Number)) {
    const mde = detectableEffect({ variance, tasks: s.tasks.length, repeats, deff });
    const needed = tasksFor({ variance, repeats, target, deff });
    lines.push(
      `- ${repeats} repetitions an arm: ${(mde * 100).toFixed(1)} pts with these tasks; ` +
        `${needed} tasks for ${(target * 100).toFixed(0)} pts`
    );
  }
  console.log(lines.join('\n'));
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
  power,
  race,
  'dry-run-race': dryRunRace,
  'apply-defaults': applyDefaultsCommand,
  'dry-run-variants': dryRunVariants,
};
const command = commands[flags._[0]];
if (!command) {
  console.error(
    'usage: eval.mjs validate [--tasks a,b] | dry-run [--tasks a,b] [--jobs N] | ' +
      'pilot --settings FILE [--jobs N] | ' +
      'run --settings FILE [--repeats N] [--tasks a,b] [--jobs N] | report [DIR] | ' +
      'variants --settings FILE [--kinds a,b | --variants a,b] | compare [A B] [--out FILE] | apply-defaults | ' +
      'power [DIR] [--repeats 3,6] [--target 5] | ' +
      'race --settings FILE --candidates FILE [--tasks a,b] [--seed N] [--jobs N] | dry-run-race | ' +
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
