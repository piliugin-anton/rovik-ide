# Make the change reviewer always on, and only with unlanded changes

The change reviewer (`Kind::Reviewer`, "Change reviewer" in Settings → Subagents) stops being an
opt-in helper. The rule "a kind the IDE starts itself ignores the switches" gets one home —
`Kind::always_on` plus a `SubagentSettings::may_run(kind)` the hooks ask instead of `kind(kind)` —
so the reviewer runs whether or not "Use subagents" and its own row's switch are on, exactly as
the test writer already does, while its row's model, fallbacks, effort and budget keep applying.
And `Run::review_change_as` starts a review only when the run has something unlanded to read:
`change_set()` non-empty (in an isolated chat the overlay's unlanded change; a chat that writes
straight into the project is reviewed too). Settings loses the reviewer's checkbox; the row keeps
its model picker, effort and budget, and the page says why.

## Steps

1. One home for the kinds that ignore the switches
   - Files: `src-tauri/crates/rovik-core/src/agent_run/subagent.rs`, `src-tauri/crates/rovik-core/src/agent_run/change_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/verify.rs`, `src-tauri/crates/rovik-core/src/agent_run/report.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/inside.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/tests.rs`
   - Add `Kind::always_on(self) -> bool` beside `lead_starts`: true for `Kind::Reviewer`; its doc
     says the IDE starts it itself, so neither switch on the Subagents page turns it off (the test
     writer is gated at its own hooks instead).
   - Add `SubagentSettings::may_run(&self, kind: Kind) -> bool` as a default method:
     `kind.always_on() || self.kind(kind).is_some()`. `SubagentConfig::kind`, `enabled_kinds` and
     `models` stay as they are, so the row's models and budget still come from
     `SubagentConfig::models`/`budget` (the chat's model and the kind's defaults when no row).
   - `Run::review_change_as` (change_review.rs:1194): ask `!config.may_run(Kind::Reviewer)`.
   - `Run::review_after_green` (verify.rs:1387): `if !config.may_run(super::subagent::Kind::Reviewer) || self.plan_mode` — R08's review once the checks pass after a fix is as mandatory as the one before the checks; its check record would otherwise go on saying "the reviewer didn't run".
   - `Run::reviewers_view` (report.rs:194): `if !config.may_run(Kind::Reviewer) { return None; }`.
   - `review_record` (autopilot/inside.rs:255): ask `config.subagents.may_run(kind)`, so only a
     kind that can be off says "Not reviewed: the <label> is off".
   - Tests: in `subagent.rs` `the_change_reviewer_runs_whatever_the_switches_say` (a default
     `SubagentConfig`, and one whose reviewer row is `enabled: false`, still `may_run` the
     reviewer; `may_run(Kind::Scout)` stays false, and the master switch still keeps an enabled
     scout off). In `autopilot/tests.rs` update `a_reviews_record_is_its_last_review_of_its_kind`:
     the change reviewer says "Not reviewed", the plan reviewer (a kind that can be off) says
     "Not reviewed: the plan reviewer is off".

2. Only when the run has unlanded files
   - Files: `src-tauri/crates/rovik-core/src/agent_run/change_review.rs`
   - In `review_change_as` (change_review.rs:1189), move the run's-budget early return
     (`!(writes_since_review || follow_up)`, `reviews_total >= cap`, `reviews_in_row >=
MAX_REVIEWS_IN_ROW`) above the `follow_up && … unanswered_review()` nudge at 1206 — nothing
     changes for the nudge, which already needs `follow_up` and `total < cap` — and put the new
     check before that nudge too: `let changes = self.change_set();`, and when it is empty,
     `self.review.change_gone()`, `self.review.finished("skipped".into(), Some("the run has no
unlanded changes".into()))` and `return Ok(false)`. A run whose change was put back is thus
     neither reviewed nor asked again for answers about it, and `render_diff` renders that same
     `changes`.
   - Add `ReviewState::change_gone(&mut self)`: clears `review_held`, `review_answers` and the
     carry's `review_findings` and `review_optional` — what a review held was about a change that
     isn't there any more, so it isn't followed up. It leaves `review_blocked` as it is: a review
     whose finding was still open at the run's last round wrote `review["blocked"] = true` into its
     row (change_review.rs:1401-1409, then `review_record` at 1415), and that row is what turns into
     the Review stage's `review-open` (autopilot/inside.rs:271-296, and the run's end at
     autopilot/driver.rs:492); clearing the flag in memory only would leave the run's record saying
     otherwise, so a change that is gone doesn't reopen a run a review already blocked.
   - Unit test beside `ReviewState`: `a_change_that_is_gone_holds_nothing_back` — a state with held
     findings and answers, after `change_gone()`: `has_held_findings()` false, `answer_review` says
     no findings are waiting, and a `review_blocked` it carried stays set.

3. e2e tests, and a stub reviewer for tests that aren't about it
   - Files: `src-tauri/crates/rovik-core/src/agent_run/e2e_tests.rs`, `src-tauri/crates/rovik-core/src/agent_run/subagent_e2e_tests.rs`
   - `Harness::start` (e2e_tests.rs:308): inside the existing `if request.mode.as_deref() !=
Some("autopilot")` block, beside the `testWriter` stub, when the config has no `"reviewer"`
     row, insert one on a stub model that answers `## Findings\n\nNone.`, so a test that isn't about
     the review doesn't spend its scripted replies on it. It stays out of Autopilot requests, whose
     tests route the reviewer's replies themselves (`reviewed_task_request`,
     autopilot_e2e_tests.rs:4565, sets no reviewer row and answers findings by the `REVIEW` marker),
     and tests that pass their own reviewer row (`reviewer_on`) are untouched.
   - Rename `small_changes_and_runs_with_the_reviewer_off_get_no_review` to
     `small_changes_get_no_review`, keeping the `review_min_lines: None` half and dropping the
     `off.enabled = false` half, which is no longer true.
   - New `the_reviewer_runs_with_the_subagents_off`: config with the master switch off and the
     reviewer row `enabled: false`, `reviewMinLines: 1`; the chat's model (the reviewer's fallback
     with no model of its own) scripts the write, "Done." and `NO_FINDINGS`; the run ends with one
     review record whose status is "clean".
   - New `a_change_that_is_gone_is_neither_reviewed_nor_asked_about`: the reviewer reports
     `FINDING_A` (a blocking finding on the line the change wrote); the agent does not answer it,
     writes `a.txt` back as it was and says "Done.", with `reviewer_on(json!({}))` and a routed
     mock whose reviewer would answer `NO_FINDINGS`. `record_order` then shows one `review:findings`
     and one `note` — no nudge for the unanswered finding, and no follow-up on an empty diff — and
     the run ends "unverified", not `review-open`. A follow-up is the case the min-lines check
     can't cover (it is skipped for one), so today this run nudges for the answer and makes a
     second review.
   - New `the_review_after_green_runs_with_the_subagents_off`, built on
     `review_after_green_fix_that_passes_ends_verified_with_round_4_of_4` (an npm/node check and
     `model_routed`): the config is `reviewer_on(json!({ "models": [profile("chat", &url)] }))`
     with the master switch and the row's `enabled` then set to `false`, and the check record after
     the fix says `reviewAfterGreen` `"findings"`.

4. Settings page: the change reviewer has no switch
   - Files: `src/components/SubagentSettings.tsx`, `src/lib/subagentSettings.ts`, `src/components/SubagentSettings.test.tsx`, `src/components/SettingsDialog.test.tsx`
   - `SubagentSettings.tsx`: `const noSwitch = kind.id === 'reviewer';` and
     `const alwaysOn = noSwitch || kind.id === 'testWriter';` (where `mandatory` was); the tag is
     "Always on" for both; the `<input type="checkbox">` renders only when `!noSwitch` (the test
     writer's stays checked and disabled); the "Code will also be sent to …" warning shows for an
     always-on kind; the master-switch sub-text and the Kinds hint say the change reviewer always
     runs, when the run has unlanded changes. Its model and effort already stay editable while
     subagents are off, since it is one of `AUTOPILOT_KINDS`.
   - `subagentSettings.ts`: the reviewer's `when` becomes "Always runs when the run has unlanded
     changes of at least the size set in Limits, at most 3 times a run."; the page's module comment
     says which kinds are always on; `export const ALWAYS_ON_KINDS: SubagentKindId[] =
['testWriter', 'reviewer']` is the list the tag and the summary read; `subagentSummary` names
     the change reviewer in both branches — "Change reviewer and test writer always on; optional
     helpers off" with subagents off, and the always-on kinds in its `On: …` list otherwise, which
     is never empty now, so `'On, but no kind is turned on'` goes.
   - The Limits the reviewer still uses stay set with subagents off: its per-kind "Tool calls" and
     "Minutes" fields (SubagentSettings.tsx:305-330) and "Review changes from (lines)" in
     `LIMIT_FIELDS` become `disabled={off() && kind.id !== 'reviewer'}` and
     `disabled={off() && field !== 'reviewMinLines'}`, and "Tokens per run" (SubagentSettings.tsx:285)
     loses its `disabled={off()}` — that cap is what stops a helper (`Run::new_sub`,
     subagent_run.rs:724, refuses one once `ledger.over_spend(config.max_run_tokens)` holds), so it
     governs the always-on reviewer's spending; every other kind's Limits stay disabled.
   - `SubagentSettings.test.tsx`: the tag list expects "Always on" for the reviewer; the reviewer's
     warning is "Code will also be sent to Beta."; its row has no checkbox while its model, effort
     and Limits fields ("Tokens per run" included) are enabled with "Use subagents" off (a scout's
     stay disabled); new test
     `the change reviewer has no switch and runs with optional helpers off`. In
     `subagentSettings.test.ts` (:69, :224) and `SettingsDialog.test.tsx` (:198) the summary's new
     words.

5. Docs
   - Files: `docs/design/subagents-plan.md`, `README.md`
   - `subagents-plan.md`: the Settings drawing and its table (the Change reviewer row has no switch
     and is always on; the "Use subagents" row: off skips the `task` tool and the triage hook, and
     the test writer and change reviewer still run), and notes B03 and B06 — the reviewer runs
     whatever the switches say, only when the run has unlanded changes, from `reviewMinLines`
     changed lines, and its row has model, fallbacks, effort and budget but no switch.
   - `README.md`: the subagents paragraph — the change reviewer always reads each finished change
     that hasn't landed, before the checks run.

## Decisions

- The user's answers: (1) the reviewer ignores both switches, like the test writer ("IDE-started
  helpers don't depend on opt-in switches"), with its row's model, fallbacks, effort and budget
  still applied and the row left editable when subagents are off; (2) "unlanded files" means
  `Run::change_set()` — the run's own diff, which in an isolated chat is exactly what hasn't
  landed, so a chat without an overlay (no virtual-files driver) is still reviewed.
- `Kind::always_on` + `may_run` rather than a special case in each hook: the gate in the review
  hook, the review after green, the reviewer's view in reports and the Autopilot stage record all
  ask the same question.
- `reviewMinLines` ("Review changes from (lines)", default 10) and the 3-reviews-per-run cap stay:
  the task was the switches and the unlanded gate, not the size limit.
- The gate is `change_set()` non-empty, so it can't see what that can't: a binary file, a file the
  run may not read (`change_set` at change_review.rs:1133-1146 skips a restricted file and a binary
  side), and a command's change in a chat that isn't isolated (`original_bytes_of`, repair.rs:1234,
  has no baseline for one and returns None). A run whose only change is one of those isn't
  reviewed, and its findings aren't followed up.
- `KindConfig.enabled` stays in the protocol (stored settings and the frontend payload keep it) and
  the core simply stops consulting it for the reviewer; nothing is migrated.
- A review with nothing unlanded leaves no record row, as the min-lines skip already does; when it
  is the after-green review, the check record's `reviewAfterGreen` says "skipped: the run has no
  unlanded changes".

## Validation

- `npm test`, `npm run typecheck`, `npm run format:check` (fix with `npx prettier --write <files>`).
- From `src-tauri/`: `cargo nextest run -p rovik-core` (or `cargo test -p rovik-core`);
  `cargo clippy -p rovik-core --tests` with no new warnings; `rustfmt --edition 2021` on every
  changed Rust file. The protocol crate is unchanged.
- Tests added or updated: `the_change_reviewer_runs_whatever_the_switches_say` (subagent.rs),
  `a_change_that_is_gone_holds_nothing_back` (change_review.rs),
  `a_reviews_record_is_its_last_review_of_its_kind` (autopilot/tests.rs),
  `small_changes_get_no_review`, `the_reviewer_runs_with_the_subagents_off`,
  `a_change_that_is_gone_is_neither_reviewed_nor_asked_about` and
  `the_review_after_green_runs_with_the_subagents_off` (subagent_e2e_tests.rs), the stub reviewer in
  `Harness::start` (e2e_tests.rs), in `SubagentSettings.test.tsx` the tag, warning, no-checkbox and
  Limits assertions plus `the change reviewer has no switch and runs with optional helpers off`, and
  the summary's words in `subagentSettings.test.ts` and `SettingsDialog.test.tsx`.
- By hand: Settings → Subagents shows the Change reviewer without a checkbox, tagged "Always on",
  with its model, fallbacks, effort and Limits still set while "Use subagents" is off; a chat whose
  run changes a few lines gets no review; a run of ten changed lines or more gets one review and its
  record; a run that puts its change back gets neither a follow-up review nor a nudge to answer the
  finding; with "Use subagents" off, a chat whose fix makes the checks pass still gets the review
  after green.

## Risks

- Every Agent-mode run with at least `reviewMinLines` changed lines now spends a review, on the
  reviewer's model or the chat's: more tokens and up to five minutes per run. The line limit, the
  three-reviews-per-run cap and the run's token cap are the knobs left.
- The unit tests of `rovik-core` run with subagents off and write into the project directly, so
  some now take the review path: the harness's stub reviewer keeps the ones that aren't about it
  from spending their scripted replies, and any test that still gets an unexpected review is fixed
  by scripting it, not by weakening an assertion.
- A follow-up review whose change is gone now settles its findings instead of reviewing an empty
  diff, so an agent that reverts its change gets out of a hold that isn't the run's last review.
  The change is really gone, so nothing lands unreviewed; a review that already blocked the run at
  its last round keeps it `review-open`, since only its record row can say otherwise.
- The phrase "Not reviewed: the change reviewer is off" disappears from review records; any stored
  record that has it is read as it was.
