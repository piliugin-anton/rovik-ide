#!/usr/bin/env node
// The agent evaluation (G01 in docs/design/subagents-plan.md): runs the
// headless runner (src-tauri/src/headless.rs) on the tasks in tasks/ and
// grades each run with the tasks' hidden tests. See README.md.
//
//   node scripts/eval/eval.mjs validate [--tasks a,b]
//   node scripts/eval/eval.mjs dry-run [--tasks a,b]
//   node scripts/eval/eval.mjs pilot --settings FILE
//   node scripts/eval/eval.mjs run --settings FILE
//   node scripts/eval/eval.mjs report DIR
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
  appendFileSync,
  copyFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  budgetAllows,
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

/** Runs a command to its end without blocking the event loop (for long runs). */
function run(cmd, args, { env = {}, cwd, timeoutMs, logFile } = {}) {
  return new Promise((done) => {
    const child = spawn(cmd, args, {
      cwd,
      env: { ...process.env, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: true,
    });
    let out = '';
    const take = (c) => {
      out += c;
      if (logFile) appendFileSync(logFile, c);
    };
    child.stdout.on('data', take);
    child.stderr.on('data', take);
    let timedOut = false;
    const timer = timeoutMs
      ? setTimeout(() => {
          timedOut = true;
          try {
            process.kill(-child.pid, 'SIGKILL');
          } catch {
            // gone already
          }
        }, timeoutMs)
      : null;
    child.on('close', (status) => {
      if (timer) clearTimeout(timer);
      done({ status, out, timedOut });
    });
  });
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
  for (const s of task.setup ?? []) {
    if (s === 'node') {
      const nm = nodeModules(ws);
      if (nm) {
        symlinkSync(nm, join(ws, 'node_modules'), 'dir');
        exclude.push('node_modules');
      }
    } else if (s === 'cargo') {
      env.CARGO_TARGET_DIR = join(CACHE, 'cargo-target', repoName(task));
      // Incremental caches of many old commits fill a disk quickly.
      env.CARGO_INCREMENTAL = '0';
    } else if (s === 'python') {
      const venv = pythonVenv();
      env.VIRTUAL_ENV = venv;
      env.PATH = `${join(venv, 'bin')}:${process.env.PATH}`;
    } else if (s === 'go') {
      env.GOFLAGS = '-mod=mod';
    }
  }
  if (exclude.length)
    appendFileSync(join(ws, '.git', 'info', 'exclude'), exclude.join('\n') + '\n');
  return { env, start: git('rev-parse', 'HEAD').stdout.trim() };
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
    sh('cargo', ['build', '--features', 'headless', '--bin', 'rovik-headless'], {
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
  const ws = join(CACHE, 'work', `run-${task.id}-${rep}`);
  const { env, start } = prepare(task, task.base, ws);
  const report = join(dir, 'report.json');
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
    String(BUDGET.perRun.timeoutSeconds),
    '--max-tokens',
    String(BUDGET.perRun.maxTokens),
  ];
  const started = Date.now();
  const r = await run(opts.runner, args, {
    env: { ...env, ...(opts.env ?? {}) },
    cwd: ws,
    timeoutMs: (BUDGET.perRun.timeoutSeconds + 300) * 1000,
    logFile: join(dir, 'runner.log'),
  });
  const rep_ = existsSync(report) ? readJson(report) : null;
  sh('git', ['-C', ws, 'add', '-A']);
  writeFileSync(join(dir, 'changes.diff'), sh('git', ['-C', ws, 'diff', '--cached', start]).stdout);
  restoreHidden(task, ws);
  const results = await runChecks(task, ws, env, join(dir, 'checks.log'));
  rmSync(ws, { recursive: true, force: true });
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
    grade: g,
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

/** Runs a campaign's runs in order, within its budget cap. */
async function campaign(name, tasks, repeats, cap, flags) {
  const { path: settings, price } = settingsFor(flags);
  const runner = runnerPath(flags);
  const policy = resolve(flags.policy ?? join(EVAL, 'policy.json'));
  const campaignDir = resolve(flags.out ?? join(RESULTS, name));
  // The first run's estimate: every token it may use at the input price.
  const firstEstimate = (BUDGET.perRun.maxTokens / 1e6) * price.inputPerMillion;
  const results = [];
  let stopped = '';
  outer: for (let rep = 1; rep <= repeats; rep++) {
    for (const task of tasks) {
      const done = existsSync(join(campaignDir, task.id, String(rep), 'result.json'));
      if (!done) {
        const spent = results.reduce((a, r) => a + (r.costUsd ?? 0), 0);
        const allowed = budgetAllows({
          cap,
          spent,
          costs: results.map((r) => r.costUsd ?? 0),
          firstEstimate,
        });
        if (!allowed.ok) {
          stopped = allowed.reason;
          break outer;
        }
      }
      process.stdout.write(`[${name}] ${task.id} #${rep}: `);
      const r = await runOne(task, rep, campaignDir, { settings, policy, runner });
      results.push(r);
      console.log(
        `${r.success ? 'PASS' : 'fail'} (${r.outcome}, ${r.grade.passed}/${r.grade.required} tests, ` +
          `$${(r.costUsd ?? 0).toFixed(3)}, ${(r.tokens / 1000).toFixed(0)}k tokens)` +
          (r.reused ? ' [kept from before]' : '')
      );
      const failing = results.slice(-3);
      if (
        failing.length === 3 &&
        failing.every((f) => ['error', 'runner-failed'].includes(f.outcome))
      ) {
        stopped = 'three runs in a row ended in an error: is the provider (or the runner) working?';
        break outer;
      }
      if (r.costUsd === null) {
        stopped = `${task.id} #${rep}: its cost is unknown (no usage reported); stopping`;
        break outer;
      }
    }
  }
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
  settingsFor(flags);
  const tasks = usableTasks(flags.tasks);
  const repeats = Number(flags.repeats ?? b.repeats);
  const pilotDir = resolve(flags.pilot ?? join(RESULTS, 'pilot'));
  const pilotFile = join(pilotDir, 'summary.json');
  if (!existsSync(pilotFile))
    throw new Error(`no pilot results in ${pilotDir}: run \`eval.mjs pilot\` first`);
  const pilotCosts = readdirSync(pilotDir, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .flatMap((d) =>
      readdirSync(join(pilotDir, d.name))
        .map((rep) => join(pilotDir, d.name, rep, 'result.json'))
        .filter(existsSync)
        .map((f) => readJson(f).costUsd ?? 0)
    );
  const proj = projection(pilotCosts, tasks.length * repeats, b.capUsd);
  console.log(
    `Pilot: $${proj.perRun.toFixed(3)} a run; ${tasks.length} tasks × ${repeats} = ` +
      `${tasks.length * repeats} runs ≈ $${proj.total.toFixed(2)} (cap $${b.capUsd}).`
  );
  if (!proj.ok) throw new Error(`not starting: ${proj.reason}`);
  await campaign(flags.name ?? 'baseline', tasks, repeats, b.capUsd, flags);
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
  return new Promise((done, fail) => {
    const child = spawn(process.execPath, [join(EVAL, 'fake-model.mjs'), ...args], {
      stdio: ['ignore', 'pipe', 'inherit'],
    });
    child.stdout.once('data', (c) => done({ url: String(c).trim(), stop: () => child.kill() }));
    child.on('error', fail);
  });
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
      writeJson(settings, {
        profile: {
          id: 'fake::model',
          label: 'Fake/model',
          model: 'fake-model',
          baseUrl: model.url,
          stream: true,
          contextTokens: 200000,
        },
        mode: 'agent',
        price: { inputPerMillion: 1, outputPerMillion: 10 },
        config: { permissions: { ...EVAL_PERMISSIONS, verifyChanges: false, checklist: 'off' } },
      });
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
  const results = readdirSync(dir, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .flatMap((d) =>
      readdirSync(join(dir, d.name))
        .map((rep) => join(dir, d.name, rep, 'result.json'))
        .filter(existsSync)
        .map(readJson)
    );
  const s = summarize(results);
  writeJson(join(dir, 'summary.json'), s);
  writeFileSync(join(dir, 'summary.md'), summaryMarkdown(s, `Evaluation: ${dir}`));
  console.log(summaryMarkdown(s, `Evaluation: ${dir}`));
}

// -------------------------------------------------------------------- main

const flags = parseFlags(process.argv.slice(2));
const commands = { validate, 'dry-run': dryRun, pilot, run: baseline, report };
const command = commands[flags._[0]];
if (!command) {
  console.error(
    'usage: eval.mjs validate [--tasks a,b] | dry-run [--tasks a,b] | pilot --settings FILE | ' +
      'run --settings FILE [--repeats N] [--tasks a,b] | report [DIR]'
  );
  process.exit(2);
}
try {
  await command(flags);
} catch (e) {
  console.error(`eval: ${e.message}`);
  process.exitCode = 1;
}
