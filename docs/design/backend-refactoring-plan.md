# Backend refactoring: plan

Follows a review of the backend at `abcc2e6`. The crate boundaries and the
connector traits held up; the problems sit in the agent's run
(`rovik-core/src/agent_run`) and in policy written out more than once. The
review made six findings, and each was traced to the code. Three
behaviours were confirmed by running throwaway tests (not committed), and
one more bug turned up on the way.

Line numbers are at `b5898ed`, today's master. Every file they point to is
the same at `abcc2e6`, the reviewed commit, except `README.md`: master
added three lines near its top, so its line 101 is line 98 there.

Two changes were asked for on top of the review, and are planned here as
phase 0: `ask_user` in Agent mode as in Plan mode, and the four working
principles of `AGENTS.md` in the agent's system prompt.

**The goal:** nothing decides what happened from text written for the
model, and adding a tool, a piece of a run's state or a message column is a
change in one place that the compiler or a test checks. Nothing a user sees
changes, except what "Decisions to confirm" lists.

A `.rs` file named alone (`tools.rs`) is in
`src-tauri/crates/rovik-core/src/agent_run/`. Other Rust paths start at
`rovik-core/src/` (`chat_db/store.rs`) or, for a crate's own files, at
`src-tauri/crates/` (`rovik-headless/src/lib.rs`). A `.ts` or `.tsx` file
is under `src/`, the frontend.

## The findings, as checked

| #   | Finding                               | Priority | Checked                          | What the code shows                                                                                                                                                                         |
| --- | ------------------------------------- | -------- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | A call's status is read from its text | High     | Confirmed; three bugs reproduced | 7 places in Rust (one unreachable) and 2 in the frontend decide "failed" from an `Error` prefix, in three spellings; 3 parsers take a command's exit code from its result's second line     |
| 2   | `Run` does too much                   | Medium   | Confirmed                        | 138 fields, 135 of them `pub(super)`; `impl Run` blocks in 23 files; a fix round's state is copied field by field into `VerifyCarry` and back                                               |
| 3   | Tool policy lives in many lists       | Medium   | Confirmed; no bypass today       | 21 built-in tools, more than 40 places that match their names as strings, 11 per-kind allow-lists; 5 default arms fail open                                                                 |
| 4   | `plan.rs` mixes concerns              | Medium   | Confirmed; no disagreement found | 2,484 lines of code in ten concerns; 11 functions start again from the raw Markdown; checks and step extraction run the same pipeline twice; `submit_plan` parses one plan up to five times |
| 5   | The message row mapping is repeated   | Lower    | Confirmed; no mismatch today     | The same 14 columns in `chat_db/store.rs` and `chat_db/sync.rs`, and twice more on the write side; no test would catch them drifting                                                        |
| 6   | The process runtime hides in globals  | Lower    | Confirmed; on purpose (D2, K18)  | `Core::new` and `shutdown` drive about ten process-wide registries; the hosts set more; the headless runner skips both                                                                      |

## Phases at a glance

| Phase | What                                                               | Finding   | Why in this order                                          | Size     |
| ----- | ------------------------------------------------------------------ | --------- | ---------------------------------------------------------- | -------- |
| 0     | `ask_user` in Agent mode; the four principles in the system prompt | asked for | Small, and the principles need the tool to ask with: first | 1 day    |
| 1     | A tool call carries its outcome                                    | 1         | Fixes bugs users see; touches every tool path first        | 3–4 days |
| 2     | One table of built-in tools                                        | 3         | The same files as phase 1, so right after it               | 2–3 days |
| 3     | `plan.rs` split, and a plan parsed once                            | 4         | It moves `plan.rs` code that phases 1–2 change: after them | 2–3 days |
| 4     | `Run`'s state in objects of its own                                | 2         | The widest change: last, so 0–3 don't rebase over it       | 6–8 days |
| 5     | One decoder for a message row                                      | 5         | Small; after 1.1, which adds to the type it decodes into   | 1 day    |
| 6     | An owner for the process runtime (deferred)                        | 6         | When `Core::new`, `shutdown` or a host's start next change | —        |

Phases 0 to 4 go in order. Phase 3 moves what phases 1 and 2 change in
`plan.rs` (`describe_todo`, `describe_outcome`, `current_checklist`,
`PLAN_BLOCKED_TOOLS`, the checklist gate), and phase 1.1 adds
`MessageRow::call_failed` beside the type phase 5 decodes into
(`chat_db/store.rs:126`); phase 5 can go any time after that. Each phase is
a few commits, one topic each, every one green on its own (`AGENTS.md`'s
checks). Each task below says what the code does today, the change, the
tests that prove it (named as they'll be written) and when it's done.

## Decisions to confirm

Everything else in this plan keeps behaviour as it is. These change it:

1. **Bugs fixed** (phase 1, 2.3): a successful call whose text starts
   with `Error:` counts as a success; a command's status is read right
   whatever its length; a checklist the IDE refused isn't the current one;
   without a project, CLM offers for `@context/` only the three tools that
   work there. Recommended: yes, each with its test.
2. **Asked for** (phase 0): `ask_user` in Agent mode as in Plan mode
   (Autopilot and subagents still don't ask: nobody is there to answer),
   and the four principles in the agent's system prompt.
3. **How the principles meet the prompt that's there** (0.2). Two of its
   lines pull against them: "Only ask the user a clarifying question when
   the task is genuinely ambiguous … Otherwise, make a reasonable
   assumption, state it briefly, and proceed" against "If uncertain, ask"
   and "don't pick silently"; and the `todo` rule against "state a brief
   plan". Recommended: the four sections word for word; the clarifying line
   goes (when to ask is principle 1's, and the line in its place says how:
   with `ask_user`); the `todo` rule says its checklist is principle 4's
   brief plan; Autopilot swaps principle 1's asking lines for deciding and
   recording, as it swaps the clarifying line today. The other way, adding
   them and changing nothing, leaves the prompt telling the model both to
   ask when unsure and to assume and go on.
4. **A tool the mode doesn't offer is refused when called** (2.3). Today
   `todo` runs in Plan mode and `submit_plan` in Agent mode if a model calls
   them anyway (from an earlier turn's history, say). At dispatch, Plan
   mode refuses `write`, `bash` and `background` only, and limits `mcp` to
   the tools a server marks read-only (`run.rs:1478`, which stays: it
   depends on the server). Recommended: refuse, so what's offered and
   what's accepted come from one row; the price is a model that relied on
   the old leniency getting an error it can read.
5. **A malformed JSON column is logged** when a message row is read
   (phase 5), where today it's dropped without a word. The row still
   loads. Recommended: yes (`AGENTS.md`: never swallow an error silently).

---

## Phase 0: the agent asks in Agent mode, and works by four principles

Asked for on top of the review. The two go together: the first principle
tells the model to ask when it's unsure, and in Agent mode today it has no
tool to ask with. Both are small and stand apart from the refactoring, so
they go first.

### Today

- `ask_user` is offered in Plan mode only (`tools.rs:579`), and its
  handler refuses it in any other mode (`execute.rs:1917`). Autopilot takes
  it out of the list (`autopilot::without_questions`, at `run.rs:2194` and
  `rovik-core/src/agent.rs:1895`) and answers a call with "nobody answers
  questions" (`execute.rs:1906`). No subagent kind lists it. A run asks at
  most `MAX_QUESTIONS_PER_RUN` (3) times (`plan.rs:41`). The question panel
  (`QuestionPanel.tsx`) doesn't depend on the mode.
- The agent's system prompt is `CODING_AGENT_SYSTEM_PROMPT`
  (`rovik-core/src/agent.rs:740`), through `lead_system_prompt` (`:782`).
  The lead gets it in Agent and Plan mode (`:1000`; Plan mode adds its own
  instructions to the project's section, `run.rs:2265`), Autopilot's lead
  with its asking line swapped (`rovik-core/src/agent.rs:793`,
  `run.rs:2291`), and a connector's harness too (`connectors/mod.rs:576`).
  Subagents have prompts of their own. The context estimate counts the
  prompt (`rovik-core/src/agent.rs:1964`); a model without tools gets it
  without "## Tool use rules" (`without_tool_rules`, `:2031`).
- Two of its lines pull against the principles (decision 3): the
  clarifying line (`CLARIFYING_QUESTIONS`, `rovik-core/src/agent.rs:776`)
  and the `todo` rule (`:753`).

### Steps

**0.1 `ask_user` in Agent mode as in Plan mode**

- `tools_for` (`tools.rs:545`) offers it in both modes, and the handler's
  Plan-mode refusal goes. Autopilot and subagents stay as they are.
- What's worded for Plan mode follows: the tool's description ("before
  planning further", `tools.rs:353`), the answer past the limit ("list your
  assumptions under "Decisions" in the plan"; in Agent mode, "say what you
  assumed"), and the docs: `README.md:101`, `plan.rs`'s module comment,
  `src/lib/questions.ts:3`, and the `questions_asked` field's comment
  (`run.rs:167`).
- The limit stays 3 a run, in either mode.
- Tests:
  - `ask_user_outside_plan_mode_is_refused` (`e2e_tests.rs:2737`) pins the
    old rule. It's rewritten for the new one, as
    `an_agent_mode_run_asks_the_user_and_goes_on_with_the_answer`: the
    questions are shown (`questionsAsked`) and the answer is the call's
    result.
  - `tool_lists_by_mode` (`tools.rs:2310`) gains `ask_user` in Agent mode's
    list.
  - `a_plan_that_asks_is_refused_and_submits_with_assumptions`
    (`autopilot_e2e_tests.rs:1034`) passes unchanged: Autopilot still asks
    nobody.

**0.2 The four principles in the system prompt**

- The four sections, word for word as given (they're the first four
  sections of `AGENTS.md`), go into `CODING_AGENT_SYSTEM_PROMPT` after
  "## Core behavior". So everything that uses the prompt gets them (Agent
  and Plan mode, Autopilot, a connector's harness), the context estimate
  counts them, and a model without tools keeps them. They add about 500
  tokens to every request, in the prefix providers cache.
- With decision 3 as recommended:
  - The clarifying line goes. In its place, how to ask: with `ask_user`
    when it's offered, all the questions in one call; otherwise in the
    reply, and stop there.
  - The `todo` rule adds that its checklist is principle 4's brief plan,
    each step with how it will be checked.
  - Autopilot's prompt swaps principle 1's lines that ask or forbid
    assuming ("Don't assume.", "If uncertain, ask.", "don't pick
    silently", "Name what's confusing. Ask.") for deciding and saying what
    was assumed, as it swaps the clarifying line today.
- Subagents keep their own prompts: they can't ask, and their kinds'
  prompts set their work. The writing kinds (worker, test writer) could
  take principles 2 and 3 later.
- Tests:
  - `the_system_prompt_holds_the_four_principles`: in Agent and Plan mode,
    with tools and without.
  - `autopilots_system_prompt_decides_instead_of_asking`
    (`rovik-core/src/agent.rs:3166`) checks today that Autopilot's prompt
    differs from the plain one by one line. It's rewritten to "by the
    asking lines only", with no line left in Autopilot's prompt that tells
    the model to ask.

### Done when

The agent can ask in Agent mode as in Plan mode, and Autopilot and
subagents can't; the prompt holds the four principles word for word, with
no line left that contradicts them.

---

## Phase 1: a tool call carries its outcome

### Today

A call's result is a `ToolResult` (`tools.rs:597`) whose
`content` is the text the model reads. Whether the call failed, and how a
command ended, are read back from that text:

| Where                                                                                                                                                                                             | Reads               | How                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------- | ------------------------------------ |
| `tools.rs:619` `is_error`, used by `record` (`execute.rs:929`, `:935`, `:953`, `:1004`), the context tools (`execute.rs:573`), `error_class` (`tools.rs:629`) and `result_meta` (`tools.rs:2000`) | failed              | `trim_start().starts_with("Error:")` |
| `tools.rs:2027`, inside `result_meta`: unreachable after its check at `:2000`                                                                                                                     | failed              | `trim_start().starts_with("Error:")` |
| `guard.rs:203` `detect`: the error streak and "the same error again"                                                                                                                              | failed              | `starts_with("Error:")`              |
| `execute.rs:641`: a finished checklist step in CLM mode                                                                                                                                           | failed              | `starts_with("Error")`               |
| `execute.rs:1580`: a harness's call of Rovik's `mcp` tool                                                                                                                                         | failed              | `starts_with("Error")`               |
| `chat_db/history.rs:571`: which old results are shortened                                                                                                                                         | failed              | `starts_with("Error:")`              |
| `subagent_run.rs:1909`: the files a resumed worker wrote                                                                                                                                          | failed              | `starts_with("Error")`               |
| `src/lib/toolArgs.ts:85` `isErrorResult`, through `callStatus` (`:97`), `toolResultMeta` (`ToolCallBlock.tsx:150`) and `src/lib/connectors.ts:681`                                                | failed              | `/^Error:/` after `trimStart()`      |
| `src/components/ToolCallBlock.tsx:1027`                                                                                                                                                           | failed              | `startsWith('Error:')`               |
| `tools.rs:763` `command_end`, for the agent log's `tool` line (`run.rs:3229`)                                                                                                                     | exit code, time-out | the result's second line             |
| `tools.rs:2080` `command_meta`, for the stored `meta.exitCode`, `background` and `waited`                                                                                                         | the same, the wait  | the second and third lines           |
| `ToolCallBlock.tsx:200` `commandMeta`, for rows stored without meta                                                                                                                               | the same            | the second and third lines           |

The text puts the command first (`format_command_output`, `tools.rs:1757`:
`$ {command}`, then the status), so a command of more than one line pushes
the status down.

Confirmed by throwaway tests:

- A `read` of a file holding `Error: disk full` is logged as a failed call
  (class "other"), stored without its line count, and shown as failed. The
  loop guard counts it too: reads of five files whose text starts with
  `Error:` draw "the last 5 tool calls all failed" (`guard.rs:257–262`, the
  limit at `:46`). By the code, three that return the same text draw "the
  same error came back 3 times" (`:265–286`), and the same note a second
  time pauses the run (`:153`).
- `bash` with `echo a` and `echo b` on two lines exits 0, and is stored with
  `exitCode: null`, which the tool block shows as "killed"; its log line has
  no exit code.
- Found on the way: a `todo` call the IDE refused (two steps in progress) is
  still "the current checklist" (`plan.rs:2271` reads the call, never its
  result), so the panel shows the list the model was told was wrong.

The typed facts exist where the text is made and are lost there: `run`
returns `Err(String)` for most failures, turned into text at
`tools.rs:990–996`; `CommandOutput` (`rovik-core/src/command_tool.rs:38`) has
`exit_code`, `timed_out` and `stopped`; MCP replies carry `isError`
(`mcp_tool.rs:539`); a harness's steps carry `ok` (`connectors/mod.rs:101`).

### Design

- `ToolResult` gains `failed: bool` and `command: Option<CommandEnd>`.
  `ToolResult::ok(text)` and `ToolResult::error(text)` replace
  `ToolResult::text`; `error` adds `Error: ` when the text doesn't start
  with it. The `Err` arms of `execute` (`tools.rs:994–995`) build `error`.
- `CommandEnd` holds the exit code (none when the command didn't exit on
  its own), whether it timed out or was stopped, the id it runs under in
  the background, and a long wait for a CPU slot. It sits beside
  `CommandOutput`, built from it and from the background tool's `Report`
  (`command_tool/background.rs:87`).
- The text the model reads doesn't change by a byte. A failed call's text
  still starts with `Error:`: the model has always seen failures so, and an
  older app reading a newer daemon's rows still decides by it. A
  `debug_assert!` in `add_tool_result` keeps it so.
- Everything after the call reads the fields:
  - `record` (`execute.rs:910`): the approval's note, CLM's recall hint, the
    write's bookkeeping and the guard.
  - The guard: `FinishedCall` (`guard.rs:54`) gains `failed`, and `detect`
    stops reading the text for it.
  - `log_tool` (`run.rs:3213`): `failed` from the field; the `error` class
    still from the wording (`error_class`), for failed calls only;
    `exitCode` and `timedOut` from `CommandEnd`. The line's fields stay as
    they are: rovik-headless counts them (`rovik-headless/src/lib.rs:828–843`).
  - `result_meta` (`tools.rs:1999`) is called for successes only, so a
    `read` of a file starting with `Error:` gets its line count.
  - A parallel batch's join error (`execute.rs:380`) and the reply to a
    harness's `mcp` call (`execute.rs:1580`).
- `add_tool_result` (`run.rs:3188`) takes the outcome. Its 26 callers and
  `answer_unrun`'s 9, in `execute.rs`, `run.rs` and `subagent_run.rs`,
  each say it, and the compiler lists them. `answer_unrun` keeps `refused`
  for the guard and takes the outcome apart from it: `end_over_budget`
  (`run.rs:702`) answers "not run" as a failure that isn't a refusal.
- Helpers that return a success or a failure as one `String` return a typed
  value: `context_tool` (`execute.rs:1117`), `answer_review`
  (`change_review.rs:1535`), `describe_todo` (`plan.rs:2214`) and
  `describe_outcome` (`plan.rs:2104`). The refusal builders
  (`not_a_test_file`, `outside_scope`, `restricted_files::refusal` and the
  like, about twenty) always describe a failure, stay `String`, and go through
  `ToolResult::error` or `answer_unrun`.
- Stored: the tool message's `meta` gets `"outcome": "done"` or
  `"failed"`, and the command's keys (`exitCode`, `background`, `waited`)
  come from `CommandEnd`, with the meaning the frontend gives them today.
  Not the row's `failed` or `error` columns: a row with either is never
  sent to the model (`chat_db/history.rs:1315`), which would drop the
  result and leave its call unanswered in the next request.
- One reader per side for stored rows: `MessageRow::call_failed()` in Rust
  and `callFailed(result)` in TypeScript. Each takes `meta.outcome` when
  it's there, else the old prefix test (rows from before this change, and
  rows a daemon of an older version mirrors). `subagent_run.rs:1909`,
  `current_checklist`, `callStatus` and `ToolCallBlock.tsx:1027` use them.
  `chat_db/history.rs:571` reads a `Msg` (`:78`), which has no meta: the
  outcome is copied into it where rows become `Msg`s (`:1313`).
- The chat's view of a harness's steps (`connectors.ts:681`) sets
  `meta.outcome` from `step.ok`, instead of writing `Error: ` into the text.

### Steps

**1.1 Tool calls say whether they failed** (Rust only; the chat still
reads the text until 1.3)

- `ToolResult::ok`/`error` and `failed`; the producers in `tools.rs` (the
  21 `ToolResult::text` calls that carry a failure, and the `Err` arms);
  the typed helpers above; `add_tool_result` and `answer_unrun` take the
  outcome; `record`, the guard and `log_tool` read it; `meta.outcome` is
  written; `MessageRow::call_failed` serves `subagent_run.rs:1909`, and
  through `Msg`, `chat_db/history.rs:571`. The unreachable check at
  `tools.rs:2027` goes.
- Tests:
  - `a_read_of_a_file_that_starts_with_error_is_not_a_failed_call` (e2e,
    `e2e_tests.rs`): its `tool` line says `failed: false`, its row says
    `outcome: "done"` and has its line count.
  - `successes_whose_text_starts_with_error_make_no_error_streak` (`guard.rs`).
  - `a_failed_call_says_error_once` (`tools.rs`): `error` adds the prefix
    to text without it and leaves text with it alone.
  - `a_stored_result_without_an_outcome_is_judged_by_its_text`
    (`chat_db/tests.rs`).
  - `every_finished_tool_call_leaves_a_tool_line` (`e2e_tests.rs:6408`)
    passes unchanged.

**1.2 A command's end travels with its result**

- `CommandEnd`, built where `bash` and `background` format their text
  (`tools.rs:1351`, `:1326`, `:1384–1390`); `meta` and the log line take it
  from there. `command_end`, `command_meta` and `EXIT_CODE` go: nothing
  reads them any more.
- Tests:
  - `a_command_of_two_lines_reports_how_it_ended` (`tools.rs`): exit code,
    time-out, stop and a signal, each for a one-line and a two-line
    command.
  - `a_two_line_commands_tool_line_has_its_exit_code` (e2e).
  - `a_background_command_of_two_lines_reports_its_id` (`tools.rs`).

**1.3 The chat reads a call's outcome from its meta**

- `callFailed` in `src/lib/toolArgs.ts`, used by `callStatus`,
  `toolResultMeta` and `ToolCallBlock.tsx:1027`; `isErrorResult` and
  `commandMeta` stay only as the fallback for rows without `outcome`, named
  so. `connectors.ts:681` sets the outcome from `step.ok`. `ToolMeta`
  (`src/types.ts:228`) gains `outcome`.
- Tests:
  - `toolArgs.test.ts`: "a result that starts with Error: but succeeded
    is done", and "a result stored before outcomes is judged by its text".
  - `backgroundToolBlocks.test.tsx`: "a two-line command shows its exit
    code".
  - `connectors.test.ts`: "a harness step that failed shows as failed and
    keeps its text".

**1.4 A checklist the IDE refused isn't the current one**

- Two places say what the checklist is. While a run goes, the panel shows
  the `AgentEvent::Checklist` that every `todo` call emits, refused or not
  (`execute.rs:714`); the run emits it for an accepted call only. When the
  chat is idle, the panel asks `current_checklist` (`plan.rs:2317`, through
  `mod.rs:2047`), which skips a `todo` call whose result failed (by
  `call_failed`, which also covers rows from before 1.1: the refusal starts
  with `Error:`).
- Tests: `a_todo_call_the_ide_refused_isnt_the_current_checklist` (the list
  before it stays current) and `a_refused_todo_call_sends_no_checklist`
  (e2e).

### Done when

Whether a call failed and how its command ended travel as fields from the
tool to the guard, the log, the database and the chat. Only `call_failed`
and `callFailed` (for rows stored before) and `error_class` (the class of
a failure's wording) still read a result's text, and `ToolResult::error`
its own, to add the prefix once. The stored meta keys are the ones the
frontend reads today plus `outcome`; the protocol types don't change
(`meta` is JSON).

### Not in this phase

- Error classes stay derived from the wording (`tools.rs:651`), now only
  for calls known to have failed. They're the evaluation's statistics,
  pinned by tests; moving them into the over 160 places that word an
  error would change nothing anyone sees.
- The header counts (`result_meta` for `read`, `search_code`, `list_dir`,
  `neighbours`, `callers`, `outline`, `diagnostics`, `write`, `web_search`
  and `browse_url`) are still read from text the tool wrote itself, and no
  wrong count was found. If one turns up, the move is
  the same: the tool sets `ToolResult.meta`.

---

## Phase 2: one table of built-in tools

### Today

There are 21 built-in tools: `read`, `write`, `list_dir`, `search_code`,
`neighbours`, `callers`, `outline`, `diagnostics`, `web_search`,
`browse_url`, `browser_screenshot`, `bash`, `background`, `ask_user`,
`todo`, `submit_plan`, `restore_context`, `task`, `extend_scope`,
`answer_review` and `mcp`. Their schemas are built in `tools.rs` (`:30`
on), `subagent.rs:1088`, `worker.rs:201`, `change_review.rs:812` and
`mcp_tool.rs:35`. They're dispatched in three places: the lead's `task`
calls in `execute_tool_calls` (`execute.rs:225–235`, to `subagent_run.rs:275`),
the run's own tools and the context folder's in `execute_one`
(`execute.rs:448`, `:563–567`, `:1117`), and the rest in `tools::run`,
where an unknown name ends in "unknown tool" (`tools.rs:1533`).

What each tool may do is decided by name, in more than 40 places. The ones
that are policy:

| Capability                        | Where                                                                                  | A tool missing from it gets                    |
| --------------------------------- | -------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Which approval rule applies       | `permissions.rs:1622` `check_tool_call`                                                | the tools rule, not the edits or commands rule |
| Changes files                     | `permissions.rs:1565` `modifies_files` (`write` only)                                  | treated as a read: no edit review, not tracked |
| Allowed hosts                     | `permissions.rs:1656` `host_refusal`                                                   | no host check                                  |
| Restricted files                  | `restricted.rs:248` `problem_read`                                                     | no check                                       |
| The test writer's hidden files    | `tools.rs:1058`, `:1461`, `:1537`                                                      | nothing hidden                                 |
| Plan mode                         | `plan.rs:23` `PLAN_BLOCKED_TOOLS`                                                      | allowed in Plan mode                           |
| Counts as work for the checklist  | `plan.rs:2383` `checklist_gate_reason`                                                 | not counted                                    |
| Runs in parallel; read-only hint  | `parallel.rs:20` `PARALLEL_TOOLS`; `connectors/mcp.rs:69` `READ_ONLY` (the same seven) | one at a time, no hint (safe)                  |
| A subagent kind's tools           | 11 `KindSpec.tools` lists; `Kind::writes` kept by hand (`subagent.rs:180`)             | refused (safe)                                 |
| Works on the context folder (CLM) | `tools.rs:489`, `:511`; `execute.rs:566`                                               | offered there, but not routed there            |
| Needs an open project             | the order of the arms in `tools::run` (web tools before the check at `tools.rs:1187`)  | —                                              |

Five defaults fail open: the approval rule, "changes files", allowed hosts,
restricted files and Plan mode (and the test writer's hidden files). None
is reachable today: every tool is listed where it has to be. The risk is
the next tool. Most visible to a user: with edits set to need approval and
other tools not, a new tool that writes files would write unasked.

On the way, three inconsistencies, each checked against the code:

- In CLM mode without a project, all eight file tools are offered for
  `@context/` (`context_tool_schemas`, `tools.rs:489`, offers every file
  tool not otherwise offered), but only `read`, `write` and `list_dir` are
  routed there (`execute.rs:566`): the other five answer "needs an open
  project folder".
- `PARALLEL_TOOLS` and `READ_ONLY` are one list kept twice.
- `todo` and `submit_plan` run in either mode: the mode decides only
  whether they're offered.

### Design

A new `agent_run/builtin_tools.rs`:

```rust
/// A tool Rovik serves to the model, by its name on the wire. An MCP
/// server's tools go through `Mcp`; a harness's own tools aren't these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTool { Read, Write, ListDir, /* … 21 in all */ Mcp }

/// What a tool is and may do, whatever the run.
pub struct ToolSpec {
    pub name: &'static str,
    pub effect: Effect,         // Reads, Writes, Runs, Web, Control, Mcp
    pub dispatch: Dispatch,     // Subagents (task), Run (its own tools), Tools
    pub modes: Modes,           // Any, PlanOnly, AgentOnly
    pub needs_project: bool,
    pub parallel: bool,         // a local read: may run beside others
    pub context_folder: bool,   // also works on @context/ (CLM)
    pub restricted: Restricted, // path checked (read or write), command parsed, results filtered
    pub hidden: Hidden,         // the test writer: refused by path, filtered from the output
    pub host: HostCheck,        // None, SearchEngine, UrlArgument
}

impl BuiltinTool {
    pub const ALL: [BuiltinTool; 21] = [/* … */];
    /// One exhaustive `match`: a new variant doesn't build without its row.
    pub fn spec(self) -> ToolSpec { /* … */ }
    pub fn parse(name: &str) -> Option<BuiltinTool> { /* … */ }
}
```

- `Restricted` and `Hidden` are sets, not one choice: `neighbours` is
  refused by path and has its results filtered, for both. (`HostCheck`,
  not `Host`: `services::Host` and `prompt_cache::Host` exist.)
- After phase 0, `ask_user`'s row takes both modes; Autopilot still takes
  it out, in code, since that depends on the run.
- Names stay strings on the wire and in the database (`ToolCall.name`,
  the agent log, the headless runner's policy file): `parse` reads them,
  and a name that isn't built in goes the way it goes now (`mcp__…` names
  from a harness, "unknown tool").
- What depends on the run stays code: whether a tool is offered (project,
  vision, a model without tools, `commands_available`, allowed hosts, CLM,
  Autopilot, lead or kind, the MCP catalog, a write scope, a held review),
  schema text, approval decisions from the settings, refusals that depend
  on arguments (the command's text, a partial write, scope, test paths,
  `@checks/` and `@context/` paths), and which kinds get which tools
  (per-kind data, now `&[BuiltinTool]`).
- A harness's tool names (`Bash`, `Edit`, `apply_patch`, `WebFetch`) keep
  their mapping onto built-in names (`connectors/claude_code.rs:1707`,
  `:2141`, `connectors/codex/hooks.rs:291`) and `host_refusal`'s
  `WebFetch` arm: they're the harness's names, not Rovik's.

### Steps

**2.1 The table, and dispatch by it**

- `builtin_tools.rs`. Which of the three dispatchers takes a tool is its
  row's `dispatch`, so a new tool can't go without one; each dispatcher
  matches on `BuiltinTool` for its own. The "needs a project" check reads
  `needs_project` instead of the order of the arms.
- Tests:
  - `every_builtin_tool_has_one_schema_and_parses_back_from_its_name`:
    the names of all schema builders' output equal `ALL`'s.
  - `every_builtin_tool_reaches_its_dispatcher`: each tool, called through
    a run with the least arguments it takes, gets an answer of its own,
    never "unknown tool".
  - `only_local_reads_run_in_parallel`.

**2.2 Permissions by the table**

- `modifies_files`, `check_tool_call` and `host_refusal`
  (`permissions.rs`), `restricted::problem_read` and the three hidden-file
  sites read the spec. Behaviour is unchanged for all 21 tools.
- Tests:
  - `a_tool_that_writes_files_asks_for_edit_approval`.
  - `every_tool_with_a_path_argument_has_a_restricted_files_rule`: from
    each schema's `path` property. `list_dir`, which shows names and no
    contents, is the one exception, named in the test, so a new tool must
    get a rule or be named there with its reason.
  - `every_tool_with_a_url_argument_is_checked_against_the_allowed_hosts`:
    from each schema's `url` property.
  - `permissions.rs:2393` `tool_calls_go_by_their_group_rule` passes
    unchanged.

**2.3 The lists by the table**

- `PARALLEL_TOOLS` and `READ_ONLY` become `spec.parallel`;
  `PLAN_BLOCKED_TOOLS` becomes `spec.modes`; the checklist gate reads
  `effect`; `context_tool_schemas` and `context_only_tool_schemas` read
  `context_folder`. If decision 4 says so, dispatch refuses a tool the
  mode doesn't offer, as it refuses `write` in Plan mode now.
- Tests:
  - `plan_mode_takes_no_tool_that_writes_files_or_runs_commands`.
  - `without_a_project_the_context_folder_offers_only_the_tools_it_serves`
    (the fix).
  - With decision 4: `a_tool_the_mode_doesnt_offer_is_refused_when_called`
    (`todo` in Plan mode, `submit_plan` in Agent mode).
  - `tool_lists_by_mode` (`tools.rs:2310`) passes unchanged.

**2.4 Kinds by the table**

- `KindSpec.tools` becomes `&[BuiltinTool]`, with `BuiltinTool::READS` for
  the seven read tools most kinds share; `Kind::writes` is derived from
  its tools.
- Test: `a_kind_that_doesnt_write_gets_no_tool_that_writes_or_runs_commands`,
  over `Kind::ALL`, beside the single-kind checks it generalises
  (`the_planner_reads_and_never_writes`, `the_judge_reads_and_never_writes`).

### Done when

Adding a tool means a variant, its row and its schema; a missing row
doesn't build, and a missing dispatch arm or policy fails a test. The string
lists (`PARALLEL_TOOLS`, `READ_ONLY`, `PLAN_BLOCKED_TOOLS` and the kinds'
lists) are gone.

### Not in this phase

- How old results are shortened (`chat_db/history.rs:519` `replace_outdated`) and
  how a repeat is recognised (`clm/mod.rs:967` `same_call`) are per tool
  too, but decide how much context old results take, not what a tool may
  do. They can join the table the same way later.
- The frontend's copies of what tools are called and how they read
  (`ToolCallBlock.tsx` `describe`, `src/lib/permissions.ts:727`,
  `src/lib/toolGroups.ts`).

---

## Phase 3: `plan.rs` split, and a plan parsed once

### Today

`agent_run/plan.rs` is 4,394 lines: 2,484 of code and 1,910 of tests.
The code is ten concerns:

| Lines     | Concern                                                                                                         | Used from                                                                                                |
| --------- | --------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| 20–120    | Plan mode's prompt text, its blocked tools                                                                      | `tools.rs`, `execute.rs`, `run.rs`                                                                       |
| 121–331   | Criteria (S01, D8)                                                                                              | Autopilot, acceptance, plan review                                                                       |
| 332–622   | Checks of a plan's form (`plan_problems`)                                                                       | `submit_plan`                                                                                            |
| 623–858   | The plan's file; the request that implements it                                                                 | `execute.rs`, `planning.rs`, `run.rs`                                                                    |
| 859–1219  | Steps and their files (`steps`, `step_files`, `file_path`)                                                      | groups, planning, the test writer, repair                                                                |
| 1220–1397 | Validation commands (V02)                                                                                       | `Run::prepare` (`run.rs:1072`)                                                                           |
| 1398–1726 | Findings left in a plan; checks against the project's files (B07)                                               | `submit_plan`, `planning.rs`, `run.rs`, `group.rs`                                                       |
| 1727–2022 | Mechanical format fixes (`normalize`, `skeleton`)                                                               | `submit_plan`                                                                                            |
| 2023–2146 | `ask_user` questions                                                                                            | `execute.rs`                                                                                             |
| 2147–2484 | The checklist (`todo`), rebuilt from history, and its gate; what a stored message is (`is_request`, `is_steer`) | `run.rs`, `execute.rs`, `mod.rs`, `subagent_run.rs`, `change_review.rs`, `chat_db/history.rs`, Autopilot |

Eleven functions start again from the raw Markdown and redo the same
work (line endings, fences, `##` headings, where a section ends):
`criteria_section` (`:173`), `with_criteria` (`:271`), `plan_problems`
(`:493`), `save_plan_file` (`:672`), `steps` (`:1150`), `section`
(`:1195`), `without_findings` (`:1404`), `project_problems` (`:1449`,
through `steps`), `files_lines` (`:1736`), `normalize` (`:1810`) and
`skeleton` (`:1943`).

`plan_problems` and `steps` run the same pipeline (the steps heading, its
section, the split into steps, their numbers, each `Files:` line), with
two differences: `plan_problems` first removes code blocks with a regex
(`CODE_BLOCK`, backtick fences only) where `steps` keeps them in the body,
and `steps` trims each step's start. No plan was found on which they
disagree, but nothing keeps them from it. `submit_plan`
(`execute.rs:2069`) parses one plan up to five times in a row:
`without_findings`; `normalize` in Balanced Autopilot; then
`plan_problems_for`, which runs `plan_problems`, `project_problems`
(through `steps`) and, in Autopilot, `criteria_problems`.

### Design

`agent_run/plan.rs` becomes the folder `agent_run/plan/`:

- `doc.rs`: `PlanDoc`, the plan parsed once. It holds the text with `\n`
  line endings, each line's offset and whether it's inside a fence, the
  `# ` title line, and the `##` sections (heading, title, body), all as
  ranges into the text as written: nothing is reformatted.
- `steps.rs`: `PlanDoc::raw_steps()` (each step's range, number, number as
  written, `StepFiles` and group), `PlanStep` and `steps` built from them,
  and `file_path`/`BadPath`.
- `check.rs`: `plan_problems`, `criteria_problems`, `project_problems` and
  `plan_problems_for`, all on `&PlanDoc`. The "too vague" test reads a
  step's lines outside fences (today: the step with `CODE_BLOCK` removed).
- `criteria.rs`, `validation.rs` (V02), `format.rs` (`normalize`,
  `skeleton`, `without_findings`), `file.rs` (saving, paths, the
  implementation request) and `prompt.rs`.
- `mod.rs` re-exports what callers use, so `plan::steps` and the rest
  keep their paths.
- Out of `plan` altogether, since they aren't plans: `agent_run/questions.rs`
  (`ask_user`), `agent_run/checklist.rs` (`todo`, `current_checklist`, the
  gate and its notes), and `agent_run/messages.rs` for what a stored
  message is (`is_request`, `is_steer`, `is_harness`,
  `is_checklist_dismissed`). Their callers' paths change.

The tolerant parsing stays exactly as it is: headings like `## 3. Steps`
and `## Next steps`, `1. 1. 1.` numbering, steps as headings or bold
lines, the `Files:` line's spellings. The 1,910 lines of existing tests are
its specification and move with their code unchanged. `PlanStep.body`
stays the step as written; `normalize` and `skeleton` give the same text.

### Steps

**3.1 Questions, the checklist and message kinds move out** (moves only;
`git diff --color-moved` shows nothing else). Tests move with their code.

**3.2 The rest of `plan.rs` goes into `plan/` by concern** (moves only).

**3.3 `PlanDoc`, and `steps` and `plan_problems` on it**

- Test: `checks_and_steps_read_the_same_steps_from_every_plan`: for every
  plan in the tests' samples, gathered in one list, the steps the checks
  name are `steps()`'s, by number and title.
- While porting, each function is compared with its old version over all
  the samples; the old version is deleted when its port lands.

**3.4 The other nine functions on `PlanDoc`**, and `submit_plan` parses
once (and once more after `normalize` changed the text).

### Done when

Only `PlanDoc::parse` turns Markdown into structure; no other function in
`plan/` normalises line endings or looks for fences and headings itself.
`plan/` holds plans only.

---

## Phase 4: `Run`'s state in objects of its own

### Today

`Run` (`run.rs:132–451`) has 138 fields, 135 of them `pub(super)`, and
`impl Run` blocks in 23 files. A scan of which file reads and writes each
field (outside tests) groups them so:

| State                   | Fields | Which                                                                                                                                                | Touched in                                                                                                                                                                                                     |
| ----------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Context pressure (CLM)  | 20     | `clm_*`, `tier75_*`, `urgent_*`, `step_*`, `pending_step_compaction`, `restricted_turns`, `restricted_stall`, `context_edited`, `edit_delta_pending` | `run.rs`, `execute.rs`                                                                                                                                                                                         |
| Repository map          | 4      | `repo_map`, `repo_map_chars`, `map_refreshed_at`, `map_refreshes`                                                                                    | `run.rs`, `subagent_run.rs`                                                                                                                                                                                    |
| Checklist               | 4      | `checklist_nudges`, `force_todo_next`, `checklist_gate_refused`, `open_steps_at_end`                                                                 | `run.rs`, `execute.rs`                                                                                                                                                                                         |
| Steering                | 4      | `steer`, `harness_waits`, `steers`, `steers_returned`                                                                                                | `run.rs`, `execute.rs`, `mod.rs`                                                                                                                                                                               |
| The plan and its review | 17     | `plan_*`, `questions_asked`, `reply_submits`                                                                                                         | `plan_review.rs`, `execute.rs`, `run.rs`                                                                                                                                                                       |
| The change review       | 14     | `review_*`, `reviews_*`, `writes_since_review`, `last_guidance`, `change_review_progress`                                                            | `change_review.rs`, `verify.rs`, `guide.rs`, `execute.rs`, `subagent_run.rs`, `run.rs`                                                                                                                         |
| Tests and acceptance    | 10     | `test_writer_files`, `tests_changed*`, `tests_after_change`, `breaker_*`, `acceptance_*`                                                             | `test_writer.rs`, `breaker.rs`, `acceptance.rs`, `verify.rs`, `repair.rs`, `repair_run.rs`, `subagent_run.rs`, `run.rs`                                                                                        |
| Verification and repair | 24     | `verify_*`, `last_verify_*`, `writes_since_verify`, `checkpoint`, `baseline`, `integrity_*`, `carry_lost*`, `round_commands`, `rechecks`, …          | `verify.rs`, `repair.rs`, `repair_run.rs`, `prepare.rs`, `triage.rs`, `landing.rs`, `group.rs`, `subagent_run.rs`, `execute.rs`, `acceptance.rs`, `breaker.rs`, `test_writer.rs`, `change_review.rs`, `run.rs` |
| Landing                 | 3      | `landed`, `merged_from_project`, `marker_reminders`                                                                                                  | `landing.rs`, `run.rs`, `verify.rs`, `before.rs`                                                                                                                                                               |
| The run itself          | 38     | identity and setup, signals, the turn loop's own flags, the run's output (`changed_files`, `ledger`)                                                 | everywhere                                                                                                                                                                                                     |

What that shape costs, concretely:

- `VerifyCarry` (`verify.rs:144`), the fix rounds' state while the checks
  wait for approval, repeats 22 of these fields. `take_verify_state`
  (`:661`) and `restore_verify_state` (`:695`) copy them one by one, and
  `VerifyCarry::for_test` (`:185`) lists them again. A field added to the
  fix rounds and missed in one of the four places is lost, without a word,
  whenever the checks wait for approval.
- `writes_since_verify` is set in nine files, for three different things:
  the lead's own changes (`execute.rs:1078`, `:1098`); changes made by
  others (a worker's writes, `subagent_run.rs:1216` and `:1932`; the test
  writer's files, `test_writer.rs:805`; the breaker's, `breaker.rs:428`;
  the project's changes merged in, `landing.rs:439` and `run.rs:1698`; a
  rollback, `verify.rs:1103`); and checks run again with nothing changed
  (`acceptance.rs:428`, `prepare.rs:449`, `verify.rs:1567`). `verify.rs`
  clears or recomputes it in four more places.
- `update_clm_pressure` (`run.rs:2874`, 135 lines) is a state machine over
  20 fields, mixed with a database read and log writes: it can only be
  tested through a whole run.
- `Run::blank` (`run.rs:1268`) is a 140-line struct literal, which every
  new field makes longer.

Tests touch few of these fields directly: `restricted_e2e_tests.rs`
(`baseline` at `:606`, `changed_files` at `:631`, `seen` at `:638`) and
`subagent_e2e_tests.rs` (`:11200`, `:11419–11420`). The move doesn't churn
them.

### Design

The rules for every state object:

- One struct per concern, in the file of the code that drives it, with
  private fields and `Default`. What happens to it is a method named for
  the event (`edited`, `round_failed`, `rolled_back`, `recheck(Why)`);
  what other code needs to know is a method too, not a field.
- It decides; `Run` does the I/O around it: reads the database, writes the
  log, emits events.
- `Run` keeps one field per object and the orchestration: the turn loop,
  the order of the end-of-run checks, the run's identity and setup, its
  signals, and the turn loop's own flags.
- Behaviour doesn't change. Each object is one commit, with the whole e2e
  suite green.

The objects, in the order to extract them (the review's `ContextState`,
`ReviewState` and `VerificationState`, and the rest named the same way):

1. **`ContextState`** (a new `context_state.rs`): the 20 CLM fields.
   `Run::update_clm_pressure` reads the database and logs; the object
   decides: `turn(seen: ClmTurn) -> Option<Restriction>`, where `ClmTurn`
   is what the function reads today (the percentage, the tiers reminded,
   the revision, the tokens, the tool calls so far), and `edited()`,
   `step_compaction_asked(…)`, `recall_hit()` and `compliance()` (the
   `clm` log line's fields, `run.rs:3014`). First, because it's the most
   self-contained and gains the most tests.
2. **`SteerState`**, **`ChecklistState`** and **`RepoMapState`**: small and
   mechanical.
3. **`PlanState`** (`plan_review.rs`): the 17 plan fields.
4. **`ReviewState`** (`change_review.rs`): the 14 change review fields.
5. **`TestsState`**: the test writer's, the breaker's and acceptance's
   fields.
6. **`VerificationState`** (`verify.rs`), last, as the most shared:
   - The state that crosses an approval moves as a whole: `VerifyCarry`
     holds `FixRounds` (the verification part), `ReviewCarry` (the review
     totals and findings it carries today) and `TestsCarry` (the test
     writer's files), plus `seen` and `ledger` as now. Take and restore
     move these instead of copying 22 fields. Two cases stay as they
     are: spilled contents come back first (`verify.rs:703`), and the
     baseline is merged, not replaced (`:712`). What isn't carried today
     (`verify_rounds`, read back from the record at `run.rs:900`, the
     stall count, the review's held findings) stays out of the carry.
   - `writes_since_verify` becomes two events: `changes_made()`, from
     every place that changes files (`Run::note_write` and `note_command`,
     a worker's writes, the test writer's and the breaker's files, a
     merge, a rollback), and `recheck(Why)` for the three that ask for
     checks with nothing changed, with `Why` naming each.
7. **`LandingState`**: its three fields.

What stays on `Run` (38 fields): `core`, `dialog_id`, `profile`,
`options`, the modes and the stage, the roots, `checks` and the project's
context, `restricted`, `mcp`, `seen`, `role`, the signals (`stop`,
`pause`, `effort`), the turn loop's flags (`guard`, `calls`,
`words_only`, `reply_cut_off`, `refused_formats`, `repetition_stops`,
`loop_resumed`, `denied_by_user`, `settle_message`, `continue_reply`,
`resumed_since`) and the run's output (`changed_files`, `ledger`,
`group_grants`).

### Tests

- `ContextState` (unit, no database):
  `an_edit_lifts_the_restriction_and_counts_turns_to_edit`,
  `ninety_percent_with_no_edit_restricts_the_next_turn`,
  `an_ignored_step_compaction_restricts_after_four_calls`,
  `below_seventy_five_percent_the_restriction_lifts`,
  `the_safety_valve_lifts_the_restriction_for_good`.
- `every_part_of_a_fix_round_survives_waiting_for_approval`: a run with
  every carried part set takes its carry; a fresh run restores it; the
  parts compare equal (derive `PartialEq` where the fields allow,
  `Debug` otherwise).
- `a_recheck_runs_the_checks_even_with_no_file_changed`, for each `Why`.
- The e2e suites (`e2e_tests`, `repair_e2e_tests`, `autopilot_e2e_tests`,
  `subagent_e2e_tests`, `context_e2e_tests`, `steer_e2e_tests`) pass
  unchanged after each extraction: they're the behaviour's specification.

### Done when

`Run` has about 45 fields, one line each in `Run::blank`; no file writes
another concern's state except through its methods; `VerifyCarry` names no
field of the fix rounds.

---

## Phase 5: one decoder for a message row

### Today

- `chat_db/store.rs:688` `MESSAGE_COLUMNS` lists 14 columns;
  `message_from_row` (`:691`) reads them by position into `MessageRow`. The
  same constant is the column list of `insert_message`'s INSERT (`:857`).
- `chat_db/sync.rs:53` lists the same 14 with an `m.` prefix, then
  `b.content` and `m.rev`; its `message_from_row` (`:442`) reads them into
  `StoredMessage` (`rovik-protocol/src/sync.rs:36`). `read_message`
  (`:355`) reads `rev` at index 15 itself.
- Some differences are on purpose: `store` parses `tool_calls` and `meta`
  into JSON, and `sync` keeps them as text, since a mirror stores the
  owner's bytes and `apply_message` compares rows for equality
  (`chat_db/sync.rs:907`).
- The write side repeats the columns twice more (`insert_message`;
  `apply_message`, `chat_db/sync.rs:901–961`), and rovik-agentd's test helper
  `messages_in` (`rovik-agentd/tests/common/mod.rs:326`) has its own order.
- No test would catch a column added to one mapping and not the other:
  `sync_tests::dump` reads both databases through `sync`'s own reader.
- `store` drops malformed JSON in `tool_calls` or `meta` silently
  (`chat_db/store.rs:701`, `:703`).

### Design

A new `chat_db/message_columns.rs`:

```rust
/// The messages table's own columns, in the order every read selects them.
const COLUMNS: [&str; 14] = ["id", "dialog_id", /* … */ "content"];

/// `"id, dialog_id, …"`; with an alias (`"m.id, …"`) for a join.
pub(super) fn columns(alias: Option<&str>) -> String { /* … */ }

/// A message as stored: its JSON columns as their text.
pub(super) struct RawMessage { /* the 14 columns */ }

impl RawMessage {
    /// The columns it reads: a caller's own columns come after.
    pub(super) const COUNT: usize = COLUMNS.len();
    pub(super) fn read(r: &Row) -> rusqlite::Result<RawMessage> { /* … */ }
    /// For the app: JSON parsed; a malformed value is logged and left out.
    pub(super) fn into_row(self) -> MessageRow { /* … */ }
    /// For a mirror: the bytes as they are.
    pub(super) fn into_stored(self, body: Option<String>) -> StoredMessage { /* … */ }
}
```

Both readers become `RawMessage::read` plus their conversion; `sync`
reads `b.content` and `rev` at `COUNT` and `COUNT + 1`. `insert_message`
takes its column list from `columns(None)` as it does now. The behaviour
change is the log line for malformed JSON, which `AGENTS.md` asks for
("never swallow an error silently"); the row still loads.

### Tests

- `every_column_of_the_messages_table_is_read`: the table's columns
  (`PRAGMA table_info`), less `rev` (sync's caller reads it), are
  `COLUMNS`.
- `a_message_reads_the_same_through_the_store_and_the_mirror`: a message
  with every column set, read by `store::get_messages` and by
  `sync::read_message`, gives the same values (JSON compared parsed).

With every column in one list, `sync`'s reader can't miss one, so the
existing `pages_and_pulls_end_as_one_full_export_does`
(`chat_db/sync_tests.rs:296`) also catches a column `apply_message`
forgets to write, as long as its samples set that column.

### Not in this phase

The write side's SQL stays as it is. `insert_message` already takes its
column list from the shared constant, so a column added there without a
value fails every insert in the tests; `apply_message` keeps its own SQL,
covered as above.

---

## Phase 6: an owner for the process runtime (deferred)

### Today

- `Core::new` (`rovik-core/src/core.rs:74`) opens a `Core`, then sets up what the
  process keeps once: the workload's history, CPU slots and settings
  (`workload::init`), and the overlays' folder, with crash recovery
  (`overlay::chats::init`). `Core::shutdown` (`:105`) ends the connectors'
  live runs, the browser, MCP servers, background commands, the virtual
  files' mount, the overlays and the workload, then marks the database
  closed cleanly.
- That state lives in process-wide statics: `workload`'s (`HISTORY`,
  `SLOTS`, `SHARED`, the settings, and `SHUTTING_DOWN`, which never resets),
  `overlay::chats`' (`DATA_DIR`, `OPEN`), the two connectors' four
  `LiveRuns` (two each),
  `command_tool::background::ALL` and `rt::HANDLE`. Free functions read
  them from about 40 files (`overlay::chats`) and dozens of call sites
  (`background`, `workload`), across rovik-core, the app, rovik-agentd and
  rovik-client.
- The hosts set more outside `Core::new`: the app's `src-tauri/src/lib.rs`
  (`confine::require`, the login shell's PATH, the runtime,
  `os_lock::take_slot`, `chat_hold::init`, `net::init`), the daemon's
  `rovik-agentd/src/serve.rs` and `rovik-agentd/src/daemon.rs`
  (`confine::protect`, `network::enable`, `net::set_host_network`,
  `allowed_hosts::configure`).
- The headless runner builds its core with `Core::open` and never calls
  `shutdown` (`rovik-headless/src/lib.rs:504`).
- Tests of `Core::new` run in a process of their own: the test binary
  runs itself for one ignored test (`rovik-core/src/core.rs:128`;
  `rovik-core/src/testing.rs:421` `in_a_process_of_its_own`, and six more
  like it).
- It's on purpose: one `Core` per process is decision D2, and K18 the
  limit it accepts (`docs/design/cloud-agent-dev-plan.md:40`, `:120–122`,
  `:1278`, `:1393–1398`).

### Plan

Not scheduled. One core per process is a decision, and moving the statics
behind an owner touches about a hundred call sites in four crates for no
behaviour a user sees. Do it when `Core::new`, `shutdown` or a host's
start-up next changes, in this order:

1. A `ProcessRuntime` made at the host boundary does exactly what's done
   now: `workload::init` and `overlay::chats::init`, and the host's own
   setup, one list per host. It refuses a second start on another data
   folder (today that mixes the folders without a word). `Core::new` takes
   it; `ProcessRuntime::shutdown(core)` runs today's order, the app's
   terminals and instances around it as now.
2. Then one registry at a time moves into it and is passed where it's
   used, starting with what tests isolate by process (the overlays'
   folder, the workload's history), so those tests can share a process.

Tests then: `a_second_runtime_on_another_data_folder_is_refused`, and
`Core::new`'s test running in the shared process.

---

## Risks

- **A refactor changes behaviour.** Every commit keeps the e2e suites
  green; the moves in phases 3 and 4 are reviewed with `--color-moved`;
  phase 3 checks each ported function against its old version.
- **Mixed versions** (an app and a daemon of different versions share
  rows): phase 1 only adds a meta key; rows without it, and readers that
  don't know it, decide by the text as now.
- **Conflicts with work going on in `agent_run`.** Small commits, merged as
  they go; phase 4 last, one object at a time.
- **Tests that pin the model's text.** Phase 1 keeps it byte for byte.

## Noticed, not planned

- `chat_context_estimate` (`rovik-core/src/agent.rs:1870`) builds the offered tools apart
  from `model_turn`, without `mcp`, `answer_review`, `background` and
  `task`, so the context estimate can run low.
- `docs/design/cloud-agent-dev-plan.md:120–122` calls `TEST_DATA`
  test-only; it's also under the `testing` feature
  (`overlay/chats.rs:286–287`).
- `docs/design/subagents.md:100` lists the scout's tools without `mcp`.
- `connectors/mod.rs:874` has its own `is_request`, with another rule than
  `plan::is_request`: phase 3's `messages.rs` moves the latter and leaves
  the connectors' alone, until someone checks why they differ.

## Progress

- [x] 0.1 `ask_user` in Agent mode as in Plan mode
- [x] 0.2 The four principles in the system prompt
- [x] 1.1 Tool calls say whether they failed
- [x] 1.2 A command's end travels with its result
- [x] 1.3 The chat reads a call's outcome from its meta
- [x] 1.4 A checklist the IDE refused isn't the current one
- [ ] 2.1 The table, and dispatch by it
- [ ] 2.2 Permissions by the table
- [ ] 2.3 The lists by the table
- [ ] 2.4 Kinds by the table
- [ ] 3.1 Questions, the checklist and message kinds move out
- [ ] 3.2 `plan/` by concern
- [ ] 3.3 `PlanDoc`, with `steps` and `plan_problems`
- [ ] 3.4 The other nine functions on `PlanDoc`
- [ ] 4.1 `ContextState`
- [ ] 4.2 `SteerState`, `ChecklistState`, `RepoMapState`
- [ ] 4.3 `PlanState`
- [ ] 4.4 `ReviewState`
- [ ] 4.5 `TestsState`
- [ ] 4.6 `VerificationState`, with the carry
- [ ] 4.7 `LandingState`
- [ ] 5 `RawMessage`
- [ ] 6 `ProcessRuntime` (when the area next changes)
