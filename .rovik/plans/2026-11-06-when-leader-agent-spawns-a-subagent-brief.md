# State the brief's length limits in the lead's prompt

A `task` call's `goal` is capped at `MAX_GOAL_CHARS` (2,000) and its `context` at `MAX_CONTEXT_CHARS`
(6,000); `parse_brief` refuses a longer goal, so the call comes back as an error, the lead loses a
turn and can trip the guard's "the same error again" rule. Today only the `task` tool schema's field
descriptions mention the caps, deep inside the tool definition. Add one shared sentence to the
`## Subagents` section `lead_prompt` builds, with the numbers from the constants, so the lead writes
a brief that runs on its first call.

## Steps

1. Add the brief's limits to the lead's subagent section
   - Files: `src-tauri/crates/rovik-core/src/agent_run/subagent.rs`
   - In `lead_prompt` (around line 1186), `format!` one paragraph from `MAX_GOAL_CHARS` and
     `MAX_CONTEXT_CHARS`: a brief's `goal` is at most 2,000 characters and its `context` at most
     6,000 (a longer context is cut, a longer goal comes back as an error and costs a turn), so write
     the goal as one question, the background under `context`, and the first files under `paths`. It
     is prepended inside the last expression, `Some(format!("## Subagents\n\n{rule}\n\n{}",
     parts.join("\n\n")))`, leaving `if parts.is_empty() { return None; }` and the per-kind `parts`
     as they are, so `lead_prompt(&[], …)` still returns `None` and a chat with no subagent kind on
     gets no section. The section keeps its `## Subagents` heading first, so the existing check that
     the lead's prompt has it still holds, and `parse_brief`'s refusal plus the schema's field
     descriptions stay as they are.

2. Test that the lead is told both limits
   - Files: `src-tauri/crates/rovik-core/src/agent_run/subagent.rs`
   - New unit test `the_leads_prompt_states_the_briefs_length_limits` in the file's `tests` module:
     for `[Kind::Scout]`, `[Kind::Worker]` with parallel off and on, and `[Kind::TestWriter]`, the
     prompt contains the goal cap and the context cap built from the same constants (so the wording
     can't drift from the check), exactly once each, and before the first kind's own bullets. Leave
     `prompts_carry_the_budget` and `the_prompts_offer_groups_only_when_parallel_workers_can_run`
     unchanged; they must still pass.

3. Document the caps where the brief is described
   - Files: `docs/design/subagents.md`, `src-tauri/crates/rovik-core/src/agent_run/subagent.rs` (read)
   - In "The subagent contract", under "Brief", one sentence after the JSON block: a `goal` over
     2,000 characters is refused and `context` is cut at 6,000, and the lead's prompt states both, so
     a brief normally fits on its first call.

## Decisions

- Prompt line only (the user's answer): `parse_brief` keeps refusing a too-long goal, because a cut
  goal can ask for the wrong thing, and the guard's handling of the error stays as it is.
- One shared sentence for every kind (the user's answer), above the per-kind bullets rather than
  repeated in each, so a kind added later gets it for free.
- The numbers come from `MAX_GOAL_CHARS` and `MAX_CONTEXT_CHARS` in the paragraph's `format!`, so the
  prompt can't disagree with the check in `parse_brief`.

## Validation

- From `src-tauri/`: `cargo nextest run -p rovik-core subagent` (or `cargo test -p rovik-core
  subagent`): the new test plus `prompts_carry_the_budget`,
  `the_prompts_offer_groups_only_when_parallel_workers_can_run`,
  `a_brief_needs_an_enabled_kind_and_a_goal`, and the e2e
  `a_scout_explores_and_reports_to_the_lead` (which asserts the lead's prompt contains
  `## Subagents`).
- `cargo clippy -p rovik-core --tests` adds no warnings; format with
  `rustfmt --edition 2021 crates/rovik-core/src/agent_run/subagent.rs`.
- No frontend file changes, so `npm test`, `npm run typecheck` and `npm run format:check` need nothing
  new beyond their current state.
- By hand: with scouts on, ask a broad question and check the request the lead sends carries the two
  caps in its system prompt; then give a subagent a long goal on purpose and see it refused.

## Risks

- About 60 extra tokens on every lead turn that has subagents on: negligible next to the reports.
- A model that ignores the system prompt still gets refused, as today; the refusal already says what
  to do, so nothing gets worse.
