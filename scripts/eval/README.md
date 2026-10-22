# Agent evaluation

G01 in [docs/design/subagents-plan.md](../../docs/design/subagents-plan.md): the
main agent's baseline on a fixed task set, before subagents change anything. The
headless runner ([src-tauri/crates/rovik-headless](../../src-tauri/crates/rovik-headless))
runs each task as the app would; the task's hidden tests then grade it.

Linux and macOS only: the workspaces, caches and process handling use
POSIX tools (`cp`, `chmod`, `tar`, process groups).

## Tasks

30 tasks in `tasks/`, each a real change with tests, taken back to the commit
before it:

| Source                                                             | Tasks | Languages                                |
| ------------------------------------------------------------------ | ----- | ---------------------------------------- |
| this repository                                                    | 20    | TypeScript (9; one also Rust), Rust (11) |
| [more-itertools](https://github.com/more-itertools/more-itertools) | 5     | Python                                   |
| [samber/lo](https://github.com/samber/lo)                          | 5     | Go                                       |

A task file:

```json
{
  "id": "mi-subfactorial",
  "repo": { "url": "https://github.com/more-itertools/more-itertools", "name": "more-itertools" },
  "base": "<commit the agent starts from>",
  "reference": "<commit with the real change>",
  "lang": "python",
  "request": "What the agent is asked, naming the interfaces the tests call",
  "hidden": ["tests/test_more.py"],
  "checks": [{ "kind": "pytest", "files": ["tests/test_more.py"] }],
  "setup": ["python"]
}
```

`repo` is `"self"` for this repository. Check kinds: `cargo` (`dir`, `filters`:
test-name filters for `cargo test --lib`), `ts` (`files`, run by `ts-test.mjs` as
`npm test` runs them), `pytest` (`files`), `go` (`packages`). Setup: `node`
(node_modules from the base's lock file, cached), `cargo` (a shared target
directory), `python` (a virtualenv with pytest), `go`.

A task can also name `steers`: messages sent to the run while it works, as
the user would send them (docs/design/steering-plan.md), each once the chat has
`after` tool results:
`"steers": [{ "text": "The helper is in src/util.rs", "after": 3 }]`. They
go to the runner as `--steer TEXT@N`. `--no-steers` runs a campaign without
them, so a task can be compared with and without the fact given mid-run.

Contamination: the more-itertools and lo changes are public and older than
current models' training data, so a model may have seen them; this
repository's are newer. Read results per source as well as overall.

## Grading

SWE-bench style. The agent works in a fresh repository holding only the base
commit's files (no history, so the answer can't be read from it). After the
run, the reference's version of the `hidden` test files is put in the workspace
(over anything the agent did to them) and the checks run. A run succeeds when
every test the reference passes also passes, and its change didn't decide
how they ran (below).

The tests passing isn't enough (P02 in
[docs/design/evolve-plan.md](../../docs/design/evolve-plan.md)): the grader
also checks the run's change itself, with its own build of the checks
(`rovik-integrity`, built from this commit whatever `--runner` a campaign
evaluates), never with the run's report. Each file the run changed, as it
was and as it is, goes through the gate's integrity rules
(`repair::integrity_flags`: tests removed, skipped or emptied, assertions
dropped, what decides the checks changed) and stricter ones of its own
(`repair::grading_flags`): a new file that decides what the checks run (a
`conftest.py` with a hook, a `pytest.ini`), a Go `TestMain`, a Rust macro
named like an assertion, a `sitecustomize.py`. A flag on a file the
reference's change doesn't touch grades the run `tampered`: it fails,
whatever the tests said, and its result says why (`tampered`; the flags are
in `grade.json`). The dry run's fourth model, on a Python task, is a
cheater whose `conftest.py` reports every failed test as passed: its tests
pass, and it must be graded tampered.

`validate` decides which tests those are. It runs the checks at the base (with
the hidden tests) and at the reference, twice. Required tests are the ones that
pass at the reference both times. A test that passes only once is flaky and
left out. A task is usable when some required test fails at the base. The
result goes to `expected/<task>.json` and is committed with the task. Grading
uses it, and a task whose reference changed must be validated again.

## What a run is kept from

P01 in [docs/design/evolve-plan.md](../../docs/design/evolve-plan.md): a run
being evaluated mustn't reach its task's answer, or a search for a higher
score would find it.

- **Its workspace is elsewhere**: in `ROVIK_EVAL_WORK`, by default
  `/var/tmp/rovik-eval-<user>`, outside this repository and the caches. Not
  inside the system's temporary folders, which every command may write:
  Landlock can't hide a folder there without breaking what commands make
  beside it, so the evaluation refuses such a folder.
- **Its commands can't see the answers**: the runner hides this repository
  (its history, today's source, `expected/`, and by default the caches and
  results), the cache and results folders wherever they are, and the other
  runs' workspaces (`--hide`). The shared virtualenv stays readable
  (`--show`) and cargo's target folder writable (`--writable`).
- **No network**: its commands are offline (`--offline`). Under bubblewrap
  they get a network of their own with only its own loopback; under
  Landlock (version 4, Linux 6.7) no TCP at all, and under macOS's Seatbelt
  no IP, this machine's included (a proxy there would be the internet). A
  sandbox that can't, or none, refuses the run rather than run it
  unconfined.
- **No web tools** (`--no-web`), unless the task allows them
  (`"web": true` in its file).
- **Canaries**: each task has one (`canaryOf` in `confinement.mjs`), a
  last comment in the hidden tests the grader puts back, a field of
  `expected/<task>.json` and a git note on the reference commit in the
  clones. Whatever reads them has the canary in its transcript: the report
  holds the run's whole transcript, its subagents' included, and the
  sandbox it ran under.

**The run monitor** (P09, `monitor.mjs`) reads each run's transcript, its
subagents' included, and its change. Strong signals grade the run
`tampered` (each result says why): its task's canary anywhere in them; a
command or a tool's path naming a folder its sandbox hides (but for its
own workspace and what it may read or write in the cache); a try at the
network (`curl`, `wget`, `git clone` and the like, or the web tools when
the task doesn't allow them). Weak ones mark it for a person to read
(`review` in its result, counted in the summary): digging in git's
history (`git log --all`, `reflog`, `cat-file`, `notes`, an old commit
shown), installing packages, and paths outside the workspace other than
the system's, the temporary folders and the toolchains'. Only commands and
the tools' paths are read for those: code a run writes can look like a
path.

The dry run's third model is a cheater that knows where the answers are:
it reads the history and the real repository, `expected/`, the caches and
the other runs, browses the task's repository and fetches it with `curl`,
then applies the reference change if it found it. It must fail, with no
canary in its transcript or its change, and the monitor must grade it
`tampered` for what it tried.

## Budget

| Campaign                 | Runs                               | Cap                   |
| ------------------------ | ---------------------------------- | --------------------- |
| pilot                    | 5 tasks × 1 (`budget.json`)        | $5                    |
| baseline                 | 30 tasks × 3 repetitions = 90      | $75                   |
| each kind (G02), 4 kinds | 30 tasks × 3 repetitions = 90 each | $75 each, $300 in all |
| a race (R01)             | each candidate: the tasks × 1      | $300 in all           |

Every run is limited to 1.5 million tokens (the runner's `--max-tokens`: the
main agent's and its subagents' requests, as the provider reports them; the run
stops with outcome `budget`) and 30 minutes (outcome `timeout`).

- A campaign starts a run only while what it has spent, plus the mean cost of
  its runs so far, stays within its cap. Before its first run, the estimate is
  the worst case: every allowed token at the input price.
- `run` (the baseline) starts only if the pilot's mean cost a run, times the
  runs planned, fits the $75 cap. Otherwise, run fewer tasks (`--tasks`) or
  repetitions (`--repeats`).
- A run's tokens and cost are counted outside the runner (P08 in
  [docs/design/evolve-plan.md](../../docs/design/evolve-plan.md)): each
  run's requests go through a meter (`meter.mjs`, the runner's
  `--meter`), which passes them on to the provider and writes what each
  reply says it used to `meter.jsonl` in the run's folder. The runner gets
  a key that opens nothing (its `apiKeyEnv` set to a dummy); the meter
  holds the real one, so a runner that went around it would get no
  answer. Everything the runner does by its provider (its API, its cache
  hints) still goes by the provider's URL: only where requests are sent
  changes (`net::routed`). The meter also refuses requests once a run has
  used its tokens. A runner whose own count falls more than a tenth short
  of the meter's is graded `tampered` (the dry run's fifth model is such a
  runner, `fake-runner.mjs`); `result.json` keeps both counts (`runner`,
  `meter`). Node's `fetch` takes no proxy from the environment unless told
  (`NODE_USE_ENV_PROXY=1` where Node has it): a provider behind a proxy
  needs it.
- A run's cost is the provider's own figure when it reports one (`usage.cost`),
  else tokens × the `price` in the settings file. The settings must have a
  price, and a run whose cost can't be known stops the campaign. That
  includes a provider that doesn't report usage (in a streamed reply, its
  last chunk): the runner stops such a run with outcome `no-usage`, since
  neither its tokens nor its cost can be counted.
- Repetitions run in order: every task once, then every task again. A campaign
  stopped by its cap still covers the tasks evenly.

Why three repetitions: one run per task can't tell a change in the agent from a
model's run-to-run spread. The report gives each repetition's success rate, and
their spread is the noise any later comparison (G02, G03) has to beat.

## Running

Prerequisites: Rust and the app's build dependencies (to build the runner and
check the Rust tasks), the app's own `node_modules` (`npm ci`: the grader's
TypeScript runner uses its esbuild), Node, Python 3 and Go, plus network
access to GitHub, npm, PyPI, the Go module proxy and the model's provider.
A sandbox that can take the network away: bubblewrap, or Landlock from
Linux 6.7, or macOS. The self tasks need this repository's whole history
(`git fetch --unshallow` in a shallow clone).

```sh
node scripts/eval/eval.mjs validate                 # all tasks (Rust ones take a while)
node scripts/eval/eval.mjs dry-run [--jobs 3]       # the pipeline, with a scripted model
node scripts/eval/eval.mjs pilot --settings my-settings.json
node scripts/eval/eval.mjs run --settings my-settings.json
node scripts/eval/eval.mjs report eval-results/baseline
```

Runs side by side (P05 in
[docs/design/evolve-plan.md](../../docs/design/evolve-plan.md)): `--jobs N`
runs N at once, in a campaign and in both dry runs, started in the plan's
order; a campaign's budget counts the runs under way at the estimate. Each
run is a runner process of its own, with its own data and log folders, and
its id (`campaign/task/repetition`, the runner's `--run-id`) is in its
report. Each worker has its own cargo target folder
(`.eval-cache/cargo-target/job-<n>/`; the first worker keeps the one runs
always used), so Rust builds side by side neither wait on each other's
lock nor lose what the pruning takes. Several heavy builds at once need
the memory for them.

The settings file is the runner's (see `settings.example.json` and the top of
its [src/lib.rs](../../src-tauri/crates/rovik-headless/src/lib.rs)), with
`price` in US dollars per million tokens. Keep the API key in an environment
variable named by `apiKeyEnv`, not in the file. Its
`config.permissions` must have `allowAnyCommand: true` (the tasks' test runners
aren't on the app's default command list) and `toolCallCheckpoint: null` (a
pause every 50 tool calls would end a headless run; the caps bound it instead).
Other settings are the app's defaults: the checks run when a run ends, and so
does the checklist. `policy.json` refuses `git push` and `sudo` whenever the
agent runs them, written in any form (`git -C . push`, `env X=1 git push`,
`bash -c 'git push'`), with no approval asked: its command deny rules become
the run's denied commands. A denied command is matched by its words: the
program, then the entry's other words in order anywhere after it; a word of
short options matches them however they're written (`rm -rf` also refuses
`rm -r -f`, `rm -fr` and `rm --recursive --force`). Anything that runs the
program by another name (a script, an alias) isn't caught. It allows everything else, and picks the
recommended answer to questions. Each question answered (and each approval,
when settings ask for them) counts as an intervention in the report.

`dry-run` runs each task four times with `fake-model.mjs`, a Python task
five. The first model applies the reference change and must grade as a
success. The second changes nothing and must grade as a failure. The third
looks for the answer and must find nothing (above). On a Python task, the
fourth tampers with how the tests run and must be graded `tampered`
(Grading). The last run is a runner that under-reports what it used
(`fake-runner.mjs`): it must be graded `tampered`, its cost the meter's
(Budget). This checks the workspace, runner, sandbox, grading and
cost accounting without a provider. The scripted model's results say nothing
about any real model.

Results go to `eval-results/<campaign>/<task>/<repetition>/`:

- `report.json`: the runner's report.
- `changes.diff`: what the agent changed.
- `grade.json`: each test's result.
- `result.json`

The campaign directory also gets `summary.json` and `summary.md`. A campaign
can be resumed: finished runs are kept.

What the runs are made with is recorded (P03 in
[docs/design/evolve-plan.md](../../docs/design/evolve-plan.md)):
`result.json`'s `identity` and the campaign's `identity.json` hold the
runner's commit (this repository's HEAD for its own build, `+dirty` with
uncommitted changes under `src-tauri`; `--runner-commit` for another
runner) and the SHA-256 of the runner, the grader's integrity check, the
settings file and the policy file. A campaign resumed with any of those
four changed refuses and says which, as does one whose files change while
it runs: its runs would mix two builds without a word. A changed commit
alone, the binary the same, isn't another build. Caches (clones, node_modules, the
virtualenv, cargo's target directory) are in `.eval-cache/`.
`ROVIK_EVAL_CACHE` and `ROVIK_EVAL_RESULTS` move them.

Caches are shared by every run, so no run may change them for the next:
each run gets its own copy of `node_modules` (a copy-on-write clone where
the file system can; where it can't, the evaluation says so once and each
run copies it in full), and the virtualenv's files are read-only (as root
that doesn't hold). Its folders stay writable, so `rm -rf .eval-cache`
works without a `chmod` first. A run that changed a shared cache anyway is reported with a warning
and `changedCaches` in its `result.json`. Cargo's target directory is shared
on purpose (building every commit from scratch costs too much): a task's
own crate is pruned from it after each run.

A run whose provider reports no usage has an unknown cost: it's left out of
the mean costs (the summary says how many), and a pilot with one can't
project the campaign's cost. The campaign stops after such a run; resumed,
it goes on past it (the run kept, its cost not counted in what was spent),
and likewise past three errors in a row kept from before.

Ctrl+C (or a kill of `eval.mjs`) ends the runs it started, and what they
started, before it exits; the run that was going has no result and runs
again when the campaign is resumed.

The report, per task and overall:

- success rate (and per repetition), and beside it pass@k and pass^k for
  each k up to the repetitions (see below)
- cost
- tokens
- peak main-agent context (its largest request)
- time
- interventions
- tool errors, edit misses and the checks' rounds (see below)
- the runs' outcomes
- for Autopilot runs, each stage: how many runs reached it, how they ended
  it and its rounds

**pass@k and pass^k** (the Autopilot plan's G03). pass@k is the chance that
at least one of k runs of a task passes: whether the agent can do it.
pass^k is the chance that all k do: how reliably it does. Each is the
unbiased estimate from a task's n runs and c passes (1 − C(n−c, k)/C(n, k),
and C(c, k)/C(n, k)), over the tasks with at least k runs. pass@1 and
pass^1 are the success rate.

**Tool errors and the checks' rounds** (the Autopilot plan's G02). The agent
log has a `tool` line for every finished tool call: its tool, whether it
failed and the error's class (`old_text not found`, `no such file`,
`denied`, … from the words the tools write; `rovik-core`'s
`tools::error_class`), how long it took, and a command's exit code. A
command that ran and exited non-zero didn't fail as a call. The runner's
report counts them, the run's subagents' included: `toolErrors` (failed
calls, those a Stop, a closed app or a budget cut aside), by class
(`toolErrorClasses`), `editMisses` (edits whose old text matched nowhere,
or more than once) and `allToolCalls`; `verifyRounds` is the checks' rounds
from the lead's `verify` lines, and `stages` an Autopilot task's stage
records (null otherwise). `result.json` keeps these, with the run's `mode`
and `effort`.

## Each kind on by itself (G02) and the defaults (G03)

After the baseline, each kind the task set exercises runs on by itself, as a
campaign of its own (`eval-results/kind-<id>/`), with the same settings
otherwise:

| Kind            | Milestone |
| --------------- | --------- |
| scout           | A         |
| change reviewer | B         |
| failure triage  | C         |
| test writer     | E         |

The plan reviewer (Plan mode only), step workers (saved plans only) and
parallel groups with the repair loop (plans with groups, isolated chats) never
come up in these tasks, which are Agent-mode requests: they aren't measured
and stay off by default.

```sh
node scripts/eval/eval.mjs variants --settings my-settings.json [--kinds scout,triage]
node scripts/eval/eval.mjs compare          # writes docs/design/subagents-results.md
node scripts/eval/eval.mjs apply-defaults   # G03: the decision into src/lib/subagentSettings.ts
node scripts/eval/eval.mjs dry-run-variants # all of it with the scripted model, in a scratch folder
```

- `variants` takes the baseline's settings (subagents off: `run` refuses
  settings with any kind on) and turns one kind on for each campaign. A kind's
  campaign starts only if the baseline's cost a run, times the kind's cost
  budget, times its runs, fits the $75 cap; otherwise it's skipped and says
  so. A stopped campaign can be resumed by running `variants` again.
- `compare` compares each kind with the baseline over the tasks both ran:
  success, cost a run, the lead's peak context, time and interventions. It
  writes `docs/design/subagents-results.md` and
  `eval-results/comparison.json`.
- The decision (G03): a kind goes on by default only if
  - its success isn't worse than the baseline's by more than the noise (the
    larger spread between repetitions, and at least one task);
  - it costs at most 2× the baseline a run (`budget.json`
    `variants.costRatioMax`, per kind or `default`; `subagents.md` section 6);
  - and it improved success by more than the noise, or brought the lead's peak
    context down to 90% of the baseline's or less.

  A campaign that stopped early, or covers fewer than 5 tasks, decides nothing.

- `apply-defaults` turns the chosen kinds on, with the master switch, in
  `DEFAULT_SUBAGENT_SETTINGS`. It refuses results from the scripted model.
  Then update the defaults test and link the results file in the pull request.

## Power: the smallest change a comparison can show (P07)

Before a comparison is believed, or paid for: how small a change in
success it could find (its minimum detectable effect), at α = 0.05 and
80% power, for a paired comparison of two campaigns over the same tasks.
It comes from each task's run-to-run noise (p(1 − p) of its outcomes,
corrected for few runs), the repetitions, and the modules' design effect:
the campaign against itself, repetition against repetition, clustered by
module and not (`stats.mjs`). `summary.md` has it ("Smallest change it can
show"), and so does each comparison. With a baseline:

```sh
node scripts/eval/eval.mjs power eval-results/baseline --repeats 3,6 --target 5
```

prints it at 3 and 6 repetitions an arm, and how many tasks a change of
5 points would need: the source experiment (milestone E) starts only at 5
points or less. For scale, at p(1 − p) ≈ 0.1 a task, 30 tasks × 3
repetitions can show about 13 points, and 5 points takes about 105 tasks
× 6 or 210 × 3.

## The same-money arm (P06)

P06 in [docs/design/evolve-plan.md](../../docs/design/evolve-plan.md), its
5.12 A: a change that costs more has to beat the current agent given the
same money another way, or "a better agent" and "more compute" look the
same. Beside Autopilot at each effort (below), `attempts-2` runs the
baseline's settings twice at each task, in two workspaces, and keeps the
attempt the project's own checks pick (`arms.mjs`): the one whose last
verification passed, else one where none ran, else one whose checks
passed with its tests weakened (`flagged`), else the first; never by the
hidden tests. The repetition's result is the kept attempt's grade, with
what both cost, and each attempt in brief (`attempts`, `picked`); the
attempts' own runs are in `attempt-1/` and `attempt-2/`. Its budget is
`budget.json` `attempts`: twice the baseline's cost a run, $150 for 30
tasks × 3.

```sh
node scripts/eval/eval.mjs variants --settings my-settings.json --variants attempts-2
node scripts/eval/eval.mjs compare baseline attempts-2
```

The pick needs checks to pick by: the baseline's settings have the app's
default, the checks run when a run ends. The dry run's attempts both make
the reference change, without checks: it tests the pipeline, not the pick
(`arms.test.mjs` does).

## A race between settings (R01)

R01 in [docs/design/evolve-plan.md](../../docs/design/evolve-plan.md) (its
5.2 F and D11), Evolve's first stage: settings race the current ones, and
no model writes anything. A candidate is a patch of what a race may vary
(`race.mjs` `RACEABLE`): the lead's model and its price (`profile`,
`price`), its reasoning effort (`options.reasoning_effort`), the context
mode (`contextMode`) and the subagents (`config.subagents`). A patch that
sets anything else is refused, naming what it may not set.

```sh
node scripts/eval/eval.mjs race --settings my-settings.json --candidates candidates.json [--jobs 3]
node scripts/eval/eval.mjs dry-run-race [--jobs 3]   # the pipeline, with scripted models
```

```json
[
  { "name": "effort-low", "settings": { "options": { "reasoning_effort": "low" } } },
  { "name": "no-subagents", "settings": { "config": { "subagents": { "enabled": false } } } }
]
```

- The current settings (`--settings`) run as `current`: what the others
  are measured against, never dropped. Every candidate runs the same
  tasks, once each, in an order drawn from `--seed` (1), a block of 5 at a
  time (`--block`).
- After each block, a candidate is dropped when a sign test on the tasks it
  and the leader (the highest success, the cheaper of equals) differ on
  shows it worse, Holm-corrected across the candidates (α 0.05), and not
  before 10 tasks. The race stops when the tasks run out, when the next
  block could pass its cap (`budget.json` `race`, $300), or when only the
  current settings are left.
- Then D11 decides, for each candidate still in, against the current
  settings on the same tasks: **better** when the paired 95% interval (a
  bootstrap over modules) is above zero and the gain at least the smallest
  change the tasks can show (P07); **cheaper and not worse** when the
  interval stays above −5 points (`--margin 5`, stated before the race)
  and it costs less a run. Neither at more than twice the current cost a
  run. The better one with the most success is chosen, else the cheapest
  of the cheaper and not worse; else the current settings stay.
- Each candidate's runs are a campaign in the race's folder
  (`eval-results/race/<name>/`, `--out`), its `settings.json` and identity
  (P03) beside them, so a race resumes: the same order, the finished runs
  kept. Its record is `race.md` and `race.json` there: each block's leader
  and who was dropped (with p), each candidate's success and cost, and the
  decision.
- What a race picks goes into the defaults only by hand, in a pull request
  that cites its `race.md` (R03).

`dry-run-race` races three scripted settings on the Go and Python tasks:
the current (a model that makes the reference change), one that changes
nothing, and the current at half the price. The second must be dropped
and the third chosen ("The race works."): it checks the blocks, the
drops, the cost and the decision, not any real model.

## Autopilot at each effort (the Autopilot plan's G03)

Beside the kinds, `variants` runs Autopilot at each effort, each a campaign
of its own (`eval-results/autopilot-balanced/`,
`eval-results/autopilot-thorough/`): the request as an Autopilot task
(the runner's `--mode autopilot --effort …`), on the baseline's settings.
A task turns on the kinds its stages need itself.

```sh
node scripts/eval/eval.mjs variants --settings my-settings.json --variants autopilot-balanced,autopilot-thorough
node scripts/eval/eval.mjs variants --settings my-settings.json --variants autopilot-balanced --repeats 1   # a pilot
node scripts/eval/eval.mjs compare baseline autopilot-balanced
node scripts/eval/eval.mjs compare autopilot-balanced autopilot-thorough
node scripts/eval/eval.mjs dry-run-variants --variants autopilot-balanced,autopilot-thorough
```

- `--variants` takes campaign or kind ids (`autopilot-thorough`, `scout`,
  `kind-scout`); `--kinds` still takes kinds only; with neither, every
  variant runs.
- **Budget** (`budget.json` `autopilot`). A task costs more than a plain
  run: "two to three plain Agent runs" for Balanced, several times that
  for Thorough (the Autopilot plan, section 8). Each effort has:

  | Effort   | The task's budget         | Cost budget      | Cap  |
  | -------- | ------------------------- | ---------------- | ---- |
  | Balanced | 1.5 h, 4.5 million tokens | 3× the baseline  | $225 |
  | Thorough | 4 h, 15 million tokens    | 10× the baseline | $750 |

  The task's budget is its own (`config.autopilot.budget` in the variant's
  settings): the task ends itself with outcome `budget` and delivers what
  it has. The runner's caps (`--timeout`, `--max-tokens`) are a tenth above
  it, so they only catch a runaway. A campaign starts only if the
  baseline's cost a run, times the cost budget, times its runs, fits the
  cap, as a kind's does; 3 repetitions unless `--repeats` says.

- **Prepare is off** (`config.autopilot.prepare: false`): the evaluation
  gives each workspace its dependencies, and an install would change the
  caches every run shares. The dry run's Prepare found more-itertools'
  `docs/requirements.txt` and installed it into the shared virtualenv.
- **Checks that fail before any change.** Autopilot's checks are the
  project's, as detected: `go vet ./...` fails on lo's own tests, and
  `ruff check .` (run wherever ruff is installed) on more-itertools, with
  or without the change. The task then ends `verify-unresolved` at its
  checks; with a real model, after rounds spent on them. The baseline's
  checks, on by default, meet the same failures.

- **`compare A B`** compares any two campaigns over the tasks both ran:
  every number side by side, pass@k and pass^k, each task, and B's stages.
  B is better when its success beats A's by more than the noise (the
  larger spread between repetitions, and at least one task) within its
  cost budget: a variant's own, `--cost-ratio N`, or none for other
  campaigns. Beside that verdict, task by task (P04 in
  [docs/design/evolve-plan.md](../../docs/design/evolve-plan.md),
  `stats.mjs`): how many tasks each did better on, the mean difference
  in success, its standard error, plain and clustered by module (tasks
  from one module aren't independent), and a 95% interval from a
  bootstrap that draws whole modules. A task's module is its file's
  `module`, else its repository and the folder of its first hidden test. A campaign that stopped early, or fewer than 5 tasks in
  both (`--min-tasks`), decides nothing. It writes
  `eval-results/compare-A-B.md` and `.json`, or `--out`. Without names,
  `compare` is the kinds' comparison above.
- **The dry run** gives Autopilot the scripted model's `autopilot` mode
  (`fake-model.mjs autopilot <files.json>`): a plan naming the files the
  reference changes, the answer each stage's brief is read for (the plan
  review and the change review find nothing, the test writers and the
  adversarial tester are blocked, the acceptance check finds every
  criterion satisfied, Thorough's planners plan and its judges pick the
  first), and the change as `write` calls of the reference's files, so
  the change review and the acceptance check see it. Every run must pass
  and reach its delivery with none of the stages the scripted model
  answers failed; the project's own checks may fail (above), and the run's
  line names them. It tests the pipeline, not Autopilot: what a task does
  with a real model is the campaigns'. `FAKE_MODEL_LOG=file` makes the
  scripted model write each request it gets to the file, a JSON line each.
