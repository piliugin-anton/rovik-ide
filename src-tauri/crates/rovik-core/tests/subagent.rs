//! The lead is told how long a brief may be, so its first `task` call fits.
//!
//! These use only what `rovik_core::agent_run::subagent` makes public: `lead_prompt`, `Kind`,
//! `MAX_GOAL_CHARS` and `MAX_CONTEXT_CHARS`.

use rovik_core::agent_run::subagent::{lead_prompt, Kind, MAX_CONTEXT_CHARS, MAX_GOAL_CHARS};

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
