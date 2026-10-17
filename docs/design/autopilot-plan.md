# Autopilot: a task done on its own, from the request to a delivered result

Today a chat needs a person at the keyboard: to approve, to answer, to
press "Implement this plan", to read the result and land it. This plan
adds **Autopilot**, a chat mode in which one message is a whole task: the
agent writes criteria and a plan, the plan is reviewed until it holds,
tests are written from the plan, the plan is implemented, the change is
reviewed, checked and judged against the criteria, and the result is
delivered with a report. Nothing in it waits for a person. What the agent
can't decide, it writes down.

Autopilot has two efforts. **Balanced** runs each stage once, with the
subagents that exist. **Thorough** spends tokens on independence: several
candidates where one model's blind spot would decide the outcome, a
second model for every judgment, an adversarial tester, and loops that
end on "no progress" rather than on a count.

On a cloud host, a third choice under "While nobody is there", **Works on
its own**, makes Autopilot the default for that host's chats and adds what
only a server needs: delivery to a git branch, a notice when a task ends,
and a budget.

Read [`subagents.md`](subagents.md) and
[`subagents-plan.md`](subagents-plan.md) for the subagent kinds this plan
builds on, and [`cloud-agent-dev-plan.md`](cloud-agent-dev-plan.md) for the
daemon, the hosts page and the way a result comes back from a server. This
file says what to build, in which files, in which order, and how each step
is tested.

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [The shape of Autopilot](#3-the-shape-of-autopilot)
4. [Each stage](#4-each-stage)
5. [Milestones](#5-milestones)
6. [Implementation order for an AI agent](#6-implementation-order-for-an-ai-agent)
7. [Testing](#7-testing)
8. [Risks and known limits](#8-risks-and-known-limits)
9. [Decided questions](#9-decided-questions)

---

## 1. Decisions

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | Why                                                                                                                                                                                                                                                        |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **Autopilot is a chat mode**, the third beside Agent and Plan (`src/lib/chatMode.ts`, `StartRequest.mode`). It works on a local chat and on a remote one. A run keeps the mode of the message that started it, as today.                                                                                                                                                                                                                                                        | It can be watched and evaluated locally before it's trusted on a server, and the headless runner and the evaluation set get it for free. A host policy alone couldn't be tried without a host.                                                             |
| D2  | **A task is a pipeline of runs in one chat**, driven by the harness (`agent_run/autopilot.rs`), not one long run. Each stage is a run (the plan, the implementation) or an IDE-started subagent (the reviews, the tests, the acceptance check), and starts in a fresh context from artifacts: the request, the criteria, the plan, the diff, the checks' output.                                                                                                                | Context rot is the main reason long unattended runs degrade. Fresh contexts also mean every stage reuses an existing code path: the Plan-mode run, the "Implement this plan" run, the verify gate, `run_ide_subagent`.                                     |
| D3  | **Every stage leaves a record** in the chat: an empty assistant message with `meta.stage`, like the verification records (`meta.verification`), never sent to the model. The pipeline's state _is_ its records: what ran, with which outcome, what comes next.                                                                                                                                                                                                                  | Rows survive restarts, go to the mirror with the chat (dev plan 6.3), and need no new storage. A daemon that restarts, or an app that reopens, reads the records and continues.                                                                            |
| D4  | **Effort: Balanced or Thorough**, in `AutopilotConfig.effort`. It's a setting on the Subagents page's new Autopilot section, and a host's "Works on its own" block can override it for that host's chats. No per-chat choice in v1.                                                                                                                                                                                                                                             | Two levels are explainable; a slider isn't. The host override exists because a server is where tokens matter least and time matters most.                                                                                                                  |
| D5  | **Autopilot never asks.** `ask_user` is refused in its plan stage with the message Agent mode gives today; assumptions go under the plan's "## Decisions". The system prompt's line on clarifying questions is replaced in Autopilot by "decide, state the assumption, proceed".                                                                                                                                                                                                | A question nobody answers is a run that never ends. The plan reviewer sees the assumptions, and the report lists them first, so a wrong one is caught at the cheapest point that has a reader.                                                             |
| D6  | **Nothing in an Autopilot run waits for a person.** The tool-call checkpoint is off, a loop pause resumes once with its reason, approvals go to the AI reviewer, and what the reviewer can't approve is _refused_, never parked. A resumed call after a crash runs again without asking. Commands naming `.rovik` stay refused (dev plan D8).                                                                                                                                   | Each "wait for the user" state is a run that looks finished and isn't. Refusing is safe: the agent is told and adapts, and the record says what was refused.                                                                                               |
| D7  | **Nothing lands unverified.** In Autopilot the outcomes `unverified` and `verify-unverified` don't land: a project with no detected check runs the commands under the plan's "## Validation" as its checks, and a check the allowlist refuses runs anyway, because the gate's commands are the IDE's, not the model's.                                                                                                                                                          | Today `LANDS_AFTER` (`agent_run/landing.rs`) lands "unverified" with a notice for the user to read. With nobody reading, the verification floor must be in the pipeline.                                                                                   |
| D8  | **Criteria before code.** The plan gains a required "## Criteria" section: testable statements of what done means, "Out of scope" and "Assumptions". The plan reviewer checks them, the test writer writes from them, the acceptance checker judges by them. Thorough has two independent criteria writers and a judge that merges them.                                                                                                                                        | "Done" today means the checks pass. Criteria turn "does it do what was asked" into something a test and a reviewer can check, and they pin the _original_ request, which the reviewers don't see today (`latest_user_request` returns the latest message). |
| D9  | **Tests first.** The test writer runs after the plan review and before the implementation, from the criteria and the plan, with the red check on: a test that passes before the change is flagged. The implementation run is told which tests to make pass.                                                                                                                                                                                                                     | The test writer already never reads the new code (subagents plan K11). Running it first makes acceptance mechanical where it can be, and gives the implementer a target instead of a description.                                                          |
| D10 | **The change review blocks.** The agent answers each finding in a structured way (`fixed`, or `not a problem` with a reason), as it answers plan findings in `submit_plan`'s `responses`. A follow-up review must find every high finding resolved before the checks count. A high finding still open at the stage's cap ends the task with `review-open`.                                                                                                                      | Today a high finding is dismissed with free text and dropped as a repeat in the next round (`drop_repeats`). That's right with a reader, wrong without one.                                                                                                |
| D11 | **An acceptance check after green.** A read-only subagent gets the criteria, the diff, the checks' results and the test writer's report, and gives a verdict per criterion with evidence. Unsatisfied criteria go back to the implementer once (Balanced) or until no progress (Thorough). Still unsatisfied ends the task with `unmet`, delivered as partial.                                                                                                                  | Passing checks never meant the request was met. This is the one stage whose only job is that question, and it's independent of the implementer.                                                                                                            |
| D12 | **Repair rounds are bounded by progress and budget, not by three.** In Autopilot `MAX_VERIFY_ROUNDS` doesn't apply: a round that uncovers failures the last one hid (a type check that passed shows the tests) isn't counted, and the loop ends after two rounds without progress or at the budget. An environment failure (exit 127, "command not found", "Cannot find module", "No module named") is a prepare failure, not a code failure: it reruns the prepare stage once. | Three rounds absorb cascades and setup problems before the real failure is seen (review findings 6 and 7). The no-progress rule exists (`repair::Judgement`); the budget is new (D16).                                                                     |
| D13 | **Thorough buys independence, not repetition.** Three plan candidates and a judge; two criteria writers; two implementers in two working copies where a second copy is possible; an adversarial tester; a second model for every judging role by default. Never "ask the model if it's sure" twice.                                                                                                                                                                             | Redundant _independent_ work decorrelates errors; repeated self-assessment doesn't.                                                                                                                                                                        |
| D14 | **Candidates are scored mechanically, the judge breaks ties.** Score: criteria tests passed, then project checks passed, then high and medium review findings (fewer is better), then diff size (smaller is better). The judge model picks only when two candidates tie.                                                                                                                                                                                                        | A judge is a model too. Tests first keeps its influence to the cases where nothing else can decide.                                                                                                                                                        |
| D15 | **Delivery on a server is a push.** After a landable outcome the daemon pushes the result commit to a branch `rovik/<slug>` on the project's remote, with a deploy key the user put on the server, and opens a pull request when `gh` is installed and logged in there. The result ref and the `ResultReady` event stay as they are, so the laptop still gets the result.                                                                                                       | Results never leave a server today without a client (dev plan 6.5). A branch is what every forge, CI and reviewer can take. The agent's own `git push` stays refused: the daemon pushes, once, what the pipeline delivered.                                |
| D16 | **A budget per task**: wall time and cost, from the Autopilot settings, overridden by the host's block. The pipeline checks it between stages, and the run checks it per reply from the usage providers report. Past it, the task ends with `budget`, delivered as partial with a report.                                                                                                                                                                                       | The lead has no bound today (`the_lead_has_no_wall`). Unattended, the only stop would be a provider error or the user. Thorough's "no progress" loops need a hard stop.                                                                                    |
| D17 | **Autopilot enables the kinds it needs for its own runs**: plan reviewer, change reviewer, triage, test writer, step workers, acceptance checker; in Thorough also planner, judge and adversarial tester. The user's switches on the Subagents page still govern ordinary chats; the rows there give the models.                                                                                                                                                                | A pipeline whose stages can be switched off isn't a pipeline. Showing the rows as "Used by Autopilot" keeps the models in one place.                                                                                                                       |
| D18 | **A notice when a task ends or is blocked**: a URL on the host's block, called by the daemon with the chat, the stage, the outcome and the report's summary. Nothing else: no email, no push service in v1.                                                                                                                                                                                                                                                                     | One HTTP call reaches everything (Slack, ntfy, a GitHub Action, a mailer). The daemon knows the moment; the laptop may be off.                                                                                                                             |
| D19 | **Prepare before the first check**: in a working copy whose dependencies aren't installed, the pipeline runs the install the lockfile implies (`npm ci`, `pnpm install --frozen-lockfile`, `yarn install --immutable`, `cargo fetch`, `pip install -r requirements.txt`, `uv sync`, `go mod download`), as heavy work, once, and records the output.                                                                                                                            | A fresh server worktree has no `node_modules`; "vitest: not found" would otherwise eat the repair rounds, and the model can't run installs under the default allowlist.                                                                                    |
| D20 | **Nothing changes for a chat that isn't in Autopilot** and for a host not set to "Works on its own", with three exceptions this plan marks as general improvements: a syntax check after every write (V04), a rollback that puts back only the files behind the regression (V05) and a Go test counter (V08). They go in their own tasks with their own tests.                                                                                                                  | The rules of the earlier plans: the app keeps working for everyone else, and a behaviour change for everyone is a decision made on purpose, in one task, with a test.                                                                                      |

---

## 2. What the code does today

What this plan builds on, with where it is. Each item is a thing to keep,
or a thing to change in a named task.

- **A run ends when the model replies without tool calls**
  (`agent_run/run.rs`, the loop in `drive`). The verify gate then runs the
  project's checks (`agent_run/verify.rs`); with none enabled it records
  `unverified`. `pending_reason` in `agent_run/landing.rs` maps every
  outcome to "lands on its own" or a reason that waits for the user;
  `LANDS_AFTER` is `done`, `verified`, `unverified`, `verify-unverified`.
- **Human-wait states**: `wait_for_approval` (`agent_run/mod.rs`) has no
  deadline; the tool-call checkpoint (`DEFAULT_TOOL_CALL_CHECKPOINT = 50`,
  `rovik-protocol/src/permissions.rs`) and the loop guard end a run as
  `paused-<kind>`; a call resumed after a crash is approved by the user
  (`agent_run/recovery.rs`); a reviewer that is unsure falls back to the
  user (`agent_run/execute.rs`); `verify-flagged`, `verify-needs-approval`
  and `verify-gave-up` park the result. `ask_user` exists in Plan mode only;
  in Agent mode a question in plain text ends the run as `done`.
- **Plan mode** ends at `submit_plan`; the plan reviewer holds a plan for at
  most `MAX_PLAN_REVIEW_ROUNDS = 2` (`agent_run/plan_review.rs`) and then
  saves it with an "## Open review findings" section. Implementation
  starts from a new user message built by `implementationRequest` in
  `src/lib/chatMode.ts`, with `meta.implementsPlan`. Plans are parsed from
  Markdown by `agent_run/plan.rs` (`plan_problems`, the steps' "Files:"
  lines, the "## Decisions" section).
- **Subagents** (`agent_run/subagent.rs`, `Kind`): scout, change reviewer,
  triage, plan reviewer, step worker, test writer, plus the AI approver
  (`agent_run/review.rs`). Every kind is off by default
  (`src/lib/subagentSettings.ts`). The harness starts the reviewer, triage
  and plan reviewer itself (`run_ide_subagent` in `subagent_run.rs`); the
  lead starts scouts, workers and the test writer with `task`. The change
  review is advisory: findings go to the lead as a note, and its answers
  are free text (`change_review.rs`, `answers_since`, `drop_repeats`).
  Parallel implementers (`agent_run/group.rs`) need an isolated chat, and
  only they get a light check between steps (`project::light_check_for`).
- **Hosts**: `ApprovalPolicy` has two values (`rovik-client/src/remote.rs`,
  `apply`), shown as two radios under "While nobody is there" in
  `src/components/CloudHostsSettings.tsx`. The host's settings
  (`rovik-client/src/host_settings.rs`, `src/lib/hosts.ts`) also carry a
  setup script that runs over SSH in `$HOME`. The daemon
  (`crates/rovik-agentd`) runs chats in worktrees, never isolated, commits a
  run's result to `refs/rovik/chats/<chat>/result` (`results.rs`,
  `after_run`) and tells the client with `ResultReady`; it never pushes
  anywhere, and `agent_run/server.rs` refuses the agent's own `git commit`
  and `git push`. `restart.rs` continues interrupted runs, at most twice an
  hour. `Config` (`config.rs`) holds the log level, the tokens and the
  slots. The daemon's event tap is `Tap` in `serve.rs`.
- **Headless** (`crates/rovik-headless`): starts one run on a project with a
  `Policy` that answers approvals and questions, a timeout and a token cap,
  and writes a report. The evaluation set (`scripts/eval`, 30 tasks with
  hidden tests) scores by hidden tests; its baseline was never run, and no
  workflow runs the suites.

---

## 3. The shape of Autopilot

### 3.1 The pipeline

One message in Autopilot mode starts a task. Its stages, in order; each
leaves a stage record, and the strip above the message box shows them.

| Stage             | Who does it                                                                    | Ends the task when                                                                                                               |
| ----------------- | ------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| **Plan**          | A Plan-mode run (Balanced); three planner subagents and a judge (Thorough)     | The planner can't produce a plan that passes `plan_problems`: `plan-failed`                                                      |
| **Plan review**   | The plan reviewer, rounds until clean                                          | A high finding is still open at the cap: `plan-blocked`                                                                          |
| **Prepare**       | The harness: the install the lockfile implies, once                            | Never. A failed install is recorded and the checks will say what's missing                                                       |
| **Tests**         | The test writer, from criteria and plan, red check on                          | Never. A blocked test writer is recorded; acceptance then rests on the reviewer and the acceptance checker                       |
| **Implement**     | An "Implement this plan" run (Balanced); two, in two working copies (Thorough) | The run ends `error`, `repetition` or `budget`                                                                                   |
| **Change review** | The change reviewer, blocking, structured answers                              | A high finding is still open at the cap: `review-open`                                                                           |
| **Checks**        | The verify gate, rounds bounded by progress and budget                         | The checks still fail with no progress: `verify-gave-up`; a weakened test: `verify-flagged` (delivered as partial, never landed) |
| **Break**         | Thorough only: the adversarial tester writes tests that try to break it        | Never. Its failing tests are a repair round; still failing, `verify-gave-up` as above                                            |
| **Pick**          | Thorough only: the candidates scored, the judge on ties                        | Never                                                                                                                            |
| **Acceptance**    | The acceptance checker, per criterion                                          | A criterion is still unsatisfied after the allowed round(s): `unmet`, delivered as partial                                       |
| **Deliver**       | Land (local) or commit and push (server); the report; the notice               | Always: the last stage                                                                                                           |

Every ending is a named outcome with a report. A person coming back sees
one red stage in the strip and a message that says exactly what blocked
it. There is no outcome that waits.

### 3.2 Effort: Balanced and Thorough

| Stage         | Balanced                                            | Thorough                                                                                                                                          |
| ------------- | --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| Criteria      | Written by the planner, in the plan's "## Criteria" | Two criteria writers in parallel, different models; a judge merges them and records where they disagreed, taking the narrower reading             |
| Plan          | One Plan-mode run                                   | Three planner subagents in parallel; the judge picks one or merges; the result goes through `submit_plan` so `plan_problems` and the review apply |
| Plan review   | Up to 3 rounds                                      | Until clean, or two rounds without progress                                                                                                       |
| Tests         | The test writer, red check                          | The same                                                                                                                                          |
| Implement     | One implementer; step workers from `workerMinSteps` | Two implementers in two working copies where possible (T05); else one, and the record says so                                                     |
| Change review | Blocking, up to `MAX_CHANGE_REVIEWS_PER_RUN`        | Until no high finding is open, or two rounds without progress                                                                                     |
| Checks        | Rounds until no progress twice, within the budget   | The same                                                                                                                                          |
| Break         | —                                                   | The adversarial tester; its failures are a repair round                                                                                           |
| Pick          | —                                                   | Mechanical score, judge on ties (D14)                                                                                                             |
| Acceptance    | One more implement round                            | Until no progress                                                                                                                                 |
| Report        | One, from the implementer's side                    | Two: the implementer's and the reviewer's                                                                                                         |
| Models        | Each kind's row on the Subagents page               | Judging roles default to a different provider than the chat's model, when one is set up (a hint on the page says why)                             |

### 3.3 Stage records and the strip

A stage record is an empty assistant message with `meta.stage`:

```json
{
  "stage": {
    "task": "t_8f3a", // one id per message that started the task
    "name": "planReview", // plan | planReview | prepare | tests | implement | review | checks | break | pick | acceptance | deliver
    "status": "running", // running | done | failed | skipped
    "round": 2,
    "effort": "thorough",
    "candidate": "b", // Thorough: which working copy, when a stage runs per candidate
    "outcome": "plan-blocked", // when failed: the task's outcome
    "ref": "<message id>", // the record this stage produced (a review record, a verification record, a report)
    "summary": "3 findings, 1 high" // one line for the strip
  }
}
```

The strip (`AutopilotStrip.tsx`) shows the stages of the latest task in
order, with a tick, a spinner or a cross, the round count, and the
summary on hover; a click scrolls to the stage's record. The strip reads
the rows; it has no state of its own, so it is right after a restart and
in a mirrored remote chat.

### 3.4 The rules of an unattended run

`Permissions` as an Autopilot run gets them, whatever the user's settings
say, applied in one place (`autopilot::permissions_for`):

- `tool_call_checkpoint: None`; the loop guard's pause resumes once with
  its reason as a harness note, then ends the run as today.
- `allow_any_command: true` with `denied_commands` from the settings, plus
  the fixed refusals: commands naming `.rovik` (dev plan D8), `git` writes
  on a server (`agent_run/server.rs`), and everything the denylist names.
- Approvals: every rule that would ask the user asks the AI reviewer; what
  the reviewer rejects, can't decide or fails on is _refused_, with the
  reviewer's reason as the tool result. A resumed call after a crash runs
  again without asking. `verify_changes: true`, `checklist: Auto`.
- `ask_user` is refused (D5), and `MAX_QUESTIONS_PER_RUN` doesn't matter.
- The gate's detected checks run even when the allowlist wouldn't let the
  model run them (D7).

### 3.5 Settings and names

**The chat.** The mode picker beside the model picker offers Agent, Plan
and **Autopilot**. The composer's placeholder in Autopilot is "Describe the
task…". The strip of 3.3 appears when a task starts.

**Settings → Subagents** gets a section **Autopilot** above "Kinds":

- **Effort**: radios **Balanced** ("each stage once, with the subagents
  above") and **Thorough** ("several candidates, a second model for every
  judgment, an adversarial tester; takes longer and costs more").
- **Budget per task**: "Stop a task after [hours] or [cost]", empty for
  none. Defaults: 2 hours and 5 in the provider's currency for Balanced;
  none for Thorough.
- **Prepare dependencies before the first check** (checkbox, on).
- **Commands Autopilot may never run** (the denylist, one per line, with
  the defaults shown).

In "Kinds", the rows Autopilot uses carry a tag **Used by Autopilot**, and
the new rows appear: **Planner**, **Judge**, **Acceptance checker**,
**Adversarial tester**. A hint under the judging rows: "A different
provider than the chat's model catches more: their mistakes differ."

**Settings → Cloud hosts → While nobody is there** gets a third radio,
**Works on its own**: "Chats on this server run in Autopilot: they plan,
review, implement, check and deliver a task without asking. Nothing waits
for you. What the agent can't decide, it writes down." When chosen, a
block unfolds:

| Option    | Control                                                                                                                                                                                                                          | Default            |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ |
| Effort    | "As in Settings → Subagents", Balanced, Thorough                                                                                                                                                                                 | As in Settings     |
| Approvals | Fixed text: "The AI reviewer decides. What it can't approve is refused and reported."                                                                                                                                            |                    |
| Delivery  | Radios: "Keep the result on the server for this computer" or "Push to a branch `rovik/<task>` on the project's remote"; a "Deploy key on the server" path; a checkbox "Open a pull request (needs `gh` logged in on the server)" | Keep on the server |
| Budget    | "Stop a task after [hours] or [cost]"                                                                                                                                                                                            | As in Settings     |
| Notify    | "Call this URL when a task ends or is blocked"                                                                                                                                                                                   | Empty              |
| Test      | A button "Test delivery": the daemon pushes an empty branch `rovik/test-<date>` and deletes it, and reports                                                                                                                      |                    |

**Rust and protocol names.** `ChatMode = 'agent' | 'plan' | 'autopilot'`;
`StartRequest.mode = Some("autopilot")`. `AutopilotConfig` (new,
`rovik-protocol/src/autopilot.rs`) in `AgentConfig.autopilot`:

```rust
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutopilotConfig {
    pub effort: Effort,                 // Balanced (default) | Thorough
    pub budget: Budget,                 // max_hours: Option<f64>, max_cost: Option<f64>
    pub prepare: bool,                  // D19, default true
    pub denied_commands: Vec<String>,   // the denylist, with DEFAULT_AUTOPILOT_DENYLIST
}
```

`ApprovalPolicy` gains `Autonomous`; `HostSettings` gains
`autonomy: AutonomySettings { effort: Option<Effort>, budget: Option<Budget>, delivery: Delivery, notify_url: Option<String> }`,
and `Host` in `src/lib/hosts.ts` the same. `Delivery` is
`KeepOnHost` or `PushBranch { deploy_key: PathBuf, open_pr: bool }`; the
remote's URL comes from the project's `origin` on the client, with the
start request (`StartRequest.deliver_to: Option<String>`), because the
daemon's bare repository knows no remote. New daemon config keys in
`~/.rovik/config.toml`: none; the daemon takes everything with the
computer's `AgentConfig` and the start, as it does the policy today (dev
plan D16). The notify URL and the delivery settings ride in a new
per-computer `AutonomyConfig` frame beside `AgentConfig`, so a run started
by a computer that has since gone away still knows where to deliver.

### 3.6 On a server

- A chat on a "Works on its own" host starts in Autopilot unless the user
  picks another mode for that message. The client applies 3.4's rules and
  the host's overrides in `ApprovalPolicy::apply`, which already runs on
  every configuration the client sends a host.
- The daemon drives the pipeline itself: `autopilot::after_run` runs in
  the core on every `RunEnded`, so the next stage starts with no client
  connected. On start, `restart.rs` also calls `autopilot::resume` for
  every chat whose latest stage record is `running` or whose last stage
  ended without a `deliver` record.
- Delivery (D15) runs in `Results` after `commit`, before `ResultReady`.
- The notice (D18) is sent from `Tap::agent` on a task's end and on a
  `failed` stage record.
- The budget (D16) is enforced in the core; the daemon adds one thing: a
  run that ended `error` with `RateLimited` is retried by the daemon with
  backoff up to an hour, within the task's budget, instead of waiting for
  a human Retry.

Known limits on a server stay: no sandbox beyond the Unix account (dev
plan "Later"), API keys readable by the agent's commands, ports shared.
Autopilot runs more commands, so the sandbox moves up the "Later" list;
it isn't part of this plan.

---

## 4. Each stage

### 4.1 Plan, with criteria

Balanced: a Plan-mode run, as today, with `PLAN_MODE_INSTRUCTIONS`
extended in Autopilot by a "## Criteria" section: "numbered, testable
statements of what done means, each checkable by a test or by reading the
result; then **Out of scope** and **Assumptions**". `plan_problems`
requires the section in Autopilot (a plan without it goes back to the
planner, as a plan without steps does). `ask_user` is refused (D5).

Thorough: three **planner** subagents (new `Kind::Planner`, read-only
tools, `Format::Sections` with "Plan" as the one required section) run in
parallel with the same brief: the request, the merged criteria (4.1a), the
repo map. Their plans go to the **judge** (new `Kind::Judge`, read-only;
sections "Choice", "Plan", "Why"), which returns one plan, picked or
merged. The harness submits it through the `submit_plan` path, so the
plan's checks and review apply to it exactly as to a planner's own.

4.1a, Thorough's criteria: two planner subagents with a criteria-only
brief (sections "Criteria", "Out of scope", "Assumptions"), different
models when the settings have two; the judge merges them and lists where
they disagreed under "Assumptions", taking the narrower reading of the
request.

### 4.2 Plan review

The plan reviewer as today (`plan_review.rs`), with the prompt extended:
criteria that aren't testable, that miss part of the request or that go
beyond it are findings. Autopilot's cap replaces `MAX_PLAN_REVIEW_ROUNDS`:
3 rounds in Balanced, "two rounds without progress" in Thorough, where
progress means the set of open high and medium findings shrank. At the
cap with a high finding open, the plan isn't saved for implementation:
the task ends `plan-blocked` with the findings in the report. Medium and
low findings go to the "## Open review findings" section, as today, and
into the report.

### 4.3 Prepare

The harness, before the test writer. In the working copy: for each
lockfile found at the root or one folder down (`package-lock.json`,
`pnpm-lock.yaml`, `yarn.lock`, `Cargo.lock`, `requirements.txt`,
`uv.lock`, `go.sum`), when its dependency folder or cache is missing, the
install it implies runs as heavy work (`workload`), once, with the
project's check timeout. Output goes to `@checks/prepare` like a check's.
A failed install is a `prepare` record with `status: failed`; the pipeline
goes on, and the checks say what's missing. The stage reruns once when a
later check fails with an environment failure (D12).

### 4.4 Tests

The test writer (`test_writer.rs`) runs as an IDE-started subagent, from
the criteria and the plan, with `red_check` forced on. Its report and the
test files it wrote go into the stage record. The implement run's request
names them: "Tests that must pass: `path`, `path`; they fail now on
purpose."

### 4.5 Implement

The "Implement this plan" run, started by the harness with the text
`plan::implementation_request` builds (moved from `chatMode.ts` to the
core, so the daemon can build it; the frontend's "Implement this plan"
button sends `meta.implementsPlan` and the plan's path, and the core
expands the text). Step workers carry plans from `workerMinSteps` steps,
and a light check (`project::light_check_for`) runs after each worker's
step in `after_worker`, with its failures handed to the next step as a
note. Thorough: two implement runs in two working copies (T05), each with
the full stages 4.6 to 4.8, before Pick.

### 4.6 Change review

As today, but blocking (D10). A new tool in Autopilot's implement run,
`answer_review`, takes `{ "responses": [ { "finding": 1, "status": "fixed" | "not-a-problem", "note": "…" } ] }`
and is the only way to answer; free text isn't read. The follow-up review
gets the responses, as the plan review's B13 brief does, and must mark
each high finding resolved. The caps: `MAX_CHANGE_REVIEWS_PER_RUN` in
Balanced; "two reviews without progress" in Thorough. A high finding open
at the cap ends the task `review-open`.

### 4.7 Checks

The verify gate with the floor of D7 and the rounds of D12. Also, in
Autopilot: a check that times out is re-run as the scoped command for the
changed files (`project::test_command_for`) and the full suite is recorded
as `slow`, not failed; the gate's command output is captured whole for
failure keys (today 64 KB, which hides middle errors).

### 4.8 Break (Thorough)

The **adversarial tester** (new `Kind::Breaker`): the test writer's tools
and write scope (test paths only), check-only bash as a worker's, a brief
of the criteria, the plan and the diff, and the instruction to write
tests that a wrong implementation would fail: edge cases, error paths,
concurrency, the boundaries the criteria name. Tests that fail on the
change are findings: they stay in the tree and the gate's next round is a
repair round with them. Tests that pass stay as regression tests when
they test a criterion, and are removed when the tester marked them
"speculative". The gate's "fewer tests" and integrity flags apply to the
implementer's later rounds as usual.

### 4.9 Pick (Thorough)

For each candidate working copy: criteria tests passed, project checks
passed, open high and medium findings, diff lines. Ordered as D14 says;
the judge gets the two diffs, the criteria and both review records only
on a tie. The losing candidate's diff and records are kept in its stage
records and a one-line note goes in the report. The winner's working copy
becomes the chat's.

### 4.10 Acceptance

The **acceptance checker** (new `Kind::Acceptance`, read-only; sections
"Verdicts", "Evidence", "Unmet"): the criteria, the diff, the checks'
latest results, the test writer's report. "Verdicts" is one line per
criterion, `1. satisfied — <evidence>` or `1. unsatisfied — <what's missing>`,
parsed strictly; an unparseable reply gets the S13 repair turn and is then
recorded `skipped`, never satisfied. Unsatisfied criteria go to one more
implement run as its request (Balanced), or until two rounds without
progress (Thorough), each followed by the checks and a new acceptance.
Still unsatisfied: `unmet`.

### 4.11 Deliver and report

Local: the chat's changes land as today (`try_land`), with the Autopilot
mapping of outcomes (D7). Server: `Results::commit` as today, then the
push and the pull request (D15), then `ResultReady`. Then a words-only
turn (`words_only_turn`) in the implementer's context writes the
**report**, saved as the final assistant message with `meta.report`, and
as the pull request's description: what changed and why, the assumptions
made (first), how it was verified (checks, the tests written, the
acceptance verdicts), what is unmet or open, cost and time. Thorough adds
a second report from the change reviewer's context. The notice (D18) goes
out last.

---

## 5. Milestones

### Milestone A: the mode and the driver

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                | Needs | Done when                                                                                                                                                                                               |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A01 | `ChatMode` gains `'autopilot'` (`src/lib/chatMode.ts`); the mode picker and the composer's placeholder in `ChatPanel.tsx`; `StartRequest.mode = "autopilot"`. The core reads it as `autopilot: bool` on `Run`, and treats it as Plan mode for the first run of a task (A03). `Autopilot` in the protocol: `rovik-protocol/src/autopilot.rs` with `AutopilotConfig`, `Effort`, `Budget`; `AgentConfig.autopilot` with a default                      | —     | `npm test` for the picker; protocol round-trip test; e2e: a message sent in Autopilot starts a Plan-mode run with `autopilot` set                                                                       |
| A02 | Stage records: `agent_run/autopilot.rs` with `Stage`, `StageRecord` (3.3), `records_of(task)`, `next_stage(records, effort)`, and `add_stage`/`end_stage` writing `meta.stage` rows. The pipeline's state is read back from the rows only                                                                                                                                                                                                           | A01   | Unit tests: `next_stage` for every status of every stage in both efforts; records survive a reload (e2e reads them back)                                                                                |
| A03 | The driver: `autopilot::after_run(core, dialog_id, outcome)` called where `RunEnded` is emitted, after landing's decision; it ends the current stage with the outcome, decides the next (A02), and starts it: a run via `agent_start` with a harness message, or an IDE-started subagent via `run_ide_subagent`. `autopilot::resume` at app start and daemon start. A stage is recorded `running` before it starts, so a crash mid-start repeats it | A02   | e2e on the scripted provider: Plan → Plan review (clean) → Implement → Checks → Deliver, each with a record; `kill` between two stages, then `resume` continues; a second `resume` starts nothing twice |
| A04 | The plan stage: the Plan-mode run as the task's first run, `ask_user` refused in Autopilot (D5), the system prompt's clarifying-question line replaced; the implement stage: `plan::implementation_request` in the core, the frontend's button sending `meta.implementsPlan` and the path; the harness starts the implement run when the plan review is clean                                                                                       | A03   | e2e: a plan that asks gets the refusal and submits with assumptions; the implement run starts without a user message and gets the same text the button used to send (a test compares both)              |
| A05 | `AutopilotStrip.tsx` above the composer, reading the stage records; `StageRecord.tsx` for the records in the chat (like `VerificationRecord.tsx`); the strip in a mirrored remote chat                                                                                                                                                                                                                                                              | A02   | `npm test` for the strip's states; manual check locally and on a same-machine daemon                                                                                                                    |
| A06 | Effort setting: the Autopilot section on the Subagents page (3.5) in `SubagentSettings.tsx` and `src/lib/autopilotSettings.ts`, sent in `AgentConfig.autopilot`; `next_stage` takes the effort                                                                                                                                                                                                                                                      | A02   | `npm test` for the section; e2e: Thorough records the Thorough-only stages as `skipped` until T builds them                                                                                             |
| A07 | D17: `autopilot::subagent_config(user's SubagentConfig, effort)` enables the kinds the pipeline needs for its runs, with the user's rows' models and the default model as fallback; the "Used by Autopilot" tag on the rows                                                                                                                                                                                                                         | A06   | Unit test: every needed kind is enabled in the config an Autopilot run gets, the user's models kept; an ordinary chat's config is untouched                                                             |

### Milestone U: the rules of an unattended run

| ID  | Task                                                                                                                                                                                                                                                                                                                              | Needs | Done when                                                                                                                                               |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| U01 | `autopilot::permissions_for(user's Permissions, AutopilotConfig)`: 3.4's rules; `DEFAULT_AUTOPILOT_DENYLIST` in the protocol (`rm -rf /`, `git push`, `git commit --amend`, `curl … \| sh`, `sudo`, `shutdown`, `dd`); applied to every run and subagent of a task                                                                | A03   | Unit tests on the function; e2e: a run in Autopilot never ends `paused-checkpoint`; a denied command is refused with the reason                         |
| U02 | Approvals in Autopilot: rules that would ask the user ask the AI reviewer when `approval_profiles` has one; the reviewer's reject, unsure and failure become a refusal with the reason (`execute.rs`, where it falls back to the user today); with no reviewer profile, an approval is refused at once. A resumed call runs again | U01   | e2e: a risky command with the reviewer unsure is refused, the run goes on; a crash during a tool call, then `Resume`, runs the call without an approval |
| U03 | The loop guard's pause resumes once in Autopilot with a harness note ("You repeated …; change approach"), then ends the run as today; `wait_for_approval` gets a deadline in Autopilot (default 10 minutes, for the states U02 doesn't cover) that denies and records                                                             | U01   | e2e: a looping scripted model is resumed once and the second pause ends the run; an approval nobody answers is denied at the deadline with a record     |
| U04 | Budget (D16): `agent_run/budget.rs` accumulating cost and wall time per task from the usage providers report (the headless runner's accounting moved into the core); checked per reply and between stages; outcome `budget`; `pending_reason` for it                                                                              | A03   | e2e: a tiny budget ends the task `budget` at the next reply, with a deliver stage and a report                                                          |

### Milestone V: the verification floor

| ID  | Task                                                                                                                                                                                                                                                                                                                 | Needs | Done when                                                                                                                                                                            |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| V01 | In Autopilot the gate's detected checks run regardless of the model's allowlist (`verify.rs`, where a refused check is skipped today); `verify-needs-approval` can't happen in Autopilot                                                                                                                             | U01   | e2e: a Python project with `pytest` not in the allowlist is checked; the same run outside Autopilot still skips it                                                                   |
| V02 | D7: `unverified` and `verify-unverified` don't land in Autopilot (`landing.rs`, an Autopilot-aware `pending_reason`); with no detected check, the commands under the plan's "## Validation" are the checks, filtered by the denylist and recorded as such                                                            | A04   | e2e: a project with no checks gets the plan's validation commands as checks; a failing one gives a repair round; a plan without runnable commands ends `unverified` and doesn't land |
| V03 | D12: environment failures (exit 127, "command not found", "Cannot find module", "No module named", "could not find `Cargo.toml`") classified in `command_output.rs`; in Autopilot they rerun Prepare once and don't count as a round; rounds in Autopilot end on two without progress or at the budget, not at three | U04   | Unit tests on the classifier; e2e: "vitest: not found" → prepare → the check runs; a round that only uncovers hidden failures doesn't count                                          |
| V04 | **General:** after every `write`, `outline::syntax_errors` runs on the written file where the language is known, and errors are appended to the tool result                                                                                                                                                          | —     | Unit test: a write that breaks a Rust file returns the error in its result; a file in an unknown language is unaffected; the e2e suites pass unchanged                               |
| V05 | **General:** `repair::roll_back` puts back only the files behind the keys in `judgement.broke` (via `causes.rs`), not every checkpointed file; the round still counts                                                                                                                                                | —     | e2e in `repair_e2e_tests.rs`: a round that fixes two checks and breaks one keeps the two fixes                                                                                       |
| V06 | A check that times out in Autopilot reruns as the scoped command (`project::test_command_for`) for the changed files; the full suite is recorded `slow`, not failed                                                                                                                                                  | V03   | e2e with a short test timeout: the scoped rerun's result is the round's                                                                                                              |
| V07 | The gate's command output is captured whole for `failure_keys` (a separate, larger limit for check jobs than `run_command`'s 64 KB); the saved `@checks/` file is unchanged                                                                                                                                          | —     | Unit test: keys from a 200 KB output include the middle errors                                                                                                                       |
| V08 | **General:** `tests_ran` parses `go test -v` output, so the "fewer tests" flag works for Go                                                                                                                                                                                                                          | —     | Parser tests                                                                                                                                                                         |
| V09 | `project_profile` detects a stack one folder down (a monorepo's `api/` and `web/`), and the checks run per folder; with several, the gate runs them all                                                                                                                                                              | —     | Detection tests on fixtures; e2e with two packages                                                                                                                                   |

### Milestone S: the Balanced stages

| ID  | Task                                                                                                                                                                                                                                                                                                                        | Needs    | Done when                                                                                                                                                                                                            |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S01 | Criteria (D8): `PLAN_MODE_INSTRUCTIONS`' Autopilot addition; `plan_problems` requires "## Criteria" in Autopilot; `plan::criteria(text)` parses the numbered statements, "Out of scope" and "Assumptions"; the plan reviewer's prompt checks them                                                                           | A04      | Parser tests; e2e: a plan without criteria goes back; the reviewer's brief has them                                                                                                                                  |
| S02 | Plan review in Autopilot (4.2): the Balanced cap of 3; `plan-blocked` at the cap with a high finding open; the task's report carries the findings                                                                                                                                                                           | S01      | e2e: a high finding the planner never fixes ends the task `plan-blocked` after 3 rounds, with a report and no implement run                                                                                          |
| S03 | Prepare (4.3): `agent_run/prepare.rs`, lockfile detection, the install as heavy work, the record and `@checks/prepare`; the `prepare` setting                                                                                                                                                                               | A03      | Unit tests on detection; e2e with a fixture whose `node_modules` is missing: the install runs once and the checks pass                                                                                               |
| S04 | Tests first (4.4): the test writer as an IDE-started subagent after the plan review, `red_check` forced; its files in the stage record and in the implement run's request                                                                                                                                                   | S02      | e2e: the test writer runs before the implement run; the implement run's request names its files; a blocked test writer is recorded and the pipeline goes on                                                          |
| S05 | Per-step light checks: `light_check_for` after each sequential worker in `after_worker`, failures to the next step as a note                                                                                                                                                                                                | A04      | e2e: a worker's step that breaks a file's syntax gets the diagnostics into the lead's next note                                                                                                                      |
| S06 | Blocking change review (4.6): the `answer_review` tool in Autopilot's implement run; `change_review.rs` reads responses instead of free text; the follow-up review must resolve high findings; `review-open` at the cap                                                                                                     | A04      | e2e: a high finding answered `not-a-problem` without the follow-up review agreeing is still open; at the cap the task ends `review-open`; a `fixed` finding the follow-up confirms lets the checks run               |
| S07 | Test-exists rule (no model): a round whose changed files include non-test files and no test file (`project::is_test_path`) starts the test writer again in Autopilot, once per task, before acceptance                                                                                                                      | S04      | e2e: an implementation without test changes gets a second test writer run                                                                                                                                            |
| S08 | Acceptance (4.10): `Kind::Acceptance`, its `KindSpec`, brief and strict parser; the hook after the checks pass (beside `review_after_green`); one implement round on unsatisfied criteria; `unmet`                                                                                                                          | S01, S06 | Parser tests on good, partial and garbage verdicts (garbage → `skipped`); e2e: an unsatisfied criterion gives one more implement round and a second acceptance; still unsatisfied ends `unmet`, delivered as partial |
| S09 | The report (4.11): `words_only_turn` with the report brief; `meta.report`; the frontend shows it as the final message with its sections; `pending_reason` and landing for every Autopilot outcome (D7): `done`/`verified` land; `unmet`, `budget`, `verify-flagged` deliver as partial without landing; the rest don't land | S08, U04 | e2e: the report names the assumptions, the tests, the verdicts and the cost; a partial result isn't landed locally but its report says why                                                                           |
| S10 | The headless runner takes `--mode autopilot --effort balanced\|thorough` and reports the stages and the task's outcome                                                                                                                                                                                                      | S09      | Headless e2e on the scripted provider: a Balanced task end to end, stages in the report                                                                                                                              |

### Milestone H: hosts that work on their own

| ID  | Task                                                                                                                                                                                                                                                                                                                              | Needs    | Done when                                                                                                                                                                                                               |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H01 | `ApprovalPolicy::Autonomous`; `AutonomySettings` on `HostSettings` and `Host`; the third radio and its block in `CloudHostsSettings.tsx` (3.5), saved through `hosts_page.rs`                                                                                                                                                     | A06      | `npm test` for the block; settings round-trip test; an old settings file without the block loads                                                                                                                        |
| H02 | `ApprovalPolicy::apply` for `Autonomous`: U01's permissions, the effort and budget overrides, the kinds of A07; a chat on such a host starts in Autopilot unless the message says otherwise; the `AutonomyConfig` frame beside `AgentConfig` (3.5), stored per computer by the daemon                                             | H01, U01 | Client test: the configuration a "Works on its own" host receives; daemon test: the frame is stored and read back after a restart                                                                                       |
| H03 | The pipeline on the daemon: `autopilot::after_run` from the daemon's `RunEnded` path; `autopilot::resume` in `restart.rs`; `agent_start` of a stage's run on the daemon with no client connected                                                                                                                                  | A03, H02 | Daemon test: a task started by a client that disconnects runs to `deliver` and its records are there when the client returns; `kill -9` between stages, then the daemon continues                                       |
| H04 | Delivery (D15): `results::deliver` after `commit`: push the result commit to `rovik/<slug>` on `deliver_to` with the deploy key (git2 over SSH with the key, or the system `git` when the host is set to it); `gh pr create` when `open_pr` and `gh` is logged in; the branch and the PR's URL in the report and in `ResultReady` | H03      | Daemon test against a local bare "remote" over a file URL and an SSH test server: the branch exists with the result; a push that fails is a `deliver` record with `failed` and the result ref still comes to the client |
| H05 | "Test delivery" (3.5): a daemon request that pushes and deletes `rovik/test-<date>`                                                                                                                                                                                                                                               | H04      | Daemon test; the hosts page shows the result                                                                                                                                                                            |
| H06 | Notify (D18): `POST notify_url` with `{ chat, task, stage, outcome, summary, branch, pr }` from `Tap::agent` on a task's end and on a failed stage; three retries with backoff; failures logged, never fatal                                                                                                                      | H03      | Daemon test with a local HTTP server: one call per end, the body as specified; an unreachable URL doesn't stop the task                                                                                                 |
| H07 | Rate limits on the daemon: a stage's run that ended `error` with `RateLimited` is retried by the daemon with backoff up to an hour, within the task's budget                                                                                                                                                                      | U04, H03 | Daemon test with the scripted provider returning 429: the run is retried and the task completes                                                                                                                         |
| H08 | `rovik-agentd status --json`: this user's chats from `state.json`, with the latest stage, the outcome, the running run and the task's cost from the agent log; the hosts page shows the same per chat                                                                                                                             | H03      | CLI test on a daemon data folder; `npm test` for the list                                                                                                                                                               |

### Milestone T: Thorough

| ID  | Task                                                                                                                                                                                                                                                                                                       | Needs              | Done when                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T01 | `Kind::Planner`: read-only tools, `Format::Sections` ("Plan"), the brief of 4.1; three run in parallel through `run_ide_subagent`; the Subagents row                                                                                                                                                       | S02                | e2e: three planner results in three records                                                                                                                                     |
| T02 | `Kind::Judge`: read-only; sections "Choice", "Plan", "Why" for plans, "Verdict", "Why" for candidates (T06); the picked or merged plan is submitted through the `submit_plan` path (`plan_problems`, then the review)                                                                                      | T01                | e2e: the judge's plan goes through the plan checks and the plan review; a judge that fails falls back to the first planner's plan with a note                                   |
| T03 | Criteria twice (4.1a): two planner subagents with the criteria brief and different models when two are set up; the judge merges and records disagreements                                                                                                                                                  | T02                | e2e: two differing criteria sets give a merged set with the disagreement under "Assumptions"                                                                                    |
| T04 | `Kind::Breaker` (4.8): the test writer's tools and scope, check-only bash; the stage after the checks pass; its failing tests become a repair round; speculative passing tests are removed                                                                                                                 | S08                | e2e: the breaker's failing test leads to a repair round that makes it pass; a speculative passing test is gone at delivery; a breaker that writes outside test paths is refused |
| T05 | Two working copies: locally a child overlay through the parallel-group machinery (`group.rs`) when the driver is there; on the daemon a sibling worktree per candidate (`work.rs`, `candidate_worktree`), with its own result commit; where neither is possible, one implementer and a record that says so | A04                | Daemon test: two candidate worktrees, two implement runs, two result commits; local e2e with overlays; a host without a second copy records the fallback                        |
| T06 | Pick (4.9, D14): the mechanical score from the candidates' records, the judge on ties, the winner's working copy becomes the chat's, the loser's records kept                                                                                                                                              | T02, T05           | Unit tests on the score's ordering; e2e: a tie goes to the judge; the loser's diff is in its record                                                                             |
| T07 | Thorough's loops (3.2): plan review, change review, repair and acceptance end on two rounds without progress or at the budget; `progress` defined per loop (open high and medium findings; failing keys; unsatisfied criteria)                                                                             | S02, S06, S08, U04 | e2e per loop: progress continues the loop; two rounds without it end the stage; the budget ends it first when smaller                                                           |
| T08 | The reviewer's report: a second `words_only_turn` in the change reviewer's context, appended to the report under "The reviewer's view"                                                                                                                                                                     | S09                | e2e: the report has both parts                                                                                                                                                  |
| T09 | Judging roles default to a different provider: `autopilotSettings.ts` picks, for planner, judge, acceptance and breaker rows left on "Default", the first enabled model of a provider other than the chat model's, when there is one; the hint on the page                                                 | A06                | `npm test`: with two providers the judging rows resolve to the other one; with one, to the default                                                                              |

### Milestone G: measure

| ID  | Task                                                                                                                                                                                                                 | Needs | Done when                                                                                                         |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ----------------------------------------------------------------------------------------------------------------- |
| G01 | `.github/workflows/ci.yml` on pull requests: `cargo test --workspace`, `npm test`, `npm run typecheck`, `npm run format:check`, `node scripts/eval/eval.mjs dry-run`                                                 | —     | The workflow passes on a pull request                                                                             |
| G02 | A `tool` line per finished tool call in the agent log (name, failed, error class such as `old_text not found`, ms); the headless report and `result.json` carry `toolErrors`, `editMisses`, `verifyRounds`, `stages` | S10   | Log tests; `summarize()` shows the new columns                                                                    |
| G03 | Eval variants `autopilot-balanced` and `autopilot-thorough` beside the kind variants; `compare` over any two campaigns; pass@k and pass^k beside the mean                                                            | S10   | `dry-run-variants` runs them on the scripted model; a real pilot on the 30 tasks is run and its summary committed |

---

## 6. Implementation order for an AI agent

### Rules for every task

1. **One task per branch and pull request.** Don't start the next until
   the current one is merged.
2. **Read first:** this plan's sections 1 to 4, the task's row, and the
   files it names; `subagents-plan.md` for a subagent task, the cloud dev
   plan's sections 5 and 6 for an H task. Follow the surrounding code's
   style: module doc comments that explain _why_, small functions, tests
   next to the code.
3. **Keep the app working.** Nothing changes for a chat that isn't in
   Autopilot or on a "Works on its own" host, except V04, V05 and V08,
   which say so in their pull requests. The existing suites pass
   unchanged.
4. **Checks before every push:**
   - `cd src-tauri && cargo check --workspace && cargo test --workspace`
   - `npm test`
   - `npm run typecheck`
   - `npx prettier --check <the files you added or changed>`
5. **Tests in the same task.** Each row's "Done when" names them. A stage
   gets an e2e run on the scripted provider; a parser gets good, partial
   and garbage inputs; a daemon change gets a daemon test.
6. **Model output is data.** Every verdict, finding and verdict list is
   parsed strictly; what doesn't parse is `skipped`, never `clean`,
   `satisfied` or `fixed`. Reports and briefs quote file contents as data
   (subagents plan D16).
7. **No new human-wait state.** A task that would add one adds an outcome
   and a report instead, and `pending_reason` learns it.
8. **When a task doesn't fit reality,** don't improvise a design. Add a
   note under "Notes from implementation", take the smallest safe path,
   and say so in the pull request.
9. **Protocol changes are additive**, or raise `PROTOCOL` and say why.
   Every new frame gets a round-trip test.
10. **Secrets:** the deploy key's path is stored, never its contents; the
    notify URL may carry a token and is never logged whole.

### Order at a glance

```
A:  A01 ─► A02 ─► A03 ─► A04 ─► A05;  A06 ─► A07 after A02        the mode, the driver, the strip, the effort
U:  U01 (after A03) ─► U02 ─► U03;  U04 after A03                   nothing waits
V:  V01 (after U01), V02 (after A04), V03 (after U04), V06 after V03; V04, V05, V07, V08, V09 any time
S:  S01 (after A04) ─► S02 ─► S04 ─► S06 ─► S08 ─► S09 ─► S10;  S03 after A03;  S05 after A04;  S07 after S04
H:  H01 (after A06) ─► H02 (after U01) ─► H03 ─► H04 ─► H05;  H06, H07, H08 after H03
T:  T01 (after S02) ─► T02 ─► T03;  T04 after S08;  T05 after A04 ─► T06 (after T02);  T07 after S08 and U04;  T08 after S09;  T09 after A06
G:  G01 any time;  G02 and G03 after S10
```

After A, U, V and S, one message in Autopilot runs a Balanced task to a
report on a local chat and in the headless runner. H puts the same on a
server with nobody connected, and delivers it. T adds Thorough. G measures
both against the baseline before either becomes a default anywhere.

A03 is the riskiest single change: it threads a state machine through the
run's end and the daemon's restart. V05 is the riskiest general change.
T05 depends most on the environment it meets.

---

## 7. Testing

- **Unit tests** next to the code: `next_stage`, the parsers (criteria,
  verdicts, judge choices, responses), the score's ordering, the
  environment-failure classifier, the lockfile detection.
- **Core e2e** on the scripted provider (`agent_run/e2e_tests.rs`'s
  harness): a Balanced task end to end; each stage's failure outcome; a
  resume after a kill between any two stages; a budget stop; the blocking
  review's three paths; acceptance's two rounds; Thorough's candidates,
  tie and breaker.
- **Daemon tests** (`crates/rovik-agentd/tests/`): a task that runs to
  delivery with the client gone; `kill -9` between stages; delivery to a
  local bare remote over a file URL and over the in-process SSH server;
  the notice to a local HTTP server; a 429 retried.
- **Headless**: a Balanced and a Thorough task on the scripted provider,
  with the stages in the report.
- **Frontend**: the picker, the strip's states, the Autopilot section, the
  host block, the hosts page's chat list.
- **Faults to inject:** the daemon killed during a stage's subagent; the
  reviewer model down for a whole stage (recorded `skipped`, the task
  goes on, the report says so); a plan with no "## Criteria"; a push
  refused by the remote; a judge that returns nothing parseable; a
  working copy that can't be made for the second candidate.
- **Manual matrix:** a Node project with no tests; a Rust workspace; a
  Python project with `pytest` only; a monorepo with two packages; a
  project whose `origin` needs the deploy key; a "Works on its own" host
  with the laptop closed for the whole task.

---

## 8. Risks and known limits

- **Cost and time.** A Balanced task costs two to three plain Agent runs;
  a Thorough one costs several times that and takes hours. The budget is
  the bound, and G03 says whether the extra stages pay for themselves
  before anything becomes a default.
- **Parsing.** Every new stage adds a verdict a model writes and the
  harness parses. Rule 6 keeps a parse failure from ever counting as a
  pass, and `skipped` stages are visible in the strip and the report.
- **Two working copies.** Thorough's second implementer needs a child
  overlay or a sibling worktree; without either (a server without FUSE
  _and_ a project the daemon can't worktree twice) Thorough runs one
  implementer and says so.
- **The deploy key.** It sits on the server, readable by the agent's
  commands, as the API keys are. Use a key that may push only `rovik/*`
  branches where the forge allows it; the sandbox stays on the "Later"
  list.
- **Validation commands as checks (V02).** Commands the model wrote run
  unattended. They come from a reviewed plan and pass the denylist; a
  project with no checks of its own is where the agent's output is least
  trustworthy anyway, so the alternative, landing unverified, is worse.
- **Assumptions.** No question is asked, so a wrong reading of the request
  costs a task. Criteria written twice (Thorough) and the report's
  "Assumptions" first keep the cost to one task.
- **General changes (V04, V05, V08)** alter behaviour for every chat. Each
  has its own task and tests, and each is small.
- **Known limits of v1:** no per-chat effort; no smoke test of the running
  product (a design note under "Later" when T is done: long-lived
  processes, port discovery, then the browser tools); no email or push
  notices; no CLI to start a task without the app; no sandbox.

---

## 9. Decided questions

| Question                                      | Decision                                                                                                                        | Where    |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- | -------- |
| A mode, a host policy, or both?               | **Both**: the mode does the work and runs anywhere; the host policy chooses it by default and adds delivery, notice and budget. | D1, H02  |
| One long run or a pipeline of runs?           | **A pipeline**: fresh contexts and existing code paths per stage.                                                               | D2       |
| Where does the pipeline's state live?         | **In stage records**, rows in the chat.                                                                                         | D3       |
| Criteria by a separate writer or in the plan? | **In the plan** for Balanced; **two writers and a judge** for Thorough.                                                         | D8, T03  |
| Tests before or after the implementation?     | **Before**, from the criteria, red check on.                                                                                    | D9       |
| Is the change review blocking?                | **Yes**, with structured answers and a follow-up review.                                                                        | D10      |
| How are candidates judged?                    | **Mechanically**, the judge on ties.                                                                                            | D14      |
| Does Autopilot ever ask the user?             | **Never.**                                                                                                                      | D5       |
| What bounds a loop?                           | **Progress and the budget**; a count only in Balanced where one exists today.                                                   | D12, T07 |
| Where is the effort set?                      | **Settings → Subagents → Autopilot**, overridden per host.                                                                      | D4       |
| What leaves a server?                         | **A branch, and a pull request when `gh` is there**; the result ref stays for the laptop.                                       | D15      |
| Changes for everyone?                         | **Three**, named, each its own task.                                                                                            | D20      |

## Notes from implementation

### A01–A03: the mode, the records, the driver

- **The denylist (A01).** `DEFAULT_AUTOPILOT_DENYLIST` has the entries
  `Permissions::denied_commands` can match today. "A download piped to a
  shell" is a rule across a pipe, which an entry (a program and its words)
  can't say: U01's.
- **The task's id** is the id of the message that started it, so a task's
  request is found from its records.
- **Stages inside runs.** The plan review runs inside the plan's run (at
  `submit_plan`), the change review and the checks at the end of the
  implementing run, as they do today. The driver records those stages
  from what the run left (`meta.review`, `meta.verification`) once the
  run ends, with the reviewer's or the gate's last round as the record's
  round. Meanwhile, the strip shows the run's own stage as running. A run
  stage's record keeps its run's outcome (`outcome` on a done record too),
  which the checks' record is derived from after a restart.
- **Stages not built yet** (prepare, tests, acceptance; Thorough's break
  and pick) are recorded `skipped`, "Not available yet", as A06 does for
  Thorough's.
- **Landing** still happens at the end of the implementing run
  (`land_at_end`); the delivery stage records what became of the result
  and the task's outcome. S09 moves landing to the delivery.
- **Fresh contexts (D2) aren't built.** A stage's run sees the chat's
  history, trimmed, as today's "Implement this plan" run does: the history
  builder has no cut-off point, and one changes what every run is sent.
  The implementation's request is the plan in full, as the button sends
  it; it starts from the IDE's message (`meta.harness: "autopilot"`,
  `meta.autopilot.record`), which is never marked failed, so a Retry
  still has it.
- **Stop and Retry.** A stage the user stopped ends the task without a
  delivery: they're there. A Retry or a Continue of a run stage that a
  failure (`error`, `repetition`, `stopped`) ended opens that stage again,
  as its next round, and the pipeline goes on from it. A Resume never
  does.
- **Waits that remain.** A run that pauses (checkpoint, loop guard, move,
  drain) or whose checks wait for approval keeps its stage running, as the
  run itself waits; U01–U03 remove those waits in Autopilot.
- **Resume.** Locally only the frontend holds the model's key, so a task
  resumes when its chat is opened, through a `Resume` start, as
  interrupted tool calls do (`resumesHere`). The daemon resumes at its
  start (`restart.rs`): a run cut mid-stage is restarted as before and
  its stage goes on; a run cut where no start picks it up, and a task cut
  between two stages, go on through `autopilot::resume`, counted as
  restarts. H03 adds the rest of the server's side (a client that
  disconnects, a `kill -9` between stages, results and `ResultReady`).
- **One driver per chat.** A run's end takes the chat's driver lock
  before `RunEnded` goes out, so a Resume sent when the app hears of it
  finds the driver at work and starts nothing.
- **IDE-started subagents as stages** (S04's test writer, S08's
  acceptance) need a lead run: `run_ide_subagent` is a method of one. No
  stage of milestone A needs it.

### A04: the plan and implement stages

- **No questions.** Autopilot's runs aren't offered `ask_user`, and one
  called anyway is refused with "nobody answers questions in Autopilot"
  and where to put the assumption, rather than with Agent mode's text,
  which says the tool is for Plan mode (the plan stage is Plan mode). The
  system prompt's line on clarifying questions, and Plan mode's lines on
  asking and on the user's choice of mode, have Autopilot's in their
  place (`agent::lead_system_prompt`, `plan::plan_mode_instructions`).
- **"Implement this plan" sends the plan's path** (`meta.planPath`, with
  `meta.implementsPlan`): the core reads the file, writes the request
  (`plan::expand_implementation`) and stores it as the message's
  `meta.sentContent`, as the frontend did. A remote chat's plan is read
  on its host, which also shows its path from the worktree's root. The
  editor saves a plan before it hands it over, so the file is what it
  shows.
- **The same text, with the map elsewhere.** The driver's run and the
  button's get the same request text. The repository map goes on the
  message that starts the request: the button's own, or in Autopilot the
  task's message, since the IDE's message starts none (it's a harness
  message, never taken for the user's request).

### A05: the strip and the records

- **The task the strip shows** is the chat's latest while it's the
  chat's current request: a message of the user's after it ends it there
  too (its records stay in the chat), as it does for a Resume.
- **It reads the whole task.** A task's runs are longer than the page of
  messages the app holds, so the strip reads its records from the
  database (`chat_task_stages`, a remote chat's mirror's) when the chat
  opens, a run starts or ends, or a record comes or changes; the records
  in the messages held are the live ones over those read. A chat opened
  around a search hit holds an older page, and the read alone says which
  task is current.
- **A record recorded running with nothing running** shows as "didn't
  finish" with a pause icon: the app closed or a crash cut the stage, and
  opening the chat resumes it (the daemon resumes its own at its start).
  Only the current task's latest record is at work while a run goes: one
  a new request took over from stays "didn't finish".
- **In the chat** each record is one line where its stage began: the
  stage, its round when it ran more than once, its status, and its
  summary. A click on the strip scrolls to it, loading the page around
  it when it isn't held, once (the messages that come after don't scroll
  back to it).
- **Checked by hand** in a browser (the components with the app's
  styles, at a side panel's width): the app itself and a same-machine
  daemon couldn't be run in the sandbox this was built in. The daemon's
  test (`rovik-agentd/tests/autopilot.rs`) checks the rows a remote
  chat's mirror gets.

### A06: the effort setting

- **The section has the effort alone for now.** Its other settings (the
  budget, preparing the dependencies, the commands Autopilot may never
  run) come with the tasks that make them do something (U04, S03, U01);
  until then the backend's defaults stand for them. A setting that does
  nothing yet isn't shown.
- **Saved with the Subagents settings** (`subagents.autopilot` in the AI
  settings file), so a file that couldn't be read is never written over
  with the defaults (P5.7). It's sent apart, as `AgentConfig.autopilot`.
- **Not greyed out** when "Use subagents" is off: Autopilot's chats use
  it either way.
- **"With the subagents below"**: the section is above "Kinds", so the
  Balanced hint says below where 3.5 says above.
- **Thorough says what it does today.** Until T builds its stages, a
  Thorough task runs as a Balanced one, with Break and Pick recorded
  skipped; a line under the radios says so while Thorough is chosen.
- **A task keeps the effort it started with** (its records carry it): a
  change of the setting applies to the next task.

### A07: the subagents a task uses

- **The kinds turned on** are the ones D17 names that exist: the plan
  reviewer, the change reviewer, the failure triage, the test writer and
  the step workers (`autopilot::needed_kinds`). The acceptance checker
  (S08) and Thorough's planner, judge and adversarial tester (T01, T02,
  T04) join the list as they're built. The scout isn't in D17's list: the
  user's switch governs it, and only while "Use subagents" is on.
- **Where it applies.** A run that carries out a stage is marked when it
  starts (`set_autopilot`, with its task's effort), and `config_for` gives
  it and its subagents the configuration with those kinds on; an ordinary
  chat's, and the Autopilot chat's between runs, is the user's. The
  driver's records use the same, so a review that didn't run says "Not
  reviewed", not that the reviewer is off.
- **The models** are the rows': the app resolves every row's models
  whether the row is on or not (its model and fallbacks, the default
  model, the chat's), and a row it didn't send gets the chat's model. The
  user's limits (per run, at once, the review's size, the workers' steps)
  stand.
- **The page.** With "Use subagents" off, a row Autopilot uses keeps its
  model and effort to set (its switch, which is for ordinary chats,
  doesn't), and so does the default model, which those rows fall back
  to: otherwise "the rows there give the models" couldn't be done without
  turning subagents on for every chat. Such a row's models from another
  provider than the chat's say "In Autopilot, code will also be sent to …".

### U01: the rules of an unattended run

- **One place.** `autopilot::permissions_for` makes the user's
  permissions an unattended run's (no checkpoint, any command but the
  denylist's, the checks on, the checklist on Auto), and `config_for`
  gives it, with A07's kinds, to every run that carries out a stage and
  to that run's subagents. The run reads it from its start: the checkpoint
  and the checks are set when it's prepared.
- **The denylist** is the user's denied commands and the settings' list,
  each entry once. An entry can now be entries joined by `|`, one piped
  into the next (`denied_entry`), which is how the defaults say "a
  download run by a shell": `curl | sh`, `curl | bash`, `wget | sh`,
  `wget | bash`. A shell run on a substitution of the download
  (`sh -c "$(curl …)"`, `bash <(curl …)`) counts too. Like the other
  entries, it's a match on the command's text: it stops accidents, not a
  determined command.
- **The page** lists the commands Autopilot may never run, the defaults
  until the list is changed, with Add, Remove and Reset list as for the
  allowed commands; the defaults are the protocol's, which a test checks.
- **The fixed refusals.** A command naming the server's own folder
  (`~/.rovik`), which waits for the user in any other chat, is refused in
  Autopilot with why. Git writes on a server were refused already.
- **Plan mode's refusal** of a tool it doesn't offer says, in Autopilot,
  that Autopilot implements the plan next, not the user.
- **Conflicts with the project** stay the user's setting ("the chat
  resolves its conflicts" is one switch for the app): with it off, an
  isolated Autopilot chat's conflicts wait for its delivery, which says
  so.

### U02: approvals in Autopilot

- **Who answers.** In a run of a task, or a subagent of one, a rule that
  would ask the user asks the AI reviewer (`authorize`). Whatever would
  then fall back to the user is refused, with why, as the tool's result:
  the reviewer rejecting, being unsure, failing to answer, no reviewer
  model set, and the shapes no reviewer approves on its own (commands
  that push, delete, deploy, escalate or run inline scripts, edits to
  secret files). The reviewer's verdict shows on the call only where it
  gave one. A rejection doesn't tell the model to ask the user.
- **A resumed call** after a crash goes by its own rule, as a new one
  does: no approval when its kind needs none.
- **The checks don't wait for approval.** With command approval on, the
  gate's checks wait for the user's go (`verify-needs-approval`), and so
  do a parallel group's checks mid-run and a merged result's checks
  before landing. In Autopilot nobody gives it, so they run: they're the
  IDE's commands, set up by the user, not the model's (D7). This is
  V01's second half, taken here because without it a task with command
  approval on would wait for ever.
- **Tests** use a System One stand-in (`testing::system_one`) for an
  unsure typed reviewer: a text reviewer approves, rejects or fails.

### U03: nothing waits

- **The loop guard** in Autopilot: its first pause of a run goes on
  instead, the model told "You repeated yourself: … Change approach"
  (a harness note of kind `loop`). The guard keeps the patterns it warned
  about, so looping again pauses the run, and that pause ends it as in
  any chat (`paused-loop`, after the model's summary).
- **The driver ends the stage** of a run that ended waiting for a person:
  `paused-loop`, and `paused-checkpoint` (which Autopilot doesn't have
  since U01), are failures whose task delivers with why. Only a move and
  an upgrade of the server's Rovik leave a stage under way: they go on by
  themselves. A user who is there can still Continue such a run: it opens
  the stage again.
- **The approval deadline** is in the one wait (`wait_for_decision`): an
  approval in a task's run, or a subagent's of one, that nobody answers
  in 10 minutes is denied, the request withdrawn from the chat, with a
  notice and a log line; the call's result says nobody answered. Since
  U02 no approval in Autopilot reaches a person, so this is the net under
  it, and its test drives the wait directly. Questions get no deadline:
  `ask_user` is refused in Autopilot (A04).

### U04: the budget

- **What counts** (`agent_run/budget.rs`, `Spent`): the time a task's
  runs ran and the cost their providers reported. Each lead run of a task
  adds its replies' cost, its subagents' too, and its time since it
  started; each stage record keeps what its runs spent (`spent`), so the
  task's total is its records' and survives a restart. A run a crash cut
  loses its part; one a move or an upgrade paused keeps it. Time between
  runs (a closed laptop) doesn't count.
- **The cost** is what a provider says a request cost (OpenRouter does),
  in its currency. The app has no prices, so with a provider that reports
  none only the time limit is kept, which the page says; such replies are
  counted (`uncosted`). The headless runner's accounting of a run's log
  (`Totals`, `Price`) moved here too; the runner uses it from the core.
- **Where it's checked:** after each of the lead's replies (that reply's
  calls are answered as not run, and the run ends `budget`), and before
  each stage that starts work. The stages done inside a run (the
  reviews, the checks) record what that run did. `budget` fails its stage
  and the task delivers, saying why; `pending_reason` says "the task went
  over its budget" (S09 makes it a partial delivery). Raising the budget
  and pressing Continue opens the stage again.
- **The page:** "Budget per task: Stop a task after [hours] or once it
  cost [amount]", empty for no limit, showing the effort's defaults
  (Balanced: 2 hours and 5; Thorough: none) until set; a budget set stays
  whatever the effort, until it's set back to the effort's own.
- **What was implemented** is now whether an implementation's run
  started (the IDE's message for it is in the chat), not whether an
  implement record exists: a stage that never started leaves one too.

### V01: the checks run what the allowlist wouldn't

- **Done by U01 already**: a task's runs allow any command but the
  denylist's, and the gate checks its commands with the run's
  permissions, so the user's allowed commands don't skip a check in
  Autopilot. `verify-needs-approval` can't happen there since U02. V01
  adds the test: a Python project's detected `pytest`, not in the allowed
  commands, runs in Autopilot and is still skipped in an Agent chat.
- **The denylist still applies to checks**: a check it names is skipped,
  and the record says which entry ("`sudo` is a command it may never
  run"), not "not in your allowed commands".

### V02: nothing lands unchecked; the plan's validation as checks

- **What lands** (`landing.rs`, `pending_reason_in`): in a task's runs,
  `unverified` and `verify-unverified` don't land: "Not landed: no check
  ran on it, and Autopilot lands only what's checked." The chat's
  landing, the driver's delivery and the daemon's results all use it;
  outside Autopilot nothing changes. A chat that works in the project
  folder has nothing to hold back: its delivery fails and says "the
  changes are in the project as they are".
- **The daemon knows a task's run** from `RunEnded.autopilot`, a new
  field (additive: left out when false, read as false when missing), set
  from the run's marking before it's unregistered. The daemon's results
  hold such a result back with the reason above.
- **The plan's checks** (`plan::validation_commands`): with no check set
  up, a task's implementation is checked by the commands of the
  implemented plan's "## Validation": its code spans and code-block
  lines that start with a known build or test program (or a `./script`)
  and name a check (`test`, `lint`, `build`, `pytest`, `tsc`). Steps
  done by hand, files and functions aren't commands; ones that don't end
  (`dev`, `watch`, `serve`) and `cargo run`/`go run` are left out. Each
  gets a kind for the gate's order (`validation_kind`). The plan is the
  one in the implement request (`meta.implementsPlan`).
- **Filtered by the denylist** where every check is: the gate skips one
  the denylist names, saying which entry.
- **Recorded as such**: the verification record has `fromPlan`, and the
  record says "No checks are set up for this project: these are the
  commands the plan's Validation section names."

### V03: the environment's failures, and rounds bounded by progress

- **The classifier** (`command_output::environment_failure`): a failed
  check's output says its environment failed when a shell didn't find a
  program (exit code 127, "command not found", `sh: 1: X: not found`,
  cmd's "is not recognized"; a bare "X: not found" only with 127 or a
  shell's name first), make's "Error 127", Node's or TypeScript's "Cannot
  find module/package" of a package's name (not `./util`, not
  `node:fs`), Python's "No module named", or cargo's "could not find
  `Cargo.toml`". The record keeps the line that says so. A typo in an
  import of the project's own Python package reads as the environment
  too; it costs one uncounted round at most. Only Autopilot's checks are
  classified.
- **Prepare doesn't exist yet** (S03), so V03 builds the part of it it
  needs: `agent_run/prepare.rs` finds the installs the lockfiles at the
  root and one folder down imply (4.3), where what they install isn't
  there (`node_modules` or `.pnp.cjs`, `.venv`, a virtual environment,
  `target` for Cargo, `vendor` for Go), and runs them as heavy work with
  the checks' timeout, their output saved as `@checks/prepare-…`.
  `npm ci` runs with `--no-audit --no-fund`: the audit is a request to
  the registry nobody reads there. The stage before the first check, its
  stage record and its switch on the page stay S03's; the backend's
  `prepare` setting (on by default) already decides whether they run.
- **The round** that fails on the environment, the first in a run,
  isn't counted: its record says so (`counted: false`, `environment`:
  the line, the check, each install with its result), the installs run,
  and the checks run again at once on the same code, as round N again.
  With nothing to install, the agent hears what's missing and that the
  round isn't counted (in Autopilot it may run the install itself). A
  later one counts as any failure, and the agent hears it's the
  environment, the dependencies installed once already. "Once" is once a
  run: S03's record will make it once a task.
- **Rounds end on progress**: in Autopilot a run's checks give up after
  two rounds in a row without progress (`repair::judge`'s no-progress
  or a rolled-back regression), not after three; a round of progress
  starts the count again. A round that shows failures the last one hid
  (the type check passes and the tests run) is progress: what hid them
  was fixed. The budget ends them as it ends the run (U04). For a task
  with no budget, `AUTOPILOT_MAX_ROUNDS` (10) is a last stop, for rounds
  that keep changing what fails. The agent's report and the record say
  "attempt N", with no count (`byProgress`); a record that gave up so
  says the last two made no progress (`noProgress`).

### V04: a syntax check after every write

- **For every chat** (D20): the agent's `write` (a whole file, or a part
  with `old_text`) checks the file as written with
  `outline::syntax_errors` and appends what doesn't parse to its result:
  "The file doesn't parse now (a syntax error):", each error as
  `line:col: message`, at most ten, then "Fix it before you go on."
  (`agent_run/tools.rs`, `syntax_note`).
- **Only what the write broke**: a file that didn't parse before the
  write, or wasn't text, says nothing. The grammars misread some valid
  code, and a file broken already says nothing new about this write. A
  new file is checked. A language with no grammar, or a file over the
  outline's limits, is written as before.
- **The result's first line** keeps the line counts, which
  `result_meta` and the frontend's `toolResultMeta` now read from the
  first line, not the last.

### V05: a rollback of only what broke

- **For every chat** (D20). A fix round that breaks what passed before
  puts back only the files behind what it broke, when every failure it
  brought in names one the round changed (`causes::files_behind`): an
  error's own file, or for a failing test the changed files whose name
  it carries (`posts` in `posts > lists`, three letters or more). The
  rest of the fix stays (`rolledBack.kept`).
- **Most regressions don't name such a file**: an error in a file the
  round wrote is a slip (P3.2), not a regression, so a regression's
  error is in a file the round didn't touch; and a test's name often
  carries no file's. Those go back whole, as before: the existing suites
  pass unchanged.
- **The rest is checked at once**: what stays is new code, so the checks
  run again on it before the agent hears anything ("repaired"). Their
  result is the round's: it counts once (the same round number, and in
  Autopilot one round toward "without progress"). Still broken, the
  round goes back whole to its checkpoint, kept for that, and the agent
  gets the round's failures as before, its next attempt still its own.
  On the last round there's no round to check what stays: everything
  goes back.

### V06: a suite too slow for its timeout

- **In Autopilot**, a test check that times out is recorded `slow`, not
  failed ("timed out after 10 min; the tests of the changed files ran
  instead"), and the tests of the changed files run in its place, as one
  more check of the round: the test files among them, and those their
  stack's conventions put beside the others (`project::test_paths_for`),
  run by `project::test_command_for`. Its result is the round's: passing,
  the round passes. Outside Autopilot a timeout stays a failure.
- **No scoped command**, no rerun: a change with no test files beside
  it, a stack `test_command_for` can't run a part of, or one where it's
  the whole suite again keeps the timeout as a failure, as before.
- **Not compared**: the slow check keeps the `timed out` key, so its
  round's tests aren't compared with another round's (repair.rs
  `blind`), and the scoped run's test count isn't kept, so "fewer tests"
  (P3.7) doesn't flag it. The agent's report lists it under "Too slow".

### V07: a check's whole output for its failure keys

- **What's kept**: a command's output keeps its first 16 KB and last
  48 KB a stream to show (`command_tool.rs`). A check of an Autopilot
  task keeps, besides, up to 8 MB a stream (its start, then its last
  1 MB past that) in `CommandOutput::whole`, never sent to the frontend,
  and the gate's `failure_keys` read that: the errors in the middle of a
  long run count when rounds are compared. Outside Autopilot it isn't
  kept (D20): the keys come from what's shown, as before.
- **Unchanged**: what the record and the agent see, and the saved
  `@checks/` file, which is still the output as shown.

### V08: Go's test count

- **For every chat** (D20): `command_output::tests_ran` counts the tests
  `go test -v` ran, its `--- PASS:` and `--- FAIL:` lines, subtests
  included and skipped ones not, so the "fewer tests" flag (P3.7) works
  for Go. Without `-v`, go prints only the failures, which aren't a
  count: the output says none then (no `=== RUN` line), as before.

### V09: stacks one folder down

- **In Autopilot** (D20: outside it, detection is as before, on the
  Settings page and in an Agent chat), a run's checks are the project's
  and those of the stacks one folder down
  (`project_profile::detect_below`): each folder that isn't hidden,
  installed, built or vendored, nor one of the root's own workspace
  members (their checks are the root's workspace-wide ones), detected as
  a project is. Each check runs in its folder (`cd api && go test ./...`,
  `project::in_folder`), every folder's every round, and the prompt names
  each stack with its folder ("Go (api/)"). The checks the user saved for
  the project stand instead.
- **One folder's type check** that fails skips that folder's tests only
  (`project::folder_of`): another stack's errors aren't theirs.
- **Not yet per folder**: V06's scoped rerun finds the test runner in the
  project's own `package.json`, so a package one folder down keeps a
  timeout as a failure.

### S01: criteria

- **What the planner is told** (`plan::plan_mode_instructions`, in
  Autopilot): after the title and summary, "## Criteria": numbered
  statements of what done means that a test or a reading of the result
  can check, together the whole request and nothing beyond it; then
  **Out of scope** and **Assumptions**, either of which may say "None.".
  Assumptions now go there: A04's "under ## Decisions" (the plan stage's
  lines and `ask_user`'s refusal) says **Assumptions** in its place;
  "## Decisions" stays for design decisions.
- **The parser** (`plan::criteria`) is strict: only numbered lines are
  criteria (a line under one continues it), the parts are found by their
  names (`**Out of scope**`, `### Assumptions`, `Assumptions: …`), a
  part may list bullets or say "None", and code blocks are skipped. The
  section is the first `## ` heading with "criteria" in it, so
  "## Acceptance criteria" is one.
- **The check** (`plan::plan_problems_for`): in Autopilot `submit_plan`
  also needs the section, a numbered criterion and both parts, each
  missing one named in the result. The implementing run's check of the
  plan file at its start (B07b) doesn't add it.
- **The plan review** (`plan_review::brief`) in Autopilot adds "## The
  criteria" after the plan: criteria nothing could check, a part of the
  request none covers, one beyond the request, and an assumption the
  request or the code contradicts are findings, about "Plan".

### S02: the plan review's cap

- **Three reviews** a task's plan run (`AUTOPILOT_PLAN_REVIEW_ROUNDS`), in
  Balanced and, until T07 brings "two rounds without progress", in
  Thorough; elsewhere two, as before. The holds say so ("review 2 of at
  most 3", and before the last: "a high finding it still finds ends the
  task").
- **Blocked**: the last review with a high finding open holds the plan
  for good (`review.blocked`): `submit_plan` answers that the plan won't
  be saved and lists the findings, and a later submit gets "the plan is
  blocked". The plan stage is done ("Written; its review held it back"),
  the plan review fails with `plan-blocked` ("Blocked after 3 reviews: 1
  high finding open"), and the delivery says "Nothing to deliver". With
  only medium findings open at the last review, the plan is saved with
  them under "## Open review findings", as before.
- **The report** that lists the findings is S09's; until then the review
  record (the stage's `ref`) holds them.

### S03: Prepare

- **A stage the IDE runs**: Prepare (and S04's Tests) is a run of the
  chat the driver starts with no message (`StartKind::Retry`) whose
  drive makes no model request (`Entry::Stage`, `autopilot::is_harness_stage`).
  So Stop, the budget, the run's registration and its log line are a
  run's, as for the model's stages. The run ends its stage record
  itself, never lands (`try_land`), and `RunEnded.stage` (additive in
  the protocol: older ones read as none) names the stage, so the daemon
  makes no result for it: a task's results stay the plan's and the
  implementation's.
- **The record**: an empty assistant row with `meta.prepare` (running,
  passed, failed or stopped, and each install with how it went), shown
  as the installs of a check round are (`PrepareRecord.tsx`); the stage
  record's `ref` is it. Done: "Installed: `npm ci --no-audit --no-fund`",
  or "1 of 2 installs failed (`uv sync` in api): the checks will say
  what's missing". Skipped: "Nothing to install: the dependencies are
  there", "No project folder", "Off in the settings". Stopped by the
  user: failed, `stopped`, and the delivery comes next.
- **The saved output**: `@checks/prepare-1.log` (`prepare-api-2.log` one
  folder down). The stage's run starts the chat's saved output afresh
  (P4.7), and the task's later runs keep `prepare-…` when they clean
  theirs: the record links it and the implement run may read it. V03's
  installs after an environment failure save `prepare-again-…`, leaving
  the stage's as they were.
- **The rerun** D12 calls for is V03's environment round, still once a
  run: the implement run is the one run of a task that checks (a
  resumed one may install once more). After Prepare, what it finds
  missing is what Prepare couldn't install, or what the change added.
- **The setting** "Prepare dependencies before the first check", on by
  default (`AutopilotConfig.prepare`): off, no stage and no install
  after an environment failure.

### S04: tests first

- **The stage** is a run the IDE does itself, as Prepare is: the test
  writer runs as an IDE-started subagent (`run_ide_subagent_with`, with
  E02's limits: test files only, the plan's files hidden, light
  commands), from the request, the task's plan (its plan run's, not the
  chat's latest) and the plan's criteria, listed in its brief ("# Write
  the tests before the change", "## The criteria": a test for each one a
  test can check). Its record row is `meta.tests` (`TestsRecord.tsx`):
  the files, its report's parts, its chat on demand.
- **The red check before the change**: E04 runs the new tests on the
  project as it was in a scratch copy, which needs an isolated chat.
  Before the change nothing is changed yet, so they run where they are,
  in any chat (one file at a time when the stack can, else the project's
  test check once). One that passes is flagged in the record, the
  stage's line and the request.
- **The implementation** is told: its request ends with "Tests that must
  pass: `a`, `b`; they fail now on purpose." (one that passes already is
  named apart: "check that it tests what the change does"). The stage
  record's `files` (additive) holds them, and the implement run takes
  them as the test writer's (`test_writer_files`): E03's line lists a
  change to one in its final message, and the repair's test rule applies.
- **A writer that's blocked**, fails, writes nothing or is stopped alone
  skips the stage, with why, and the task goes on; tests it wrote before
  it was blocked stay the task's (the stage is done, and says why). Stop
  on the chat stops the task, as for any stage.
- **The suites' scripted model** answers the stage's writer apart
  (blocked) unless a test gives it replies, so a task's other runs keep
  the replies written for them; the daemon's tests route it too.
- **Stage lines** show their `code` spans as code (S03's commands, S04's
  files).

### S05: a light check after each step

- **When**: in Autopilot, as a step worker that ran alone ends
  (`after_worker`, on the lead's side), on the files it changed, done or
  not. A group's workers run their own light checks (F05b) and aren't
  checked again.
- **What**: each file's syntax (`outline::syntax_errors`) where it parsed
  before the step or is new (V04's rule), then the commands
  `project::light_check_for` gives (a linter on a file, `go vet` on a
  package) as check work, two minutes at most each; the denylist holds.
  A stack with no such command is checked for its syntax only: the
  editor's language server isn't the harness's to run.
- **Where it goes**: a note with the worker's result, for the lead ("The
  light check after step 1 found problems in its files: …", five syntax
  errors a file at most, a command's output cut to 2,000 characters), and
  under that step in the later steps' briefs ("## Steps done so far").
  The step stays done: what to do about it is the lead's call.

### S06: the change review blocks

- **The answers**: Autopilot's implement run has `answer_review`
  (`{ "responses": [ { "finding": 1, "status": "fixed" | "not-a-problem", "note": "…" } ] }`,
  read as tolerantly as `submit_plan`'s `responses`), the only way to
  answer the findings sent: its replies aren't read. Finishing with one
  unanswered is asked about, twice at most (`MAX_ANSWER_NUDGES`); then the
  follow-up checks what was answered.
- **The follow-up**: the next review follows the findings up whatever
  changed (no write needed, no minimum size, and "two reviews in a row"
  doesn't hold it back): it gets each finding with its answer and gives a
  verdict on each, by B13's rules and repair turns
  (`plan_review::follow_up`). A finding stays open until a follow-up finds
  it resolved; a review that couldn't report (failed, stopped) leaves
  them as they were.
- **`review-open`**: a high finding still open after the run's last
  review (`MAX_CHANGE_REVIEWS_PER_RUN`, in both efforts until T07) ends
  the run `review-open`, before the checks or, after a repair, in place
  of R08's green. The implement stage is done ("Finished; its review held
  it back"), the change review fails ("Blocked after 3 reviews: 1 high
  finding open"), and the delivery doesn't land it ("the change review
  still finds a high problem in it"). Medium findings at the last review
  go to the agent with no review to follow: the checks decide.
- Agent mode's review stays advice, as it was (D20).

### S07: the test-exists rule

- **When**: in Autopilot, as a round of the implement run's checks
  passes, on a change with a code file (one `project::test_paths_for`
  has a test place for: a README isn't code) and no test file
  (`project::is_test_path`), when the Tests stage wrote none either
  (S04). Once a run, which is once a task: the implement run is the
  task's run that checks. It comes after R08's review and before the run
  ends, so before acceptance (S08).
- **What**: the test writer with E02's brief and limits, told the change
  is made and came with no tests, its files hidden; E04's red check on
  what it wrote (in an isolated chat); its record is `meta.tests` with
  `after`. What it wrote is the test writer's to the run (E03), and the
  checks run again at once ("repaired"), a round like any other: one that
  fails goes to the agent, with the test rule for the writer's tests.
- **Nothing written** (blocked, failed, no file): recorded, and the
  green round stands.

### S08: acceptance

- **The checker** (`Kind::Acceptance`, `acceptance.rs`) is read-only,
  with the reviewer's tools. Its report has "Verdicts" (required),
  "Evidence" and "Unmet", in a format of its own (`Format::Verdicts`):
  one line per criterion, numbered in order, "satisfied" or
  "unsatisfied" and what shows it, parsed strictly (`parse_verdicts`). A
  reply that doesn't read as verdicts gets the repair turn and is then
  recorded skipped, as is one with a verdict count that isn't the
  criteria's, or a checker that says it's blocked: never satisfied.
- **Its brief**: the request, the task's plan's criteria, the run's diff,
  each check and how it ended, the test writer's report (S04's or
  S07's), and on the second check what the first found unsatisfied.
- **The hook** (`accept_after_green`): as the implement run's checks pass
  in Autopilot, after S06's and S07's. Unsatisfied criteria go to the
  agent in a note (`harness: acceptance`, with the checker's advice) for
  one more round, which ends with the checks and a second check whatever
  it changed. Still unsatisfied then (`MAX_ACCEPTANCE_CHECKS`, 2), the
  run ends `unmet`. Thorough's "until two rounds without progress" is
  T's.
- **The stages**: met, the acceptance is done ("The criterion is met",
  "All 3 criteria met"); `unmet`, the implement and checks stages are
  done and the acceptance fails ("1 of 1 criteria unmet after 2
  checks"), and the delivery doesn't land it ("the acceptance check finds
  a criterion of the plan unmet"): S09's report delivers it as partial.
  A plan without criteria, or a checker that couldn't judge: skipped,
  and the checks decide.
- **The settings**: "Acceptance checker" joins the kinds' rows, used by
  Autopilot (`needed_kinds`); ordinary chats never run it. The subagents
  doc's report table has its row.

### S09: the report, and delivery per outcome

- **The delivery is a run** of the IDE's (`is_harness_stage`): the driver
  records it running and starts it; the run (`deliver_stage`) records
  what became of the result and has the model write the report in one
  turn it can't call tools in (`tool_choice: "none"`: the history keeps
  its tools, for the provider's cache), in the implementer's context, the
  chat. A task the user stopped gets no report (they're there), nor does
  one resumed after a crash at its delivery: the driver records the
  delivery as before. The daemon makes no result for the run.
- **The facts** (`autopilot::report_facts`, kept as `meta.report`): how
  the task ended and what became of it, the plan's assumptions and
  criteria, the acceptance's verdicts, the tests written for the task
  (S04, S07), the last checks, the change review's line, what it spent.
  The brief lists them and asks for "## Assumptions", "## What changed",
  "## How it was verified" and "## Open", in that order, "Open" starting
  with why the result wasn't delivered whole when it wasn't. A failed or
  empty turn: the IDE writes the report from the facts (`byIde`).
- **The record**: the delivery's record points at the report; the chat
  shows it as the final message, its facts under it (`ReportFacts.tsx`).
- **Per outcome (D7)**: `done` and `verified` land; `unmet`, `budget` and
  `verify-flagged` are delivered as partial: not landed (an isolated
  chat's changes wait in it, with why; a chat working in the project
  left them there), the delivery done and saying so ("Delivered as
  partial, not landed: …"); the other outcomes deliver nothing, and the
  delivery fails, as before.
- **The suites' scripted model**: a route marker that starts with
  `last:` is looked for in a request's last message
  (`testing::route_of`): the report's brief ends the chat, whose first
  message is the task's.

### S10: the headless runner

- **The flags**: `--mode agent|plan|autopilot`, over the settings file's
  `mode`, and `--effort balanced|thorough`, for Autopilot only (Balanced
  unless said).
- **It waits for the task**, not its first run: the task is over when
  its stages are and nothing of the chat runs (`autopilot::unfinished`,
  `is_running`), looked at every 250 ms, since the driver records some
  stages without a run.
- **The report**: `outcome` is the task's; it adds `effort`, `stages`
  (each with its status, round, outcome, line, files and spend) and
  `report` (the task report's facts); `finalMessage` is the task's
  report. A timeout or the token cap stops the run going on, as before:
  the stages after it may still be being recorded as the report is
  written.

## H: the host works on its own

### H01: the third choice and its block

- **Where the block lives**: on each host (`Host.autonomy` in
  `cloud-hosts.json`), not on the file's `HostSettings`: each server has
  its own key, remote and budget. A host without the block (every host
  saved before H01) loads with the defaults; a host with the defaults
  saves without it. The block stays when another choice is picked, and
  the host's form doesn't send it: `hosts_page.rs` keeps it on a save.
- **`Delivery::PushBranch.deploy_key` is a `String`**, not a `PathBuf`:
  it's a path on the server, which a Windows computer would read as one
  of its own.
- **Checked where it's kept** (`AutonomySettings::checked`, from
  `hosts_set_autonomy`): pushing needs a key's path (one line), the
  notify URL is an `http(s)://` address with no spaces, a budget's limit
  of 0 or less is no limit of its own; trimmed. The page saves each
  change as it's made and shows the refusal when there is one.
- **The budget's empty fields** are as in Settings → Subagents, each on
  its own: a host that sets only hours keeps the computer's cost limit
  (H02 merges them). A budget with no limit left isn't kept.
- **"Push to a branch"** saves nothing until the key's path is in: until
  then, results stay on the server, and the pull request box is off.
- `ApprovalPolicy::Autonomous` applies as "Ask the AI reviewer first,
  then me" until H02.

### H02: what a host that works on its own runs with

- **`ApprovalPolicy::apply(autonomy, config)`** takes the host's block:
  under `Autonomous`, its effort, and each of its budget's limits that it
  sets, are over the computer's (`config.autopilot`); what would wait for
  the user goes to the AI reviewer first, as under "Ask the AI reviewer
  first, then me", and in Autopilot what it doesn't approve is refused
  (U02). U01's rules and A07's kinds aren't put on the configuration: the
  core applies them to every Autopilot run wherever it runs
  (`AgentState::config_for` → `run_config`), the daemon's included, and
  a message the user sends there in Agent or Plan mode keeps the
  computer's rules.
- **The budget's defaults**: the computer sends its budget resolved (the
  effort's default when the user set none), so a host on Thorough under a
  computer on Balanced keeps the computer's limits unless it sets its
  own: limits are the safer side to err on.
- **Autopilot unless the message says otherwise**: the mode picker keeps
  a mode of its own for each chat on such a host while it's open,
  Autopilot until the user picks another for it (`setOwnModeChat`,
  `HostSummary.autonomous`); the selector's mode for the other chats, and
  the one saved, stay as they were. A start that names no mode runs in
  Autopilot there (`Remote::start`), for a client that sends none.
- **The frame is a request**, `AutonomyConfigure(AutonomyConfig)` (the
  block's delivery and notify URL), served with the `autonomy` feature,
  kept per computer in state.json (`State.autonomy`, 0600) and read back
  at start (`Store::autonomy_of`, for H04 and H06). The client sends it
  on connecting, after the configuration, whether or not the frontend
  applied its settings yet (it's the host's own settings), and at each
  change of the host's policy or block (`Remote::reconfigure`, with the
  configuration); under another policy it sends the default, which the
  daemon forgets. A daemon without the feature isn't sent it.
- **The notify URL is never logged whole**: `AutonomySettings` and
  `AutonomyConfig` show it in `Debug` as `url_for_logs` does (scheme and
  host, `/…` when there's more), so a request logged with `{:?}` doesn't
  carry its token.
- **`StartRequest.deliver_to`** comes with the delivery (H04), which is
  the first to read it.

### H03: the pipeline on the server

- **Already there since A03**: the core drives the task wherever it
  runs. `autopilot::after_run` is called where a run's end is told
  (`agent_start`), on the daemon as on the desktop, and starts the next
  stage's run itself; `restart.rs` restarts a cut run, then resumes
  every chat whose task is unfinished with no run to restart
  (`resume_task`: a Resume start, which `autopilot::resume` takes).
  Nothing waits for a client: H03 adds the daemon tests that show it.
- **The tests** (rovik-agentd/tests/autopilot.rs): a task started over a
  WebSocket whose client then goes runs to its delivery, and the client
  back pulls its stage records (`ChatChangesSince`); `kill -9` while the
  plan's review is written (inside the plan's run, S02) goes on, the
  review asked again, the plan not made again; `kill -9` between two
  stages goes on through `restart_interrupted`'s second pass.
- **Between two stages, made**: the driver writes the stages done inside
  a run, then starts the next run, all in milliseconds, so a kill lands
  there only by chance. The test kills the daemon as the Tests stage's
  writer is asked and sets state.json to say no run of the chat went, as
  it said a moment before that run was told.
- **A kill in the instant after a run's end is kept and before its stage
  ends** (the record still running, no run going) retries that stage's
  run when the daemon starts again: `recovery::after_crash` can't tell a
  run that ended from one cut between steps, so the stage is done again.
  Work is redone, nothing is lost.

### H04: delivery by a push

- **When**: at the task's delivery, not after each implementation run: a
  task may implement several rounds (acceptance, S08), each committed as
  the chat's result, and only the last is the task's. The core's
  Deliver stage asks the server's `Deliverer` (set by the daemon, like
  its config resolver) before the report is written, for a landable
  result only (`done`, `verified`; not a partial one); the records-only
  delivery (a resume after a crash at it) asks too. The server waits for
  the last run's commit, commits the worktree again (the same result,
  numbered as it was, when nothing changed), pushes it, opens the pull
  request, and records the branch and the PR on the chat's result,
  which goes out again in a `ResultReady` (same number: a client passes
  over it, and lands nothing twice). The report's facts (`meta.report`)
  have the branch and the PR, the delivery's line says where it went,
  and the report's brief has that line; the PR's description is the
  IDE's facts until the report is written, then the report
  (`gh pr edit`).
- **The remote comes with `ProjectEnsure`**, not `StartRequest`
  (`deliver_to`): it's the project's, the daemon keeps it with the
  project (`ProjectState.remote`), and the start request has 31 struct
  literals a field would touch. The client sends it only to a host that
  pushes, and the next sync after a change of the host's policy or block
  ensures the project again. It's the `origin` of the project's
  repository as an SSH address (`push_url`): `https://` becomes
  `git@host:path`, credentials and port left behind; a `file://` URL or
  a path stays (a remote on the server itself).
- **The system `git` pushes**, never forced, nothing prompting
  (`GIT_TERMINAL_PROMPT=0`); for an SSH remote, `GIT_SSH_COMMAND` runs
  `ssh -i <key>` with `IdentitiesOnly`, `BatchMode` and
  `StrictHostKeyChecking=accept-new`. git2 over SSH would need libssh2
  and OpenSSL in the daemon's static musl build. The host's key is
  trusted on first use (`accept-new`), in the daemon user's
  `known_hosts`. The key's path is kept, never its contents; `~/` is the
  daemon's home.
- **The branch** is `rovik/` + the task's title (its plan's, else its
  message's first line) in lowercase words joined by dashes, at most 40
  characters, + the first six characters of the task's id: two tasks of
  the same name don't meet.
- **The pull request**: `gh pr create --repo <repo> --head <branch>`,
  into the repository's default branch, `<repo>` being the remote's
  host and the last two folders of its path. `gh` missing, not logged
  in, or failing: the branch is still delivered, and the delivery's line
  adds why there's no pull request.
- **A push that fails** is a failed delivery: its record says "Not
  delivered: the push to … failed: …; the result waits on the server for
  its computer", with the remote and git's own words, the task's outcome
  stays, and the result reaches the computer as before.
- **The branch goes on from the project's history** when the snapshot
  did (the shadow's parent is the project's `HEAD`, 6.4); a project sent
  without its history gives a branch that shares none with the remote,
  and a pull request the forge can't merge.
- **Tests**: the daemon pushes to a local bare remote over a file URL,
  and over SSH to the tests' russh server with the deploy key, a `gh`
  that keeps what it's asked standing in for the real one; a push to a
  remote that isn't there fails the delivery.

### H05: Test delivery

- **`DeliveryTest { remote }`** (served with `autonomy`): the daemon
  makes a commit of the empty tree in a scratch repository, pushes it to
  `rovik/test-<yyyymmdd-hhmmss>` on `remote` as a task's result would go
  (the same `git push`, with this computer's deploy key), deletes the
  branch, and reports (`DeliveryTested`): the branch, and why the push or
  the delete didn't go, if they didn't. With pull requests asked for, it
  also runs `gh auth status --hostname <host>` and says whether they'd
  open. A computer that has the server keep its results is refused, and
  told to choose the push first.
- **The remote** is the open project's `origin` (`push_url`, no
  credentials), from the computer: the hosts page names no project, and
  the test is for where the next task's result would go. No project open
  with an `origin`, or a daemon without the feature: the page says so.
- **The button** shows under the push's settings once they're saved, and
  the line under it what the test found.

### H06: the notice

- **From the events** (`Tap::agent` → `notify::Notifier`): a stage
  record comes by in `MessagesAdded` or `MessagePatched`, as every client
  gets it. A record whose status is `failed`, and the delivery's once
  it's no longer running, are told, each once by message and status (a
  record patched again, its spend updated, tells nobody twice; the set
  is the process's, so a restart may tell a record that ends again). A
  failed stage that ends its task gives two notices: the stage's, then
  the end's.
- **The body**: `{ chat, task, stage, outcome, summary, branch, pr }`,
  `outcome` being the record's (the task's outcome) or its status when
  it has none, `summary` its line (for the end, the delivery's: where
  the result went). The branch and the pull request are the task's
  report's (`meta.report`, H04), seen go by before its delivery's end; a
  delivery recorded without a report (a resume after a crash at it)
  names none.
- **The call**: `POST` with a 10-second timeout, tried again three
  times, a second, two, then four after the one before; a 2xx is done.
  One that never goes through is logged with `url_for_logs` (the URL's
  scheme and host), and the error without its URL
  (`reqwest::Error::without_url`): the URL may carry a token. It never
  waits in the task's way: each notice is a task of its own.

### H07: rate limits on the server

- **In the driver, not the daemon**: the stage's end is the core's
  (`after_run`), so a run that ended `error` with its last failure
  `rate_limited` (`meta.failure.kind`, after the request's own four
  attempts) doesn't end its stage on a server (`on_server`): the stage
  stays running, its line says when it goes on ("Rate limited by the
  provider: again in …"), and its run starts again as a Retry after the
  wait, when the stage is still the task's latest and running and no run
  of the chat goes. On a computer, the user is there: the stage ends as
  before.
- **The wait**: until the reset the failure named (`resetAt`, from the
  provider's `Retry-After`), at least a second; without one, a minute,
  then two, four…. The stage's waits add up to an hour at most, and none
  starts past the task's budget (U04: what it spent, the run's with it);
  past either, the stage ends as before. The waits are counted in memory
  by stage record: a daemon that restarts meanwhile resumes the task at
  once (A03), and counts again.

### H08: what's going on there

- **`rovik-agentd status [--data DIR] [--json]`** reads the daemon's
  folder, not its socket: state.json's chats (computer, the run going,
  the last run's outcome) and, from the chat database opened to read
  only (safe while the daemon writes it), each chat's title and its
  latest task (`autopilot::task_state`: the task's latest stage record,
  its status, outcome and line, and what its runs spent). `--json`
  prints `{ "chats": [ChatTask…] }`; without it, a line a chat.
- **The cost is the records'**, not the agent log's: each stage record
  keeps what its runs spent (U04), from the same replies' costs the log
  has, and a task's records are its own, where the log would be read
  whole and sorted by chat.
- **The hosts page asks the daemon** (`TaskStatus`, served with
  `autonomy`): the same list, of the asking computer's chats only (D16),
  under "Tasks on this server" on a connected host's page, with a
  Refresh.
