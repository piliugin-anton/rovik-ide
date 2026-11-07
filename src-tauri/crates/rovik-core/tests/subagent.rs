//! The lead is told how long a brief may be, so its first `task` call fits,
//! and which kinds the user's switches leave on.
//!
//! These use only what `rovik_core::agent_run::subagent` makes public:
//! `lead_prompt`, `Kind`, `SubagentSettings`, `MAX_GOAL_CHARS` and
//! `MAX_CONTEXT_CHARS`.

use rovik_core::agent_run::subagent::{
    lead_prompt, Kind, SubagentSettings, MAX_CONTEXT_CHARS, MAX_GOAL_CHARS,
};
use rovik_protocol::subagents::SubagentConfig;
use serde_json::{json, Value};


/// The `## Subagents` section a chat with a kind on gets.
fn lead_section(kinds: &[Kind], parallel: bool) -> String {
    lead_prompt(kinds, 6, parallel)
        .expect("a chat with a subagent kind on gets a ## Subagents section")
}

#[test]
fn the_leads_prompt_states_the_briefs_length_limits() {
    // Both caps are built from the numbers the runtime enforces, so the prompt cannot disagree
    // with `parse_brief` (2_000 renders as "2000": no thousands separator).
    let goal_cap = format!("at most {}", MAX_GOAL_CHARS);
    let context_cap = format!("at most {}", MAX_CONTEXT_CHARS);
    let heading = "## Subagents\n\n";
    let cases: [(&[Kind], bool, &str); 4] = [
        (&[Kind::Scout], false, "At most 4 scouts per turn"),
        (&[Kind::Worker], false, "### Step workers"),
        (&[Kind::Worker], true, "### Step workers"),
        (&[Kind::TestWriter], false, ""),
    ];
    for (i, &(kinds, parallel, marker)) in cases.iter().enumerate() {
        let prompt = lead_section(kinds, parallel);

        // The heading stays first, as the rest of the prompt and the e2e tests rely on.
        assert!(
            prompt.starts_with(heading),
            "case {i}: the section starts with its ## Subagents heading: {prompt}"
        );

        // One shared paragraph right under the heading, so every kind gets it before its own
        // bullets and a kind added later gets it for free.
        let first_paragraph = prompt[heading.len()..]
            .split("\n\n")
            .next()
            .unwrap_or_default();
        assert!(
            first_paragraph.contains(&goal_cap),
            "case {i}: the lead is told the brief's goal cap ({goal_cap:?}): {prompt}"
        );
        assert!(
            first_paragraph.contains(&context_cap),
            "case {i}: the lead is told the brief's context cap ({context_cap:?}): {prompt}"
        );
        for word in ["goal", "context", "paths", "turn"] {
            assert!(
                first_paragraph.contains(word),
                "case {i}: the limits paragraph mentions {word:?}: {prompt}"
            );
        }

        let goals = prompt.matches(&goal_cap).count();
        assert_eq!(
            goals, 1,
            "case {i}: the goal cap is stated once, not {goals} times: {prompt}"
        );
        let contexts = prompt.matches(&context_cap).count();
        assert_eq!(
            contexts, 1,
            "case {i}: the context cap is stated once, not {contexts} times: {prompt}"
        );

        // And before the kind's own bullets.
        if !marker.is_empty() {
            let marker_at = prompt
                .find(marker)
                .unwrap_or_else(|| panic!("case {i}: the prompt has {marker:?}: {prompt}"));
            let goal_at = prompt.find(&goal_cap).unwrap_or_default();
            let context_at = prompt.find(&context_cap).unwrap_or_default();
            assert!(
                goal_at < marker_at,
                "case {i}: the goal cap comes before {marker:?}: {prompt}"
            );
            assert!(
                context_at < marker_at,
                "case {i}: the context cap comes before {marker:?}: {prompt}"
            );
        }
    }

    assert!(
        lead_prompt(&[], 6, false).is_none(),
        "a chat with no subagent kind on gets no ## Subagents section"
    );
    assert!(
        lead_prompt(&[], 6, true).is_none(),
        "a chat with no subagent kind on gets no ## Subagents section, parallel or not"
    );
}

/// The settings as the app sends them: `resolveSubagentConfig`'s payload.
fn settings(payload: Value) -> SubagentConfig {
    serde_json::from_value(payload).expect("the app's own subagents payload")
}

/// A kind the IDE starts itself isn't the user's to switch off: the change
/// reviewer runs whether "Use subagents" and its own row's switch are on,
/// while a kind the lead sends still needs both.
#[test]
fn the_change_reviewer_runs_whatever_the_switches_say() {
    // One home for "the IDE starts it": the reviewer, and nothing else.
    let always_on: Vec<&str> = Kind::ALL
        .iter()
        .filter(|kind| kind.always_on())
        .map(|kind| kind.id())
        .collect();
    assert_eq!(
        always_on,
        [Kind::Reviewer.id()],
        "only the change reviewer ignores the switches (the test writer is gated at its own hooks)"
    );

    // Subagents off, and the reviewer's row off as well.
    let off = settings(json!({
        "enabled": false,
        "kinds": { "reviewer": { "enabled": false } },
    }));
    assert!(
        off.may_run(Kind::Reviewer),
        "neither switch turns off the kind the IDE starts itself"
    );
    assert!(!off.may_run(Kind::Scout), "a scout needs the switch on");

    // The master switch alone keeps an enabled helper off; the reviewer
    // doesn't wait for it. The same rows with the master switch on are kinds
    // that run, so it is the switch — not the row — that's off below.
    let rows = |enabled: bool| {
        settings(json!({
            "enabled": enabled,
            "kinds": { "reviewer": { "enabled": true }, "scout": { "enabled": true } },
        }))
    };
    let master_on = rows(true);
    assert!(
        master_on.kind(Kind::Scout).is_some(),
        "the scout's own switch is on: this row runs when the master switch is"
    );
    assert!(master_on.may_run(Kind::Scout));
    let master_off = rows(false);
    assert!(
        master_off.kind(Kind::Scout).is_none(),
        "the master switch hides even an enabled row from kind()"
    );
    assert!(
        !master_off.may_run(Kind::Scout),
        "a scout needs both switches, and the master one is off"
    );
    assert!(
        master_off.may_run(Kind::Reviewer),
        "the reviewer runs whatever the switches say"
    );

    // Everything on: what has no row of its own is off, the reviewer
    // included in "on".
    let on = settings(json!({
        "enabled": true,
        "kinds": { "scout": { "enabled": true } },
    }));
    assert!(on.may_run(Kind::Scout));
    assert!(
        on.may_run(Kind::Reviewer),
        "the reviewer has no row of its own and still runs"
    );

    // The default settings turn on no kind at all: the reviewer is the
    // exception, and it applies its row's models and budget when it has one.
    let default = SubagentConfig::default();
    assert!(default.may_run(Kind::Reviewer));
    assert!(!default.may_run(Kind::Scout));
    for kind in Kind::ALL {
        if kind.always_on() {
            assert!(default.may_run(*kind), "{} always runs", kind.id());
        } else {
            assert!(
                !default.may_run(*kind),
                "{} is off in a chat that turned no kind on",
                kind.id()
            );
        }
    }
}

