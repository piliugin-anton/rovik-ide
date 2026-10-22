# Make Balanced Autopilot's plan stage finish on its own, within limits

Today the Balanced Autopilot plan stage blocks after 3 plan reviews (`AUTOPILOT_PLAN_REVIEW_ROUNDS`). Since commit a03f80e, every finding holds the plan, whatever its severity, and new findings count anywhere. A weak model rarely gets to zero in 3 reviews, and a run error, a repeating reply, or a plan written in the reply ends the stage with nobody there to press Retry. This plan keeps "every finding must be resolved" and keeps the final block. It replaces the fixed count with limits the user sets: rounds, rounds without progress, minutes, a share of the task's budget, and automatic retries. It makes reviews converge: a numbered finding list kept across reviews and automatic retries, a complete first review, an evidence rule for new findings, concrete fixes, and regression marks. It makes revisions small and hard to break: mechanical format fixes, and `edits` instead of a full resubmit. A finding the author and the reviewer keep disputing is settled once by a fresh plan reviewer run as an arbiter. The stage starts again by itself after a run failure. One author writes the plan throughout: no other model takes over the writing. Everything is limited to Balanced Autopilot: `in_autopilot() && !thorough()` in the run, `Effort::Balanced` in the driver. Plan mode and Thorough behave as today, with three exceptions: Step 3's prompt wording, which corrects text that already disagrees with the code in every mode; the optional `edits` argument (Step 7); and `submit_for_review`'s new optional arguments, which Thorough's callers leave at today's values (Step 9).

## Steps

1. **Plan-fix settings in the protocol and the client**
   - Files: `src-tauri/crates/rovik-protocol/src/autopilot.rs`, `src-tauri/crates/rovik-protocol/src/tests.rs`, `src-tauri/crates/rovik-client/src/remote.rs`, `src-tauri/crates/rovik-client/src/remote/tests.rs`, `docs/design/autopilot-plan.md`
   - **`PlanFix`:** add `pub struct PlanFix { pub max_rounds: u32, pub stall_rounds: u32, pub max_minutes: f64, pub budget_share: f64, pub max_retries: u32 }`.
     - Derives `Serialize, Deserialize, Clone, Copy, Debug, PartialEq`, with serde `camelCase` and `default`.
     - Write `Default` by hand, with 8, 2, 30, 0.4, 2.
     - Add `PlanFix::bounded(self) -> PlanFix` to clamp the values: rounds 1–20, stall 1–5, minutes 1–240, share 0.1–0.9, retries 0–5.
   - Add `pub plan_fix: PlanFix` to `AutopilotConfig` and its `Default`. Old settings load unchanged through serde defaults.
   - **Per-host override, field by field like `budget`:**
     - Add `pub struct PlanFixOverride { pub max_rounds: Option<u32>, pub stall_rounds: Option<u32>, pub max_minutes: Option<f64>, pub budget_share: Option<f64>, pub max_retries: Option<u32> }`. `None` means "as on this computer".
     - It derives `Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq`, with serde `camelCase`, `default`, and `skip_serializing_if = "Option::is_none"` on each field.
     - Add `#[serde(skip_serializing_if = "Option::is_none")] pub plan_fix: Option<PlanFixOverride>` to `AutonomySettings`, and list it in `AutonomySettings`'s hand-written `Debug` impl.
     - In `AutonomySettings::checked`, clamp each `Some` value to `PlanFix::bounded`'s ranges, and drop an override with no field set, as an empty budget is dropped.
   - **`ApprovalPolicy::apply`** (`remote.rs`): merge each field where the host sets one, like `budget` (`kept.max_rounds = host.max_rounds.unwrap_or(kept.max_rounds)`, and so on).
   - **Existing literals that list every field:**
     - add `plan_fix: PlanFix::default(),` to the `AutopilotConfig` literal in `config()` in `rovik-protocol/src/tests.rs`;
     - add the `"planFix": {…}` object to the shape asserted in `autopilot_settings_round_trip_and_default_when_missing`;
     - add `plan_fix: None,` to the `AutonomySettings` literals in `autonomy_settings_round_trip_and_default_to_keep_on_the_host` (`autopilot.rs`) and `a_host_that_works_on_its_own_gets_its_block` (`remote/tests.rs`).
     - The other `AutonomySettings` literals (`host_settings.rs`, `hosts_page.rs`, `a_host_that_pushes_is_told_the_projects_remote`) end in `..Default::default()` and need nothing.
   - **Tests:** round-trip and clamping tests next to those two protocol tests, and a remote test that a host's `planFix: { maxRounds: 5 }` changes only `config.autopilot.plan_fix.max_rounds` and keeps the computer's other values.
   - **Docs** (`docs/design/autopilot-plan.md`):
     - add `plan_fix` to the printed `AutopilotConfig` and `AutonomySettings` shapes (§3.5);
     - add a "Plan fixing" item, with defaults and ranges, to the settings list above them;
     - in S02's milestone row and in "S02: the plan review's cap", replace "the Balanced cap of 3" and `AUTOPILOT_PLAN_REVIEW_ROUNDS` with the plan-fix limits.

2. **Settings UI**
   - Files: `src/lib/autopilotSettings.ts`, `src/components/AutopilotSettings.tsx`, `src/components/HostAutonomy.tsx`, `src/lib/hosts.ts`, `src/lib/subagentSettings.ts`, `src/components/AutopilotSettings.test.tsx`, `src/components/CloudHostsSettings.test.tsx`, `src/components/SubagentSettings.test.tsx`
   - **`autopilotSettings.ts`:**
     - add `interface AutopilotPlanFix { maxRounds: number; stallRounds: number; maxMinutes: number; budgetShare: number; maxRetries: number }`. `budgetShare` is a fraction (0.4); the pages show it as a percentage;
     - add `DEFAULT_PLAN_FIX = { maxRounds: 8, stallRounds: 2, maxMinutes: 30, budgetShare: 0.4, maxRetries: 2 }` and `planFixOf(settings)`, and clamp to Step 1's ranges in `normalizeAutopilotSettings`;
     - add `planFix?: AutopilotPlanFix` to `AutopilotSettings` and to the TS `AutopilotConfig`;
     - `autopilotConfig()` sends it as `planFix: planFixOf(settings)`;
     - `autopilotSummary` states the plan limits.
   - **Pages:**
     - Add a "Plan fixing" group to `AutopilotSettings.tsx`, following the budget fields: max reviews, reviews without progress, minutes, budget share in %, automatic retries.
     - Add the same group to `HostAutonomy.tsx`. An empty field means "as on this computer" and is sent as an absent field of `planFix`.
     - Add `planFix?: Partial<AutopilotPlanFix>` to `AutonomySettings` in `hosts.ts`.
   - **`subagentSettings.ts`:** change only the `planReviewer` entry's `when`. In Autopilot, the plan review continues within the "Plan fixing" limits, and also settles a disputed finding. No new kind.
   - **Tests:**
     - `AutopilotSettings.test.tsx`: `autopilotConfig` sends `planFix`, and clamping works;
     - `CloudHostsSettings.test.tsx`: a host's plan-fix field is sent inside `planFix` in the `hosts_set_autonomy` payload, an emptied one disappears from it, and the existing payload assertions still hold;
     - `SubagentSettings.test.tsx`: update any assertion on the old `planReviewer` wording.

3. **Prompts that say what the code does**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`
   - `PLAN_REVIEWER_PROMPT` (line 95) and the follow-up part of `brief` (line 219) still say "high ones anywhere, others only about what the revision changed". Rewrite both to match `follow_up`: every new finding counts, wherever it is. Also say:
     - style, naming and a design you'd prefer are never findings, at any severity;
     - each finding's "Fix" is the replacement text (a step's line, a Files entry, a criterion), not advice;
     - a new finding about a step the revision didn't change adds a "- Evidence:" line with a `path:line` or a symbol name, and what breaks (enforced in Step 5).
   - These lines apply in every mode, because they correct existing text.
   - **First review, Autopilot only:** in `brief`, when `p.autopilot` and `p.earlier.is_none()`, a checklist goes next to `CRITERIA_REVIEW`: callers, names exist, order, scope, validation, criteria. It adds "when one step has a problem, list every step with the same problem". Follow-ups don't get it.
   - **Tests:** update `the_brief_has_the_request_answers_and_plan_never_the_conversation` and the prompt assertions in `the_hold_asks_for_checked_fixes_and_answers`.

4. **Plan-stage limits replace the fixed count**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/loops.rs`, `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/run.rs`, `src-tauri/crates/rovik-core/src/agent_run/budget.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/stage.rs` (read), `src-tauri/crates/rovik-core/src/agent_run/autopilot/mod.rs` (read), `src-tauri/crates/rovik-core/src/agent_run/autopilot/inside.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot_e2e_tests.rs`, `src-tauri/crates/rovik-core/src/agent_run/subagent_e2e_tests.rs`
   - **`loops.rs`:**
     - add `stall` and `cap` fields to `Progress`, and `Progress::with(stall: u32, cap: u32) -> Progress`;
     - `another_round` compares against `self.stall` and `self.cap`;
     - drop `Default` from the derives and write it by hand (`best: None, stalled: 0, stall: STALLED, cap: MAX_ROUNDS`). A derived one would be 0/0 and end every Thorough loop after one round;
     - add `Progress::best(&self) -> Option<usize>` and `Progress::restored(stall, cap, best, stalled) -> Progress`, so Step 5 can save and restore the state.
   - **Weighted progress:** add `fn weight(findings: &[Finding]) -> usize` (high 3, medium 2, low 1) in `plan_review.rs`. The Balanced Autopilot loop measures progress by weight; Thorough keeps its count.
   - **`run.rs`:** add `fn balanced_autopilot(&self) -> bool` (`in_autopilot() && !thorough() && sub().is_none()`). In a Balanced Autopilot run, `plan_review_progress` starts as `Progress::with(plan_fix.stall_rounds, plan_fix.max_rounds)`, from `self.config().autopilot.plan_fix`, unless Step 5 restored it.
   - **`plan_review_rounds` / `another_plan_review`:**
     - Balanced Autopilot follows that progress, as `thorough()` does today;
     - Plan mode keeps `MAX_PLAN_REVIEW_ROUNDS`, and Thorough keeps `loops::MAX_ROUNDS`;
     - remove `AUTOPILOT_PLAN_REVIEW_ROUNDS`, and update its users: `plan_review_rounds`, the `findings_result` assertions in `the_hold_asks_for_checked_fixes_and_answers`, and the design doc (Step 1);
     - rounds count across an automatic retry: `plan_reviews` starts from the count Step 5 restores, so `max_rounds` is the stage's total.
   - **Plan share of the budget:** add `pub fn share(budget: &Budget, fraction: f64, max_minutes: f64) -> Budget` to `budget.rs`. It scales `max_cost` and `max_tokens` by the fraction and sets `max_hours` to `max_minutes / 60`.
   - **`over_plan_share`:** add `async fn over_plan_share(&self) -> Option<String>` on `Run`, in `plan_review.rs`.
     - It loads the task's stage records with the `autopilot` module's record loader (the one `driver.rs` uses as `load`), sums `total_spent` over the records named `Stage::Plan`, and checks `budget::over(&share(..), &spent)`.
     - This run is already in that sum, through its record's `spending`, so it isn't added again.
   - **Where it's checked (Balanced Autopilot):**
     - before each review, a block instead of a review when the run is over the share, or when `plan_reviews >= max` (a restored count can already be at the limit). This replaces the "Review skipped" branch there, because an unreviewed plan is never saved. Plan mode and Thorough keep that branch;
     - after each hold: over the share, the hold becomes the final block;
     - the record says why: `review["stop"] = "plan-time" | "plan-budget" | "rounds" | "stalled" | "reviewer-unavailable"`.
   - **Blocks before a review:**
     - they still return a review record (`{"kind": "plan", "status": "blocked", "blocked": true, "stop": …, "round": plan_reviews}` plus Step 5's `ledger` and `planState`), which `submit_plan` puts in `meta.review` as the hold path does, so the block is restored and never retried;
     - with held findings open, the message is `blocked_result`;
     - with none, it's a new `fn unreviewed_blocked_result(stop: &str, rounds: u32) -> String`, which names the limit and lists no findings.
   - **Messages:** `findings_result` gets `(findings, round, max, autopilot, limits: Option<(&Progress, f64)>)`. In Balanced Autopilot, `limits` carries the progress and the minutes left, and the text states the real rule ("reviews continue while each leaves less open; at most N more reviews, about M minutes left"). Update its call in `plan_review` and the test.
   - **Records:** `review_record` in `inside.rs` puts the stop reason in the blocked line ("Blocked after 5 reviews (time limit): 2 findings open", or "…: no review left" with none open), for every stop value above.
   - **Existing tests:**
     - `a_high_finding_never_fixed_blocks_the_plan_after_three_reviews`, `a_plan_review_that_cant_report_keeps_its_high_finding_open`, `a_last_review_that_cant_report_leaves_its_high_finding_open` and `a_high_finding_the_follow_ups_dont_settle_ends_the_task_review_open` (`autopilot_e2e_tests.rs`) set `plan_fix.max_rounds = 3` and expect the new blocked line;
     - `budget_used_record_shape` and `a_failing_plan_reviewer_saves_the_plan_with_a_note` (`subagent_e2e_tests.rs`) keep their "Review skipped" assertions in Plan mode. If either runs in Balanced Autopilot, it expects the block (Step 4) or the wait (Step 9) instead.
   - **Tests:**
     - `loops.rs`: custom stall and cap, restored state, `Default` unchanged;
     - weight-based progress and `budget::share`;
     - the over-share and rounds blocks before a review, with their records, in Balanced Autopilot, and "Review skipped" still in Plan mode;
     - `unreviewed_blocked_result`, and each stop reason in `review_record`.

5. **A finding list kept across reviews and retries, with evidence and regressions**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/run.rs`, `src-tauri/crates/rovik-core/src/agent_run/change_review.rs`, `src-tauri/crates/rovik-core/src/agent_tools.rs` (read)
   - **Ledger:**
     - add `struct LedgerEntry { number: usize, finding: Finding, status: Status, disputes: u32, ruled: bool }`, where `Status` is Open, Fixed, Disputed, Resolved or Reopened;
     - add a `plan_ledger: Vec<LedgerEntry>` field on `Run`, used in Balanced Autopilot only. It keeps resolved entries too;
     - a finding keeps its number across holds; new ones take the next number;
     - `plan_findings` still holds the open findings, in ledger order, so `describe`, `follow_up` and the verdict parsers keep working on it. The ledger carries the numbers. `review_plan`'s save branch marks every entry Resolved.
   - **Numbers in messages:** the author sees the open entries as "F3" in `findings_result`, and `responses` refer to F-numbers. The reviewer's "## The earlier review" keeps its positional "Earlier 1, 2…", which `follow_up`, `parse_verdicts` and `missing_verdicts` read, with "(F3)" after each.
     - The run maps both ways: position to F-number when it records verdicts, and F-number to position when it reads responses for `brief` and `follow_up`.
     - In Balanced Autopilot, `unanswered` / `unanswered_problem` check the answers against the open ledger numbers.
   - **Saved state:** each Balanced Autopilot review record keeps `review["ledger"]` and `review["planState"] = { round, best, stalled, held, blocked, formatRejections }`.
     - `held` is the held plan text, `blocked` mirrors `plan_blocked`, and `formatRejections` is Step 6's counter.
     - Held, saved and blocked `submit_plan` results all carry it in `meta.review`.
   - **What a run restores** (`prepare`, Balanced Autopilot only, from the latest such record in the task's chat):
     - **an automatic retry** (Step 9, recognised by its `planRetry` note) restores everything: `plan_ledger`, `plan_reviews` (from `round`), `plan_review_progress` (`Progress::restored`), `plan_held`, `plan_blocked` and the format counter;
     - **a person's Retry or Continue** restores only the ledger and the held plan (as the draft), and starts with a fresh review count, stall count and `plan_blocked = false`, as a Retry does today.
   - **Evidence rule:**
     - Add `pub evidence: Option<String>` to `change_review::Finding`, read from a "- Evidence:" line: add `evidence` to the `FIELD` regex and to `parse_findings`. Its only other literal, the test helper `finding()` in `plan_review.rs`, gets `evidence: None`. Other `Finding` types in other crates are unrelated.
     - Add `pub changed: &'a [usize]` to `Facts`.
       - The real literal in `plan_review` passes `&changed_steps(&held, plan)`.
       - The one in `change_review.rs` (`review_change_as`) and the four in `a_follow_up_keeps_what_is_unresolved_and_new_findings_where_the_plan_changed` pass `&[]`, which keeps today's behaviour there.
     - In Balanced Autopilot, a new finding about a step that isn't in `changed` is held only when its evidence checks out. Its cited path must exist under the project root, or its symbol must be found by `crate::agent_tools::tool_search_code` (the search behind the reviewer's own `search_code` tool), and the line must say what breaks.
     - Otherwise it's recorded under `review["dropped"]` with the reason and never sent to the author. The next review's brief lists the dropped titles: "dropped for lack of evidence; report again only with evidence".
   - **Regressions:** a new finding that `same_finding` matches to a Resolved ledger entry reopens that entry as "Reopened: your revision undid the fix for F2". It's held without needing evidence.
   - **Tests:** extend `a_follow_up_keeps_what_is_unresolved_and_new_findings_where_the_plan_changed` for the evidence rule (in Balanced Autopilot only), and add tests for:
     - `parse_findings` reading "- Evidence:";
     - ledger numbering across three holds;
     - `unanswered` by ledger number;
     - regression reopening;
     - restoring after an automatic retry versus a person's Retry, where a person's Retry after a block at the limit can still review and save;
     - the change review's follow-up unchanged with `changed: &[]`.

6. **Mechanical format fixes and a skeleton after repeated format failures**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/plan.rs`, `src-tauri/crates/rovik-core/src/agent_run/execute.rs`, `src-tauri/crates/rovik-core/src/agent_run/run.rs`
   - **`normalize`:** add `pub fn normalize(markdown: &str, root: Option<&Path>, request: Option<&str>) -> (String, Vec<String>)` in `plan.rs`. It returns the fixed plan and one line per fix, and makes only these fixes:
     - inside `Files:` lines only (found with `lines_with_fences` and `FILES_LINE`, outside code fences), per token:
       - a `../x` token that `file_path` reports as `BadPath::Outside(Some(inside))` becomes `inside`;
       - a bare file name (no `/`) missing at the root becomes its full path when `files_named` finds exactly one;
       - text outside `Files:` lines is never replaced, and a token it can't settle is left alone;
     - a missing **Out of scope** or **Assumptions** part under "## Criteria" becomes "None.";
     - a code fence left open at the end is closed;
     - a missing "# " title is taken from the request's first line; with no request, nothing is fixed.
   - **`skeleton`:** add `pub fn skeleton(markdown: &str) -> String`. It gives the "# " title, the parsed steps (`steps`) with their `Files:` lines, and every missing section, with `TODO:` where the model must write.
   - **`submit_plan`** in `execute.rs`, in Balanced Autopilot only: load `recent()` and the request first (the latest `plan::is_request` message, as the save path does), then run `normalize` after `without_findings` and before `plan_problems_for`. The saved and reviewed plan is the fixed one, and the result says "The IDE fixed: …".
   - **Format-failure counter:** add `plan_format_rejections: u32` on `Run`, saved and restored with `planState` (Step 5). After 2 rejections for checker problems alone, the error result also contains `plan::skeleton(plan_text)`.
   - Format rejections never use a review round.
   - **Tests:**
     - `normalize`: each fix, a `../` token and a bare name inside and outside a `Files:` line, that it changes nothing else, and no title without a request;
     - `skeleton`.

7. **Revisions as edits to the draft**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/tools.rs`, `src-tauri/crates/rovik-core/src/agent_run/execute.rs`, `src-tauri/crates/rovik-core/src/agent_run/run.rs`, `src-tauri/crates/rovik-core/src/edit_file.rs` (read), `src/types.ts`, `src/components/ToolCallBlock.tsx`, `src/components/ToolCallGroup.tsx` (read), `src/lib/toolGroups.ts`, `src/components/records.test.tsx`, `src/lib/toolGroups.test.ts`
   - **Draft:** add `plan_draft: Option<String>` on `Run`: the last plan submitted after `normalize`, whether held or saved. At a run's start it comes from the `held` text Step 5 restores, else the task's saved plan file (`chat_plan_file`, or `task_plan_file` in Autopilot).
   - **Schema (`submit_plan_schema`):**
     - add an optional `edits` array whose items are `{old_text, new_text}` objects, both strings, both in the item's `required`;
     - rewrite the description's "a revision passes the complete revised plan" to "after a review, send `edits` (exact text replacements in your last plan) instead of the whole plan";
     - drop `"plan"` from `required`.
   - **`submit_plan`:**
     - Before `plan_problems_for`, with neither argument: `Error: the plan wasn't saved. Neither \`plan\` nor \`edits\` was given: send the complete plan.`
     - With `edits` but no draft: `Error: the plan wasn't saved. There's no earlier plan to edit: send the complete plan.`
     - With `edits` and a draft, apply each edit in order with `crate::edit_file::apply_edit(&draft, old, new, None, None)`. Fully qualify it, because `agent_run::tools::apply_edit` is a different function.
     - An edit that doesn't match exactly once returns `Error: the plan wasn't saved. Edit N: <apply_edit's message>`, and nothing is applied.
     - Each message starts with "the plan wasn't saved", so `ERROR_CLASSES` keeps classing it as "plan".
     - The edited plan is then checked and reviewed like a full plan.
   - **Result meta:** a `submit_plan` result carries `meta.plan`:
     - when the plan was checked, it's the plan as checked (full, or after edits), held and rejected results included;
     - when the edits didn't apply, it's the draft as it was.
   - **Frontend:**
     - add `plan?: string` to `ToolMeta` in `src/types.ts`;
     - in `ToolCallBlock.tsx`, `describe` (case `submit_plan`) and the plan view (the `str(args().plan)` block) read `args.plan`, else the result's stored `meta.plan`;
     - `callText`'s omit list stays `['plan']`. When there's no `args.plan`, the `edits` array is what the Call view shows;
     - widen `describeGroup`'s `results` parameter in `toolGroups.ts` to `({ dir?: string; plan?: string } | undefined)[]`, and in its `submit_plan` case map with the index (`argsList.map((a, i) => …)`), reading `str(a.plan) ?? results[i]?.plan`. Its caller in `ToolCallGroup.tsx` already passes each call's result meta;
     - an edits submit shows its title, line count and plan, with "(N edits)" in the label.
   - **Scope:** `edits` is offered in every mode, because it's the same tool. Only Balanced Autopilot's findings result suggests it. A model that ignores it keeps working as today.
   - **Tests:**
     - Rust:
       - edits applied in order;
       - a missing or ambiguous match;
       - edits without a draft, and neither argument, each with its error class "plan";
       - `meta.plan` on saved, held and rejected results;
       - a standalone `submit_plan_schema` test (`edits` items require both keys, `required` is empty, `responses` unchanged).
     - TS:
       - an edits-only submit is described and shown from `meta.plan` (`records.test.tsx`, `toolGroups.test.ts`);
       - two edits submits with the same title differ by their "(N edits)" label.

8. **An arbiter for disputed findings, run by the plan reviewer**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/subagent_run.rs` (read)
   - **Trigger:** in Balanced Autopilot, after `follow_up`, a ledger entry gets `disputes += 1` when the author answered it `not_a_problem` and the reviewer still gave "not resolved" without new evidence.
     - When `disputes` reaches 2 and `!ruled`, the arbiter runs there, inside `plan_review` and before the hold-or-save decision, just as the review itself runs inside the `submit_plan` call.
     - It runs at most once per review.
   - **The run:** a fresh `Kind::PlanReviewer` subagent, with `run_ide_subagent_on(Kind::PlanReviewer, call_id, "Settle a disputed finding", arbiter_brief(..), Vec::new())`. It reads, never writes, and uses the reviewer's model list and fallbacks. There's no new kind and no new setting.
   - **`arbiter_brief`:** add `fn arbiter_brief(request, plan, entry, author_notes, reviewer_reasons) -> String`. It gives the plan, the one finding and both sides' arguments, and tells the model to check the code.
     - It overrides the reviewer's general instructions explicitly. The reply is exactly one of two things:
       - "## Findings" then "None." if the finding doesn't hold;
       - "## Findings" then one finding (`1. [severity] Step N — title` with `- Scenario:`, `- Fix:`, `- Confidence:`) if it does.
     - No other findings about the plan, however real.
   - **Ruling:** read with `parse_findings`.
     - An empty list resolves the entry ("ruled not a problem").
     - Exactly one finding that matches the entry (`same_finding`, or the same step) keeps it open, marked "ruled valid: fix it".
     - Anything else, or a failed or skipped run, is unreadable. The entry stays open and `ruled` stays false, so a later review can try once more, within `max_rounds`.
     - A readable ruling sets `ruled = true`, and the entry is never ruled again.
     - The ruling goes in `review["rulings"]` and is saved with the ledger. A resolved ruling can let the same submit be saved.
   - **Tests:**
     - the dispute count (only `not_a_problem` vs "not resolved" without new evidence);
     - the trigger at 2, once per review;
     - the brief's content;
     - each ruling: None resolves, the matching finding keeps it open, another finding or a parse error leaves it unruled, a skip leaves it unruled.

9. **No dead ends: a plan written in the reply, automatic retries, reviewer outages**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/run.rs`, `src-tauri/crates/rovik-core/src/agent_run/planning.rs`, `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/driver.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/outcome.rs`, `src-tauri/crates/rovik-core/src/agent_run/autopilot/rate_limit.rs` (read)
   - **`submit_for_review`** in `planning.rs` gets `responses: Option<Value>` and `harness: &str`.
     - With `Some` responses, the call it builds has `"responses"` next to `"plan"`, in the `{finding, action, note}` shape `parse_responses` reads.
     - `harness` replaces the hard-coded `"thoroughPlan"` in its two messages' meta. Its two callers in `thorough_planning` pass `None` and `"thoroughPlan"`, so `thorough_plan_due` behaves as today.
   - **Plan in the reply:** in `finish_plan_reply`, in Balanced Autopilot, when no `planFile` result is found and the reply `looks_like_plan`:
     - call `submit_for_review(text, responses, "planFromReply")`;
     - `responses` is `Some` only while ledger findings are held: one `{action: "fixed", note: "revised in the reply"}` per open F-number, read back by Step 5's `unanswered`. Otherwise it's `None`;
     - return `Ok(true)` without setting `plan_nudged`, so the model answers the new result on its next turn;
     - the one-nudge path below stays for Plan mode and Thorough.
   - **Automatic retry, the rule:** in `outcome.rs`, add `pub(super) fn auto_retries(outcome: &str, effort: Effort) -> bool`. It's true for `"error"`, `"repetition"` and `"plan-failed"`, in `Effort::Balanced` only. `RUN_FAILURES`, `failure_of` and `reopens` stay as they are, so a person's Retry in either effort behaves as today.
   - **Where it hooks:** in `after_run` (`driver.rs`), after `end_run_stage` and before `advance`.
     - Load the ended record (`load`) and read **its** `outcome`, `effort`, `task` and `round`. `end_run_stage` sets `"plan-failed"` there, never in the run's outcome, and `Settings` carries no effort.
     - Read the limits with `core.agents.config_for(&dialog_id).autopilot.plan_fix`, as `end_run_stage` reads the budget. `Settings` isn't extended.
     - It qualifies when all of these hold:
       - the stage is `Stage::Plan` and the record is Failed;
       - `auto_retries(record.outcome, record.effort)`;
       - the automatic retries already made are **fewer than** `plan_fix.max_retries`. They're counted as the `planRetry` notes in the task's chat since the task's message, so a person's Retry neither uses up nor restores one; `0` never retries, and `2` allows exactly two;
       - the plan share isn't spent (`budget::share` against the Plan records' `total_spent`);
       - the stage's latest saved `planState` isn't blocked and is below `max_rounds`.
     - If it qualifies, `retry_plan`:
       1. appends a note to the task's chat, with `meta.harness = "planRetry"`: "the plan stage starts again after <why>; continue from the draft and the open findings";
       2. records a new round (`StageRecord::running(&record.task, Stage::Plan, record.round + 1, record.effort)`, `add_stage`);
       3. starts its run with `start_run(.., StartKind::Retry)`, as `start_harness_stage` does under the driver's turn;
       4. then **returns without calling `advance`**, so no Deliver record is written.
     - `stage_for` takes the new round up through its running-record branch, because `Stage::Plan` has a run. `reopens` doesn't change.
     - Otherwise `advance` runs as today, and a failure past the retries delivers.
   - **The retried run:** sees its `planRetry` note and restores the full state (Step 5), including the draft (Step 7).
   - **Reviewer outage:** in `plan_review`, in Balanced Autopilot, a review skipped because every plan reviewer model failed is retried after a short wait (30 s, then doubled).
     - Each wait races the run's stop, the plan time limit and the run's own wall and budget, and never sleeps past the first of them.
     - At that limit, it blocks with stop reason "reviewer-unavailable" (Step 4), with its record. It's never saved unreviewed.
     - Plan mode keeps "Review skipped".
   - **Tests:**
     - `auto_retries`: Balanced vs Thorough, each outcome;
     - the retry decision: `max_retries = 0` never retries; at the exact limit (2 notes with `max_retries = 2`) it doesn't retry; a person's Retry round isn't counted; the share; a saved state at the limit; a Stop;
     - the reply extraction returns `true`, and carries responses only while findings are held;
     - Thorough's `submit_for_review` call is unchanged;
     - the outage: the wait, a Stop during it, and the block at each limit.

10. **Logging**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/plan_review.rs`, `src-tauri/crates/rovik-core/src/agent_run/execute.rs`
   - The `planReview` log event adds `weight`, `stalled`, `dropped` (no evidence), `reopened`, `disputes`, `ruling` and `stop`.
   - Add a `planSubmit` event with `formatRejections`, `fixes` (normalize), `edits` and `fromReply`. The eval in Validation reads these from the log.

## Decisions

- **Scope:** everything is limited to Balanced Autopilot (`in_autopilot() && !thorough()`, `Effort::Balanced` in the driver). The user asked for Balanced and said not to touch Thorough.
  - Plan mode gets only the corrected prompt text (Step 3), because the current text contradicts the code in every mode, and the optional `edits` argument (Step 7), because it's one shared tool and harmless when unused.
  - The retry rule is a Balanced-only `auto_retries`, not a change to `RUN_FAILURES`, so a person's Retry stays the same everywhere. It also restores no review count or block (Step 5).
  - `submit_for_review`'s new arguments keep Thorough's calls as they are.
  - Thorough's `Progress` keeps its values through a hand-written `Default`.
- **No Plan fixer** (user). Handing a stalled plan to another model risks what the research warns about: a handoff loses what the author learned, and names drift. One author writes the plan throughout. So there's no new subagent kind, no escalation setting, and no change to `needed_kinds` or the Subagents page's kinds.
- **The arbiter is the plan reviewer, not a writer.** The research supports clean-context critics that only read. A fresh plan reviewer run, told to rule on one finding only, needs no new kind, format or model row. It runs inside `plan_review`, as the review does, so message order doesn't change. Only a readable ruling settles a finding, so an outage never settles one by default.
- **Every finding must be resolved** (user). The ledger, the evidence rule and the arbiter decide *what counts as a finding* and *when it's settled*; they never let an open finding through. The final block stays as it is (user). In Balanced Autopilot a plan is never saved unreviewed: running out of reviews, time or budget blocks it.
- **Limits are settings** (user), with the defaults the user accepted: 8 rounds, 2 without progress, 30 minutes, 40% budget share.
  - Automatic retries (default 2; 0 turns them off) are a setting too, since the user wants fixing "limited by max time/budget/tries in settings".
  - A host overrides them field by field, as it does the budget.
  - Rounds, the stall count and time are totals for the stage across automatic retries.
- **An automatic retry is marked by its note, not by a new stage-record field.** That leaves the stage record's many literals alone, and it tells an automatic retry from a person's Retry in the chat.
- **The evidence rule drops a finding instead of asking the reviewer again:** the reviewer's repair turn carries verdicts only, and the next review may report a dropped finding again with evidence.
- **The IDE fixes only mechanical format problems** (Step 6), only inside `Files:` lines and the plan's frame, never reviewer findings of any severity. The user agreed: a finding's fix is another model's opinion in free text.
- **`edits` uses `edit_file`'s matching rules**, not new per-step tools: models already use that shape with `write`, and it's one tool. The UI reads the checked plan from `meta.plan`, so an edits submit shows like a full one.
- **The ledger isn't applied to the change review**, which stays out of scope. The change review only gets `Facts.changed` as empty and the parsed `evidence` field, unused there.
- **Progress weighs findings** (high 3, medium 2, low 1), so fixing two highs while one low appears counts as progress. Thorough keeps its count.

## Validation

- `cargo test -p rovik-protocol`, `cargo test -p rovik-client`, `cargo test -p rovik-core` (the unit tests in each step), `cargo clippy --workspace`.
- `npm run typecheck`, and `npm test` for `autopilotSettings`, `AutopilotSettings.test.tsx`, `CloudHostsSettings.test.tsx`, `SubagentSettings.test.tsx`, `records.test.tsx` and `toolGroups.test.ts`.
- New e2e tests in `src-tauri/crates/rovik-core/src/agent_run/autopilot_e2e_tests.rs` with the scripted model:
  - a Balanced author that needs 5 reviews, each with less open, and gets the plan saved;
  - two stalled reviews, which block with stop reason "stalled";
  - a dispute settled by the arbiter, ruled not a problem, and the plan saved on that submit;
  - the plan time limit reached with a finding open, which blocks with the stop reason;
  - a plan written in the reply, extracted and saved;
  - a run error, retried automatically, with no Deliver record written before the retry. It then saves, and its review rounds continue from the first run's count;
  - a retry restored at `max_rounds - 1`, whose one remaining review either saves or blocks with stop reason "rounds", and never saves unreviewed;
  - a person's Retry after a block at the limit, which starts with a fresh review count and can save;
  - a revision sent as `edits` after a hold, reviewed and saved;
  - a Thorough task whose plan loop and Retry behave as before.
- The four existing 3-review e2e tests pass with `plan_fix.max_rounds = 3`, and the two "Review skipped" tests in `subagent_e2e_tests.rs` pass in Plan mode (Step 4).
- By hand:
  - run a Balanced Autopilot task on a weak model, and check that the stage strip shows rounds and stop reasons;
  - change the settings on Settings → Autopilot and on a host, and check that they're sent and followed;
  - check that an edits-only submit renders its plan in the chat.
- Eval: add 3–5 plan-heavy tasks to `scripts/eval/tasks`. Compare reviews-to-save, block rate, cost and time on a weak model before and after, from the `planReview` and `planSubmit` log events.

## Risks

- **Cost:** more reviews and arbiter runs spend tokens. The plan share (default 40%) limits all of them, and the implementation keeps the rest.
- **A weak author may still stall.** Without a fixer, a model that can't make progress hits the stall limit and blocks, as the user chose. The eval shows how often.
- **The evidence rule may drop a real finding** that the reviewer explained badly. The next review is told and may report it again with evidence, and dropped findings stay in the record.
- **The arbiter shares the reviewer's system prompt,** which asks for a whole-plan review. The brief overrides it, and a reply that doesn't answer the brief is treated as unreadable, but a model may still drift. The tests cover the parsing; the eval shows how often rulings are unreadable.
- **Appending the `planRetry` note from the driver:** `retry_plan` must write it the way the driver's other harness rows are written, before `start_run`, under the driver's turn. Check this against `start_harness_stage` and `retry_later` while implementing.
- **Record size:** saving the held plan in each review record (`planState.held`) adds up to 40K characters per review. If that's too much, keep only the latest record's.
