# Subagents: development plan

This plan builds the six subagent kinds proposed in
[`subagents.md`](subagents.md):

- **A.** Scout
- **B.** Change reviewer, and a plan reviewer for Plan mode
- **C.** Failure triage
- **D.** Sequential step worker
- **E.** Test writer
- **F.** Parallel implementers

Read `subagents.md` first for the reasons behind each kind and the rules
they follow. This file says what to build, in which files, in which order,
and how each step is tested.

Contents:

1. [Decisions](#1-decisions)
2. [How a subagent fits the existing run](#2-how-a-subagent-fits-the-existing-run)
3. [Parallel implementers: integrate first, then build and test once](#3-parallel-implementers-integrate-first-then-build-and-test-once)
4. [Milestones](#4-milestones)
5. [Implementation order for an AI agent](#5-implementation-order-for-an-ai-agent)
6. [Open questions](#6-open-questions)

Section 3 also describes the repair loop: what happens when the build or
tests fail after the workers' changes are merged.

---

## 1. Decisions

| #   | Decision                                                                                                                                                                                                                                               | Why                                                                                                                                                                                   |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | A subagent is a **child chat**: a row in `dialogs` with `parent_id`, `parent_call_id` and `kind`. The chat list hides it, and it runs through the same `Run` as any chat.                                                                              | It reuses everything a run already does: storage, streaming, failures, repetition and loop guards, approvals, Stop. Its history can be opened from the parent and survives a restart. |
| D2  | The lead has one new tool, `task`. Its `kind` argument picks the subagent kind, and the **harness**, not the model, fixes each kind's tools, budget and prompt.                                                                                        | The model can't give a scout `write`, or give itself an unlimited budget.                                                                                                             |
| D3  | **Depth 1.** A subagent is never offered `task`.                                                                                                                                                                                                       | Nesting is a failure mode the evidence warns about. It also keeps costs bounded.                                                                                                      |
| D4  | Subagents use a **"Subagent model" setting with fallbacks**, like the AI reviewer. When it isn't set, they use the chat's model. The change reviewer can have a model of its own.                                                                      | Cheap work goes to a cheap model, while the user keeps control.                                                                                                                       |
| D5  | Read-only kinds (A, B, C) use the **parent's root**: its overlay view, or the project.                                                                                                                                                                 | They write nothing, so sharing the view is safe and costs nothing.                                                                                                                    |
| D6  | Writing kinds: a sequential worker (D) writes into the **parent's overlay**. A parallel worker (F) and the test writer (E), when it runs alongside workers, each get a **child overlay stacked on the parent's view**, landed back into it.            | Only one writer at a time touches any overlay, so there are no lost updates.                                                                                                          |
| D7  | Writing kinds get a **write scope** (path globs). A write outside it is refused with a clear message.                                                                                                                                                  | This makes "disjoint files" a guarantee rather than a hope. Test writers stay on test files.                                                                                          |
| D8  | Permissions and approvals come **from the parent**. Approval requests appear in the parent chat, labelled with the subagent.                                                                                                                           | The user has one place to look, and a subagent can't do more than its parent could.                                                                                                   |
| D9  | Subagents never get `ask_user`, `submit_plan` or `todo`, and never use CLM context mode (they use trimming). When something is unclear, they report it as `blocked`.                                                                                   | Only the lead talks to the user and owns the plan and checklist.                                                                                                                      |
| D10 | Budgets for each kind: tool calls, wall time, and result length. Past the budget, the subagent gets one turn without tools to write its result, and its status is `partial`.                                                                           | A subagent that runs out of budget still returns what it found, never nothing.                                                                                                        |
| D11 | At most **3 subagents run at once per chat**, and at most 12 per run (settings). Heavy commands from all of them go through the `workload` queue, as today.                                                                                            | Limits cost, and keeps the machine usable.                                                                                                                                            |
| D12 | **Parallel workers don't build or run tests.** After the workers' changes are merged, the lead builds once and runs all checks, including the test writer's tests. See [section 3](#3-parallel-implementers-integrate-first-then-build-and-test-once). | With one CPU slot, builds run one at a time anyway. One integrated build finds the mismatches between workers that separate builds can't.                                             |
| D13 | The **verification gate stays the final judge**. Subagent reports inform the lead; they never mark a change as working.                                                                                                                                | The environment checks the result, not an agent's opinion.                                                                                                                            |
| D14 | Everything is behind a **Subagents** settings page, with a switch for each kind and **all kinds off** until milestone G turns them on.                                                                                                                 | Nothing changes for existing chats until the measurements are in.                                                                                                                     |

---

## 2. How a subagent fits the existing run

```
Lead Run (dialog P)                                 Child Run (dialog P~s1, kind=scout)
───────────────────                                 ──────────────────────────────────
model_turn → tool_calls: task{kind:"scout",…}
execute_tool_calls
  └─ execute_one("task")
       └─ subagent::spawn(brief) ─────────────────► Run::prepare_child(parent, spec)
                                                      root = parent's view (D5)
                                                      tools = kind's set (D2)
                                                      system prompt = kind's prompt + brief
                                                    drive(): model turns + tools
                                                      (no checklist, no verify gate,
                                                       no CLM, no plan mode)
                                                    ends: done | budget | stop | error
       ◄──────────────── SubResult (≤ cap) ───────── final message parsed into SubResult
  record(result) as the tool result
model_turn (sees only the result)
```

What is new, by file:

| File                                        | Change                                                                                                                                                                                                                                                                  |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `chat_db/schema.rs`                         | `SCHEMA_V8`: `dialogs.parent_id` (references `dialogs (id)` with `ON DELETE CASCADE`), `parent_call_id`, `kind`. `list_dialogs` and search leave out child chats.                                                                                                       |
| `agent_run/subagent.rs` (new)               | `Kind`; `KindSpec` (tools, default budget, prompt, read-only or not); `Brief`; `SubResult`; `spawn()`; parsing and capping the result; budget enforcement.                                                                                                              |
| `agent_run/run.rs`                          | `Run.role: Role { Lead, Sub(SubSpec) }`. `Run::prepare_child()` builds a child run from the parent's root, project context, repo map and model. `drive()` skips the checklist, the verify gate and CLM for `Sub`, and ends with a result turn when the budget runs out. |
| `agent_run/mod.rs`                          | `AgentState` registers child runs under their own dialog id, linked to the parent: Stop on the parent stops its children, and `is_running(parent)` counts them. `AgentConfig` gets `subagent_profiles`, `review_profiles` and `subagents: SubagentSettings`.            |
| `agent_run/tools.rs`                        | `task_schema()`, offered to the lead only (`tools_for` gains a `role` parameter). Each child kind gets its own filtered tool list.                                                                                                                                      |
| `agent_run/execute.rs`, `parallel.rs`       | `task` is dispatched to `subagent::spawn`. Read-only `task` calls can run in parallel batches, with their own limit (D11). Write-scope checks happen in `authorize()`.                                                                                                  |
| `agent_run/events.rs`                       | `SubagentStarted`, `SubagentProgress` (tool count, current tool) and `SubagentEnded`, keyed by parent dialog and call id. Approvals carry `parent_dialog_id`.                                                                                                           |
| `src/lib/agentRuns.ts`, `chatStore.ts`      | The new events, routed to the parent chat. Child messages are loaded when the block is opened.                                                                                                                                                                          |
| `src/components/ToolCallBlock.tsx`          | A `task` block: kind badge, brief, live progress, result, and "Open transcript" (`MessageList` for the child dialog).                                                                                                                                                   |
| `src/components/SubagentSettings.tsx` (new) | The settings page: kinds on or off, the subagent model and fallbacks, the reviewer model, and limits.                                                                                                                                                                   |

---

## 3. Parallel implementers: integrate first, then build and test once

**The question:** what if the lead takes all the code from the
implementer subagents, then builds (if the project needs building) and
runs the test writer's tests?

**Short answer:** yes, and this should be the design for kind F. The lead
waits for every worker in a group, merges their changes, builds once and
runs all checks, including the test writer's tests. The gain is real, and
the risks can be handled with five safeguards.

### What gets better

1. **No wasted builds.** Rovik runs heavy commands one at a time by
   default (`workload/`, one CPU slot). If three workers each built, the
   builds would queue one behind another anyway, and each would build a
   tree that is missing the other workers' changes. One build of the
   merged result costs about the same as one of theirs, and it's the build
   that counts.
2. **Integration mistakes show up.** Separate builds can all pass while
   the combination fails. For example, worker 1 renames a field that
   worker 2 still uses, or two workers both register the same command.
   This is the Flappy Bird problem from the research, applied to code, and
   only a build of the merged result finds it.
3. **The test writer stays independent.** Its tests come from the request
   and the plan, not from the code. Running them only on the merged result
   is the earliest point at which they're meaningful.
4. **The lead sees the whole picture.** It is the only agent that knows
   every step, so it's the right one to fix problems where the workers'
   pieces meet.

### What goes wrong, and the safeguards

| Problem                                                                                                                      | Safeguard                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Workers write code blind.** Without a build, a worker can leave dozens of type errors that only appear later, all at once. | **Light checks only.** Workers get `diagnostics`, the language server's errors for the files they changed (TypeScript in-app, rust-analyzer when installed). That catches most type errors without a build. The `bash` tool refuses commands that `workload/classify.rs` rates `Heavy`, with "The lead builds after merging."                                                                              |
| **Workers guess at shared interfaces differently.** Each worker invents the signature it needs.                              | **Contract first.** Before a parallel group, the plan has a sequential "contract" step that the lead (or one worker) does. It adds the shared types, function signatures, command names and settings keys, with stub bodies. Workers fill in bodies against the contract. The plan checker requires a contract step before any group in which more than one step names a shared file as read-only context. |
| **One failure, five suspects.** A failing build or test after the merge doesn't say which worker caused it.                  | **Attribution by write scope.** `error_locations` in `command_output.rs` already finds file paths in failure output. Each location is matched to the worker whose write scope owns it, and the failure message to the lead says "src/a.rs:40 (step 3, worker w2)". Failures in files no worker owns are seams, and they go to the lead.                                                                    |
| **Is the code wrong, or the test?** The test writer may have misread the request.                                            | **Rule for changing tests.** The lead may change a test-writer test only by citing the request or the plan, and every such change is listed in its final message to the user. An optional **red check** (E04) runs the new tests on the contract-only code before workers merge. A test that already passes there doesn't test the new behaviour and is flagged.                                           |
| **The lead's context fills with build output.**                                                                              | **Triage (kind C) handles large failures.** The lead receives a root cause and a file list, not 8,000 characters of compiler output.                                                                                                                                                                                                                                                                       |
| **The fix loop becomes sequential again.** After the merge, the workers' overlays are gone.                                  | **Resume the owning worker.** `task{continue: "w2", …}` resumes worker w2's chat, with its context, as a **sequential** worker in the parent overlay. It's used for failures entirely inside one worker's scope. The lead fixes seams itself.                                                                                                                                                              |

### The flow (kinds F and E together)

```
1  Lead: contract step          shared types/signatures, stub bodies      (sequential, in chat overlay)
2  Fork                         ┌ W1  scope: src-tauri/src/foo/**          child overlay on chat view
                                ├ W2  scope: src/components/Foo*.tsx       child overlay on chat view
                                └ T   scope: test paths only, from spec    child overlay on chat view
                                  each: read tools + write (in scope) + diagnostics + light bash
3  (optional) red check         T's tests on contract-only code: pass → flagged "doesn't test the change"
4  Join                         land W1, W2, T into chat overlay, one at a time (land.rs)
                                scopes are disjoint → conflicts only on bugs; any conflict → lead
5  Lead: build + all checks     one build, typecheck, lint, full tests + T's tests     (workload queue)
6  Failures                     large → triage (C); attribute by scope
                                  inside one scope → resume that worker (sequential)
                                  seams / unowned → lead fixes
                                  (the repair loop below: ≤ 3 rounds, progress-checked)
7  Review (B) → verification gate → land into project, as today
```

The expected gain is modest and depends on the task. Parallelism saves
model time, not build time. It pays off when steps are large, independent
and mostly writing code: a backend command and its UI page, or three
separate settings pages. It costs more than it saves on small groups. So F
stays off by default, and the lead uses it only for groups of two or more
steps that each change more than about 100 lines (a prompt rule, measured
in milestone G).

### When the merged build or tests fail: the repair loop

**No agent is started per failed file.** One root cause usually breaks
many files: a changed signature can fail at twenty call sites. Twenty
fixers would each patch a symptom, and they would repeat the coordination
problem that parallel writers have. The loop works on **root causes**,
fixes them **one writer at a time**, and rebuilds **once per round**.

#### One repair round

1. **Fingerprint the failures.** Each failure gets a stable key:
   - for a build or type error: check, file, error code and message
     without line numbers;
   - for a test: the test's name.

   The set of keys is this round's **failure set** (R01).

2. **Group the failures into root causes.**
   - **Small output:** group by the error locations (`error_locations`)
     and by which worker's write scope owns each file.
   - **Large output or unclear grouping:** triage (C) reads the whole
     output and returns root causes, each with its files and a
     confidence.

   Type errors come first. The gate already skips tests when the type
   check fails, because they would only repeat the same errors.

3. **Assign each root cause.**

   | Root cause                                              | Fixed by                                                 |
   | ------------------------------------------------------- | -------------------------------------------------------- |
   | Entirely inside one worker's scope                      | That worker, resumed with its context (`task{continue}`) |
   | Across scopes, in no worker's files, or in the contract | The lead                                                 |
   | In a test the test writer wrote                         | The lead, under the rule for changing tests (section 3)  |

4. **Fix one cause at a time** in the chat overlay, in order of how many
   failures each cause explains. Parallel fixing is allowed only when all
   of these are true:
   - there are two or more causes;
   - each cause is inside a different worker's scope;
   - triage rated each cause high confidence.

   It is off by default (R07), because fixes are usually small and the
   build, not the model, is the slow part.

5. **Rebuild once** and run all the checks again.

#### Making sure the loop ends with a correct result

"Correct" means: **every check passes, the tests weren't weakened to get
there, and a fresh review found nothing serious.** The loop can't
guarantee more than the checks and tests cover. It does guarantee that
nothing is reported as done, or landed automatically, without meeting
this bar.

| Guard                                     | How it works                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **The loop is bounded**                   | At most `MAX_VERIFY_ROUNDS = 3` rounds, as today, and at most 12 subagents per run (D11). Each round starts at most one fixer per root cause.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| **Each round must make progress** (R01)   | A round **made progress** when its failure set is smaller than the last one and contains no new key. If the set is unchanged, or the same as two rounds ago, the round made **no progress**. A different agent shouldn't repeat the same attempt, so the loop goes straight to the next level of the escalation ladder below instead of retrying.                                                                                                                                                                                                                                                                                               |
| **Regressions are rolled back** (R02)     | A checkpoint of the chat overlay is taken before each repair round. A key that was passing before the round (a test that used to pass) and fails after it is a regression. That round's changes are rolled back to the checkpoint, and the lead is told what the attempt broke.                                                                                                                                                                                                                                                                                                                                                                 |
| **Tests can't be weakened to pass** (R05) | After every round, the harness compares the test files and the check setup with the start of the run. It flags removed test functions, added `skip`, `only` and `#[ignore]` markers, and fewer assertions. It also flags changes to what the checks run: `package.json` scripts, `Cargo.toml`, tsconfig, lint and test-runner configuration. A flagged change to a test that existed before the run counts as a failure unless the lead explains it, and every flag is listed for you. The number of tests that ran, before and after, is in the verification record.                                                                           |
| **The escalation ladder**                 | Round 1: the owners fix their causes (workers resumed, the lead fixes seams). Round 2, or a round that made no progress: **the lead alone**, with every cause and the whole picture. Round 3 is the last. If it still fails, the run ends as **gave-up**, not done. The record shows the remaining failures, the triage summary and the rolled-back attempts. Nothing lands automatically, and you choose between **Continue**, **Land anyway**, **Discard** and **Redo sequentially**. Redo sequentially throws away the group's merged changes and redoes those steps one at a time with step workers (D), the slower but more reliable path. |
| **A fresh review after green**            | Once every check passes, the change reviewer (B) reads the whole diff, with no knowledge of the repair attempts. It catches code that passes the tests but is wrong. Its high-severity findings go back to the lead, and the checks run again after any fix.                                                                                                                                                                                                                                                                                                                                                                                    |
| **Checks again when landing**             | This already exists: landing re-runs the checks on the chat's changes merged with the project as it is now, so work that changed meanwhile can't break it silently.                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |

R01, R02 and R05 also help **a single agent with no subagents**. Today's
gate retries 3 times, but it can't tell progress from churn, doesn't roll
back regressions, and doesn't notice a weakened test. They can ship before
the rest of the subagent work.

---

## 4. Milestones

Each task below names files and has a "Done when" line with its tests. All
e2e tests use the scripted provider in `agent_run/e2e_tests.rs`.

### Milestone S: shared foundation

| ID  | Task                                                                                                                                                                                                                           | Needs    | Done when                                                                                                                          |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| S01 | `SCHEMA_V8`: `dialogs.parent_id`, `parent_call_id`, `kind`; `store::create_child_dialog`; `list_dialogs`, `dialogs_in_project` and search leave out children; deleting a parent deletes its children                           | –        | Migration test from v7; child chats absent from list and search; cascade delete test                                               |
| S02 | `subagent.rs`: `Kind`, `KindSpec`, `Brief` (validated: goal required, scope and budget clamped), `SubResult` (parsed from the final message's sections, capped at 6,000 characters with a note); no callers yet                | –        | Unit tests: parsing well-formed, missing and oversized results; brief validation                                                   |
| S03 | `Run.role` and `Run::prepare_child()`: child runs share the parent's root, project context, repo map and checks, and get the kind's tools and prompt. `drive()` skips the checklist, the verify gate and CLM for `Sub`         | S01, S02 | e2e: a child run started directly reads a file and ends; no verification record; no `todo` offered                                 |
| S04 | `AgentState`: child runs registered under the parent; Stop on the parent stops children; approvals carry `parent_dialog_id`; settings fields in `AgentConfig`                                                                  | S03      | e2e: Stop on the parent during a child's tool call stops both; a child's approval appears with the parent's id                     |
| S05 | Budgets (D10): tool-call count through `guard.rs`, wall time through a timer on the stop watch, a final result turn without tools; status `done`, `partial`, `blocked`, `failed` or `stopped`                                  | S03      | e2e: a scripted child that never stops gets a result turn at the limit and returns `partial`                                       |
| S06 | `task` tool: schema, `tools_for(role)`, dispatch in `execute.rs`, record and result; read-only `task` calls batch in parallel with a limit of 3                                                                                | S04, S05 | e2e: three `task` calls in one turn run at the same time (`parallel::batches_of`); results come back in call order; a fourth waits |
| S07 | Events and frontend: `SubagentStarted`, `Progress`, `Ended`; `agentRuns.ts` routing; `ToolCallBlock` `task` view; opening the child transcript                                                                                 | S06      | `npm test` for event routing and the block's summary; manual check in the app                                                      |
| S08 | Settings: `SubagentSettings.tsx`, `aiSettings.ts` fields, `AgentConfig` wiring; all kinds off (D14)                                                                                                                            | S06      | `npm test` for defaults and saving; with all kinds off, `task` isn't offered (e2e)                                                 |
| S09 | Logging: every subagent request tagged with role and kind in `ROVIK_API_LOG`; one `subagent` line in the run log per child (kind, model, tool calls, input and output tokens when the provider reports them, duration, status) | S06      | Unit test of the log line; log file from an e2e run contains the tags                                                              |

### Milestone A: scout

| ID  | Task                                                                                                                                                                                                                                                                | Needs | Done when                                                                                      |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ---------------------------------------------------------------------------------------------- |
| A01 | Scout `KindSpec`: tools = `PARALLEL_TOOLS` + `web_search`, `browse_url`; budget of 20 calls and 3 minutes; prompt asks for an answer, `path:line` evidence, and open questions                                                                                      | S06   | e2e: a scout's `write` and `bash` calls are refused; the result has the required sections      |
| A02 | Lead prompt: when to delegate (more than about 15 reads, two or more unrelated areas, a broad question) and when not to (small tasks); the brief template; at most 4 scouts per turn. In `run.rs`'s system prompt for Agent and Plan modes, only when scouts are on | A01   | Snapshot test of the prompt with scouts on and off                                             |
| A03 | Plan mode: `task{kind:"scout"}` allowed, though `PLAN_BLOCKED_TOOLS` still blocks `write` and `bash`                                                                                                                                                                | A01   | e2e: a Plan-mode run sends two scouts, then `submit_plan`                                      |
| A04 | Follow-ups: `task{continue: "<child>", goal}` resumes a finished scout with its context, for a cheap follow-up question                                                                                                                                             | A01   | e2e: a resumed scout sees its earlier messages; resuming a running or unknown child is refused |

### Milestone B: change reviewer and plan reviewer

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Needs    | Done when                                                                                                                                                                                       |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B01 | Reviewer `KindSpec`: read-only tools; the harness writes the brief, not the lead: the user's request (`latest_user_request`), the plan (`chat_plan_file`), the checklist, and the chat's diff (overlay `changes()`, or `changed_files` + `diff.rs`). The lead's history is not included. The prompt is based on the reviewer prompt in `PROMPTS.md`                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | S05      | Unit test: the brief contains the diff and request and nothing from the lead's history                                                                                                          |
| B02 | Findings format: `severity` (high, medium or low), `path:line`, scenario, suggested fix, confidence. Parsed strictly; an unparseable reply counts as "no findings" and is logged                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | B01      | Parser tests on good, partial and garbage replies                                                                                                                                               |
| B03 | Hook in `run.rs` `drive()`: when the lead says it's done, after the checklist and marker checks and **before** `verify_at_end`. It runs only if files changed, the diff is at least 10 changed lines (setting), and fewer than `MAX_REVIEW_ROUNDS = 2` rounds have run. High and medium findings go to the lead as an `IDE_NOTE` harness message; the lead answers each one ("fixed" or "not a bug, because …")                                                                                                                                                                                                                                                                                                                                                                                                                   | B02      | e2e: findings, then a lead fix turn, then a second review, then the verify gate; a third round never happens; a reviewer model error skips review with a note and the run continues             |
| B04 | Round 2 gets round 1's findings and the lead's answers, and must not repeat a finding the lead rejected unless it brings new evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | B03      | e2e: a repeated finding without new evidence is dropped by the parser                                                                                                                           |
| B05 | UI: `ReviewRecord.tsx` (like `VerificationRecord.tsx`), stored as a record message with `meta.review`, never sent to the model                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | B03      | `npm test` for the record view; the record survives a restart (e2e reads it back)                                                                                                               |
| B06 | A reviewer model of its own (`review_profiles`); when unset, the subagent model is used                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | B03, S08 | e2e: the reviewer request goes to the configured model                                                                                                                                          |
| B07 | Plan checks that need no model, added to `plan::plan_problems` (run on every `submit_plan`): every path in a step that isn't marked "(new)" exists in the project; a step doesn't use a file that a **later** step creates; a group's steps don't share a file (F03's check, moved here so Plan mode has it from the start)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | D01      | Checker tests: a missing path, a "(new)" path, a file used before the step that creates it, an overlapping group                                                                                |
| B08 | Plan reviewer `KindSpec`: read-only tools (`read`, `search_code`, `outline`, `callers`, `neighbours`); the harness writes the brief: the user's request, the answers to `ask_user`, and the plan. The lead's history and scout reports are left out, so it reads the code itself. The reviewer checks for: callers and dependents a step changes but the plan doesn't update; functions, types or settings that don't exist or have other names; steps in the wrong order; scope beyond the request, or parts of it left out; a weak Validation section (no tests for new behaviour); and, for groups, shared registration points (`lib.rs` command lists, `commands.ts`, `mod.rs`, routes) that several steps would have to edit, plus a missing contract step. Findings use B02's format, with a step number in place of a line | B02      | Unit test of the brief (no lead history); parser tests with step-number findings                                                                                                                |
| B09 | Hook in `execute.rs` `submit_plan`, after `plan_problems` passes and before `save_plan_file`. When the plan reviewer is on, it reviews the plan. High and medium findings mean the plan is **not saved yet**, and the tool result lists the findings for the lead to fix or answer ("not a problem, because …"). At most `MAX_PLAN_REVIEW_ROUNDS = 2` per run. After the last round the plan is saved, and any findings still open go into an "## Open review findings" section at the end of the plan for the user. A revised plan that changes fewer than 10 lines from the saved version is saved without a review                                                                                                                                                                                                             | B07, B08 | e2e: findings → not saved → the lead fixes → saved on the second submit; a third submit is never reviewed; the open findings section appears; a reviewer model error saves the plan with a note |
| B10 | UI: the plan review shown as a record under the plan message, through `ReviewRecord.tsx` (B05) with `kind: "plan"`; a switch on the Subagents page; it uses the reviewer model (B06)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | B05, B09 | `npm test` for the record view; manual check in Plan mode                                                                                                                                       |

### Milestone C: failure triage

| ID  | Task                                                                                                                                                                                                                                                   | Needs | Done when                                                                                                                        |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----- | -------------------------------------------------------------------------------------------------------------------------------- |
| C01 | Full check output saved to a file in the run's store (overlay `store/` or the app's temp folder), so a subagent can read it in pieces                                                                                                                  | S03   | Unit test: output over `FAILURE_OUTPUT_CHARS` is saved whole; files cleaned when the chat lands or is discarded                  |
| C02 | Triage `KindSpec`: read-only tools; brief from the harness: check kind, command, path of the saved output, `error_locations`, `split_by_changed` result, changed files. Result: root cause, files to fix, smallest next step, confidence               | C01   | e2e: the triage brief names the output file; triage can't write                                                                  |
| C03 | Hook in `verify.rs`: triage runs when a failure's output is longer than `FAILURE_OUTPUT_CHARS`, or the same check failed in the previous round. The lead's fix message gets the triage result plus a short `focus_failures` excerpt (2,000 characters) | C02   | e2e: large output → triage summary in the fix message; small output → today's message, unchanged; triage error → today's message |

### Milestone D: sequential step worker

| ID  | Task                                                                                                                                                                                                                                     | Needs    | Done when                                                                                                                                 |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| D01 | `plan::steps(markdown) -> Vec<PlanStep>` (number, title, files, body) and `plan::section(markdown, "Decisions")`, reusing `STEPS_HEADING`                                                                                                | –        | Unit tests on the plans in `.rovik/plans/` and the checker's fixtures                                                                     |
| D02 | Worker `KindSpec`: read tools + `write` + `bash` (the parent's permissions) + `diagnostics`; budget of 60 calls and 15 minutes; writes into the parent's root (D6); write scope = the step's files, plus new files in their folders (D7) | S06, D01 | e2e: a write outside the scope is refused with the scope in the message                                                                   |
| D03 | `task{kind:"worker", step: n}`: the harness writes the brief: plan title and summary, Decisions, the step's text, one-line summaries of finished steps, and the current diff of the step's files                                         | D02      | Unit test of the brief; e2e: step 2's worker sees step 1's changes                                                                        |
| D04 | After a worker: changed files are taken from the overlay (before/after), not from the worker's report. A difference is noted to the lead. The matching checklist item is marked done                                                     | D03      | e2e: a worker that reports a file it didn't change → note to the lead                                                                     |
| D05 | Lead prompt: plans with at least 6 steps (setting) run step by step through workers; the lead reads each result, may resume a worker once (`continue`) or do a step itself                                                               | D03      | e2e: a 3-step scripted plan (threshold lowered) → three workers in order; the lead's context holds summaries, not the workers' tool calls |

### Milestone E: test writer

| ID  | Task                                                                                                                                                                                                                                                                                                                                                   | Needs    | Done when                                                                  |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- | -------------------------------------------------------------------------- |
| E01 | `project.rs`: test path conventions for each detected stack (`*.test.ts(x)`, `__tests__/`, `tests/`, `*_test.go`, `test_*.py`, Rust `tests/` and separate `tests.rs` modules, as this repository uses) and `test_command_for(files)` (vitest/jest by path, `cargo test` by test target or module, `pytest` by path; falls back to the full test check) | –        | Fixture tests per stack                                                    |
| E02 | Test writer `KindSpec`: read tools + `write` limited to test paths (E01) + `diagnostics` + light `bash`; the brief has the request, the plan, and the public interfaces of the files the plan changes (`outline`), not the implementation                                                                                                              | E01, D02 | e2e: a write to `src/x.ts` is refused; a write to `src/x.test.ts` works    |
| E03 | Sequential use: the lead calls the test writer after implementing, and its tests run with the verify gate (they're ordinary test files). The lead's prompt has the rule for changing tests (section 3), and the final message lists any test-writer test the lead changed                                                                              | E02      | e2e: a lead edit to a test-writer file appears in the final message's list |
| E04 | Red check (optional, setting): the new tests run on the code **before** the implementation (the overlay at the contract point, or the project for a sequential run). Tests that pass there are flagged to the lead as "doesn't test the change"                                                                                                        | E02, F02 | e2e: a test that passes on the old code is flagged; a failing one isn't    |

### Milestone F: parallel implementers

| ID  | Task                                                                                                                                                                                                                                                                                                                                              | Needs              | Done when                                                                                                                                           |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| F01 | **Spike: stacked overlays.** Open an `OverlayFs` whose base is the parent chat's view, either through the mount or through an in-process `Base` interface on `OverlayFs`, then land it back into the parent with `land.rs` (project = the parent's view). Write the result under "Notes from implementation". Isolation off → F stays unavailable | –                  | A test: child writes, then lands into the parent; the parent sees the change; the project is unchanged; a crash during the child's landing recovers |
| F02 | `overlay/chats.rs`: `open_child(parent, child)`, `land_child(child)` and `discard_child(child)`, with the child's limit counted inside the parent's 512 MiB                                                                                                                                                                                       | F01                | Tests: two children of one parent land one after another; their space is counted in the parent's budget                                             |
| F03 | Plan groups: `Group: <name>` on steps; the plan checker refuses a group whose steps' file lists overlap, and a group without a contract step before it when its steps share a read-only file (section 3)                                                                                                                                          | D01                | Checker tests: overlapping groups, missing contract step, valid groups                                                                              |
| F04 | `task{kind:"workers", group}`: forks one worker per step (and optionally the test writer) into child overlays, runs up to 3 at once with scopes from the plan, then joins them by landing children one at a time. A conflict stops the join and goes to the lead                                                                                  | F02, F03, D03, E02 | e2e: two workers in parallel → both land into the chat overlay → the project is unchanged; a forced conflict goes to the lead with both versions    |
| F05 | Workers in a group: `bash` refuses `Heavy` commands (`workload/classify.rs`) with "The lead builds after merging"; `diagnostics` is on                                                                                                                                                                                                            | F04                | e2e: `cargo build` in a group worker is refused; `ls` runs                                                                                          |
| F06 | After the join: the lead runs the checks at once (`verify_now`, the same code as `verify_at_end`, called in the middle of a run) and gets failures with attribution by scope ("step 3, worker w2", or "seam")                                                                                                                                     | F04, C03           | Unit test of attribution on fixture outputs; e2e: a failure in w2's file is labelled w2                                                             |
| F07 | Fixes: `task{continue:"w2"}` resumes a group worker as a sequential worker in the chat overlay, with its scope; seams are fixed by the lead                                                                                                                                                                                                       | F06, A04           | e2e: a resumed worker writes into the chat overlay, inside its old scope only                                                                       |
| F08 | Lead prompt: when to use a group (two or more steps that each change more than about 100 lines, isolation on); the contract step comes first                                                                                                                                                                                                      | F04                | Snapshot test of the prompt                                                                                                                         |

### Milestone R: the repair loop

R01, R02 and R05 don't need subagents. They improve today's verify gate
and can be done first.

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                      | Needs         | Done when                                                                                                                                                                   |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R01 | `verify.rs` + `command_output.rs`: a failure key for each failure (error: check, file, error code, message without line numbers; test: its name, parsed for cargo, vitest/jest and pytest output, otherwise the check alone). Each round's failure set is stored in the verification record. Progress, no progress and regression decided as in section 3, and a round with no progress tells the lead so | –             | Unit tests on fixture outputs: line shifts give the same key; progress, no progress and regression are told apart; e2e: two identical failing rounds → the no-progress note |
| R02 | Checkpoints: before a repair round, save the overlay's changed files (or, in direct mode, the run's `changed_files`) and restore them on a regression. The rolled-back attempt's diff is kept in the record                                                                                                                                                                                               | R01           | e2e: a fix that breaks a passing test is rolled back; the lead's next message names what it broke                                                                           |
| R03 | Grouping and assignment: failures grouped by location and write scope, or by triage's root causes when C03 ran, then assigned to owner, lead or test rule (section 3)                                                                                                                                                                                                                                     | R01, C03, F06 | Unit tests: twenty errors from one changed signature → one cause owned by the lead (contract); errors only in w2's files → w2                                               |
| R04 | Sequential repair runner: causes fixed one at a time, ordered by the number of failures each explains, then one rebuild per round                                                                                                                                                                                                                                                                         | R03, F07      | e2e: two causes → two fixers in order → one check run                                                                                                                       |
| R05 | Test integrity: compares test files and check setup with the run's start (removed tests, `skip`/`only`/`#[ignore]`, fewer assertions, changed scripts and configs); the number of tests that ran in the record; flags listed in the final message                                                                                                                                                         | E01           | Fixture tests per stack; e2e: a run that adds `.skip` to an old test to pass is not reported as verified                                                                    |
| R06 | Escalation ladder and end states: lead-only round after no progress; **gave-up** record with the remaining failures and rolled-back attempts; **Redo sequentially** in the isolation bar (discard the group's merge, then run its steps through D)                                                                                                                                                        | R02, R04      | e2e: three failing rounds → gave-up, nothing landed; Redo sequentially → step workers run on the group's steps                                                              |
| R07 | Optional parallel repair (off by default): only for causes in different scopes, each rated high confidence by triage, in child overlays (F02), with one rebuild after they're merged                                                                                                                                                                                                                      | R04, F02      | e2e: two high-confidence causes in different scopes → fixed in parallel → one check run; a cause owned by the lead → never parallel                                         |
| R08 | Review after green: the change reviewer (B03) runs once the checks pass after a repair; its high-severity findings start one more fix and check round                                                                                                                                                                                                                                                     | R04, B03      | e2e: green → review → a finding → fix → checks again                                                                                                                        |

### Milestone G: measure and switch on

| ID  | Task                                                                                                                                                                                                                 | Needs      | Done when                                                           |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | ------------------------------------------------------------------- |
| G01 | Evaluation set: 20–40 tasks from this repository's merged pull requests (request text, starting commit, how to check the result), in `scripts/eval/` with a runner that starts headless runs with a settings profile | S09        | The runner completes the set with subagents off and writes a report |
| G02 | Baseline with all kinds off; then each milestone's kind on by itself. Report: success, cost, peak lead context, time, interventions (`subagents.md` section 6)                                                       | G01 + kind | Report committed as `docs/design/subagents-results.md`              |
| G03 | Defaults: a kind is turned on by default only if it improved success or lead context and stayed within its cost budget. The decision and its numbers go in the results file                                          | G02        | Settings defaults changed in one PR that links the results          |

---

## 5. Implementation order for an AI agent

### Rules for every task

1. **One task per branch and pull request.** Don't start the next task
   until the current one is merged.
2. **Read first:** the files the task names, this plan's sections 1–3,
   and `subagents.md`. Follow the surrounding code's style: module doc
   comments that explain _why_, small functions, tests next to the code.
3. **Keep the app working.** Everything sits behind the Subagents
   settings, with all kinds off (D14). A chat with every kind off behaves
   exactly as today; the existing e2e suite must pass unchanged.
4. **Checks before every push:**
   - `cd src-tauri && cargo check && cargo test`
   - `npm test`
   - `npx tsc --noEmit`
   - `npm run format:check`
5. **Tests in the same task:** each task's "Done when" names them.
6. **When a task doesn't fit reality,** don't improvise a new design. Add
   a note under "Notes from implementation" below, take the smallest safe
   path, and say so in the pull request.
7. **Tauri commands:** a new or changed command is registered in `lib.rs`
   and given a typed wrapper in `src/lib/commands.ts` in the same task.

### Order at a glance

```
S:  S01 ─► S02 ─► S03 ─► S04 ─► S05 ─► S06 ─► S07 ─► S08 ─► S09
G:                                                      └─► G01 (baseline early)
A:  A01 ─► A02 ─► A03 ─► A04                    (needs S06)
B:  B01 ─► B02 ─► B03 ─► B04 ─► B05 ─► B06       (needs S05)
    B07 (needs D01) ─► B08 ─► B09 ─► B10           plan reviewer: B08 needs B02, B10 needs B05
C:  C01 ─► C02 ─► C03                           (needs S03)
D:  D01 ─► D02 ─► D03 ─► D04 ─► D05             (D01 can start any time)
E:  E01 ─► E02 ─► E03                           (needs D02); E04 after F02
F:  F01 (spike, can start any time) ─► F02 ─► F03 ─► F04 ─► F05 ─► F06 ─► F07 ─► F08
R:  R01 ─► R02 ─► R05 (can start any time: they improve today's gate)
    R03 ─► R04 ─► R06 ─► R07 ─► R08   (after F07)
G:  G02 after each milestone ─► G03
```

After S, milestones A, B and C touch different files and can run in
parallel. D comes before E, and both before F, because F reuses the step
briefs (D03), the test writer (E02) and triage (C03). F01 is a spike with
the highest technical risk, so it should start early, alongside S.

Rough size: S is the largest part (about a third of the work), then F,
B, D, E, A, C. G01 should be ready before A is turned on, so every kind
is measured against the same baseline.

---

## 6. Open questions

These don't block S–C. Each should be decided before the milestone it
names.

1. **Review before or after the verify gate (B03)?** This plan reviews
   first, so a build isn't spent on code with obvious logic bugs. If
   measurements show most reviews are followed by failing checks anyway,
   swap the order.
2. **Stacked overlays (F01):** use the mounted view as the base, or add an
   in-process `Base` interface? The spike decides. The mount is less code,
   but it needs the virtual files driver.
3. **Token budgets:** providers report usage differently, and some don't
   report it at all. Budgets use tool calls and time until usage reporting
   is reliable across providers (S09 collects the data).
4. **Child chats in search (S01):** hidden for now. Searching inside
   subagent transcripts could come later, as a filter.
5. **Plan reviewer before or after the user sees the plan (B09)?** This
   plan reviews it before it's saved, so you only see reviewed plans. The
   other choice is to save it at once and review in the background. That
   is faster, but the plan can change while you read it. Measure how long
   a plan review takes in G02 before deciding.

## Notes from implementation

_None yet._
