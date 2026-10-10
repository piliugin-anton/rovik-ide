# Agent evaluation

G01 in [docs/design/subagents-plan.md](../../docs/design/subagents-plan.md): the
main agent's baseline on a fixed task set, before subagents change anything. The
headless runner ([src-tauri/src/headless.rs](../../src-tauri/src/headless.rs))
runs each task as the app would; the task's hidden tests then grade it.

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

Contamination: the more-itertools and lo changes are public and older than
current models' training data, so a model may have seen them; this
repository's are newer. Read results per source as well as overall.

## Grading

SWE-bench style. The agent works in a fresh repository holding only the base
commit's files (no history, so the answer can't be read from it). After the
run, the reference's version of the `hidden` test files is put in the workspace
(over anything the agent did to them) and the checks run. A run succeeds when
every test the reference passes also passes.

`validate` decides which tests those are. It runs the checks at the base (with
the hidden tests) and at the reference, twice. Required tests are the ones that
pass at the reference both times. A test that passes only once is flaky and
left out. A task is usable when some required test fails at the base. The
result goes to `expected/<task>.json` and is committed with the task. Grading
uses it, and a task whose reference changed must be validated again.

## Budget

| Campaign | Runs                          | Cap |
| -------- | ----------------------------- | --- |
| pilot    | 5 tasks × 1 (`budget.json`)   | $5  |
| baseline | 30 tasks × 3 repetitions = 90 | $75 |

Every run is limited to 1.5 million tokens (the runner's `--max-tokens`: the
main agent's and its subagents' requests, as the provider reports them; the run
stops with outcome `budget`) and 30 minutes (outcome `timeout`).

- A campaign starts a run only while what it has spent, plus the mean cost of
  its runs so far, stays within its cap. Before its first run, the estimate is
  the worst case: every allowed token at the input price.
- `run` (the baseline) starts only if the pilot's mean cost a run, times the
  runs planned, fits the $75 cap. Otherwise, run fewer tasks (`--tasks`) or
  repetitions (`--repeats`).
- A run's cost is the provider's own figure when it reports one (`usage.cost`),
  else tokens × the `price` in the settings file. The settings must have a
  price, and a run whose cost can't be known stops the campaign.
- Repetitions run in order: every task once, then every task again. A campaign
  stopped by its cap still covers the tasks evenly.

Why three repetitions: one run per task can't tell a change in the agent from a
model's run-to-run spread. The report gives each repetition's success rate, and
their spread is the noise any later comparison (G02, G03) has to beat.

## Running

Prerequisites: Rust and the app's build dependencies (to build the runner and
check the Rust tasks), Node, Python 3 and Go, plus network access to GitHub,
npm, PyPI, the Go module proxy and the model's provider.

```sh
node scripts/eval/eval.mjs validate                 # all tasks (Rust ones take a while)
node scripts/eval/eval.mjs dry-run                  # the pipeline, with a scripted model
node scripts/eval/eval.mjs pilot --settings my-settings.json
node scripts/eval/eval.mjs run --settings my-settings.json
node scripts/eval/eval.mjs report eval-results/baseline
```

The settings file is the runner's (see `settings.example.json` and
headless.rs), with `price` in US dollars per million tokens. Keep the API key in
an environment variable named by `apiKeyEnv`, not in the file. Its
`config.permissions` must have `allowAnyCommand: true` (the tasks' test runners
aren't on the app's default command list) and `toolCallCheckpoint: null` (a
pause every 50 tool calls would end a headless run; the caps bound it instead).
Other settings are the app's defaults: the checks run when a run ends, and so
does the checklist. `policy.json`
answers approvals (everything except `git push` and `sudo` is allowed) and
picks the recommended answer to questions. Each answer counts as an
intervention in the report.

`dry-run` runs each task twice with `fake-model.mjs`. The first model applies
the reference change and must grade as a success. The second changes nothing
and must grade as a failure. This checks the workspace, runner, grading and
cost accounting without a provider. The scripted model's results say nothing
about any real model.

Results go to `eval-results/<campaign>/<task>/<repetition>/`:

- `report.json`: the runner's report.
- `changes.diff`: what the agent changed.
- `grade.json`: each test's result.
- `result.json`

The campaign directory also gets `summary.json` and `summary.md`. A campaign
can be resumed: finished runs are kept. Caches (clones, node_modules, the
virtualenv, cargo's target directory) are in `.eval-cache/`.
`ROVIK_EVAL_CACHE` and `ROVIK_EVAL_RESULTS` move them.

The report, per task and overall:

- success rate (and per repetition)
- cost
- tokens
- peak main-agent context (its largest request)
- time
- interventions
- the runs' outcomes
