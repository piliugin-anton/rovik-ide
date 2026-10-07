//! What the agent is told in CLM mode: the system prompt section about its
//! context file, and the notes added to a request (edit receipts, the
//! context readout, budget reminders).
//!
//! Wording follows the Context Language Models harness (Shao et al. 2026:
//! the mirror protocol, the note contract of its budget reminders, "edits
//! that keep very little of a region are usually followed by re-doing the
//! deleted work") and pi-clm (surgical revisions over global summaries).

/// The path the agent's file tools use for its context file.
pub const CONTEXT_PATH: &str = "@context/LIVE_CONTEXT.md";
pub const REVISIONS_PATH: &str = "@context/revisions";
/// The agent's task card, in its context folder.
pub const TASK_FILE: &str = "TASK.md";
pub const TASK_PATH: &str = "@context/TASK.md";

pub fn system_section(
    real_path: &str,
    real_dir: &str,
    budget: usize,
    revision: Option<u64>,
    card: Option<&str>,
) -> String {
    let revision = revision
        .map(|r| format!(" (now revision {r})"))
        .unwrap_or_default();
    let card = match card {
        Some(text) => format!(
            "\n\n### Your task card (`{TASK_PATH}`)\n\nYou wrote this; it is shown here on every request, outside your context file, so no context edit can drop it. Keep it current.\n\n```\n{}\n```",
            text.replace("```", "``\u{200b}`")
        ),
        None => format!(
            "\n\n### Your task card (`{TASK_PATH}`)\n\nYou have none yet. When the work is more than a quick answer, write one before you start editing your context."
        ),
    };
    format!(
        r#"## Your context is a file (Context Language Models)

Everything you see in this conversation after this system prompt is mirrored to a file{revision}:
- `{CONTEXT_PATH}` with read_file / edit_file / write_file, or
- `{real_path}` for run_command (sed, python3, …) when commands are allowed.

It is rewritten before every request. **Whatever the file says when your next request is sent is your context from then on**: edit it and your next request contains the edited version; leave it alone and new turns are simply appended. Your context budget is about {budget} tokens; every request reports the current size.

Format: one block per message, in order:
```
[[CTX_TURN 3 role=tool id=…]]
<the text you saw>
```
- Keep the header line of every block you keep. Edit, shorten, merge, reorder or delete blocks freely. Remove a block by deleting it, header included.
- Add your own blocks with any role and a new id, e.g. `[[CTX_TURN 0 role=notes id=new-plan]]`: a plan, a tracker, a ledger of what's done and ruled out.
- A rewritten block is kept as plain text (a rewritten tool result loses its tool-call structure; that's fine).
- **The user's messages (`role=user`) are protected**: the task, its scope and every instruction since. Removing or rewriting one has no effect; the app puts it back as it was (a very long one, such as a pasted log, you may shorten). Compact your own work around them.

Your task card, `{TASK_PATH}`, is a separate file shown in this system prompt on every request; editing your context file never touches it. Keep in it what must survive every compaction: the goal, the scope and constraints (what not to touch), decisions taken, what's done, and NEXT. Edit it with the file tools when any of that changes; keep it short (a few hundred words).

When to manage it:
- Don't edit for every turn. When the context grows (reminders say so), finish the unit of work in flight, then do ONE batched revision.
- Prefer surgical edits: shorten big tool outputs, dead ends and superseded notes in place, near the request they answer. Keep decisions, constraints, file paths, commands still needed, test results, open questions and the next step. Don't flatten everything into one summary unless asked.
- When you replace text, copy the facts forward: what was ruled out and why ("do not retry"), the exact commands/queries already tried, values marked VERIFIED or UNVERIFIED, and a NEXT line. A detailed replacement costs little; a vague one makes you redo the work.
- An edit makes everything after it be re-read, so compact early, stale regions together rather than many small edits.
- Don't read the whole file back: its text is already in your context. To edit, quote a short unique part of a block (edit_file) or match block headers in a script.

Nothing is ever lost: every state of the file is a revision. `{REVISIONS_PATH}` (read_file) lists them; `{REVISIONS_PATH}/12.md` is revision 12 (use start_line/end_line), `{REVISIONS_PATH}/12.md#7` is just its block 7. With run_command they are not files; use read_file.
- After you remove something, the revision before your edit still holds it.
- **If you notice that something you need is no longer in your context** (a file's content, an output, a decision), ask yourself whether you need a particular revision: check `{REVISIONS_PATH}` and read only the block you need, instead of redoing the work. Re-run a tool only when the result may have changed since.
- When the app sees you repeat a call whose earlier result you removed, it tells you which revision and block still hold it.
- To put a whole earlier state back, don't copy it through your context: `restore_context` with a revision number makes that revision your file again (with the messages since appended), and `restore_context` with "stored" drops your edits so the chat is sent as stored. Use it when an edit removed too much; it applies to your next request.

Other files in `{real_dir}` are yours too (scratch notes, offloaded data): `@context/<name>` with the file tools; `list_dir @context` lists the folder.{card}"#
    )
}

/// The task card was deleted or emptied: its last text, so it isn't lost.
pub fn card_gone(last: &str) -> String {
    format!(
        "[context: your task card {TASK_PATH} was deleted or emptied. If the task isn't finished, write it again \
         (only delete it when the task is done). Its last text was:\n{last}]"
    )
}

/// The size readout at the end of every request.
pub fn readout(tokens: usize, budget: usize, revision: Option<u64>) -> String {
    let pct = percent(tokens, budget);
    let rev = revision
        .map(|r| format!(", revision {r}"))
        .unwrap_or_default();
    format!("[context: ~{tokens}/{budget} tokens ({pct}%){rev}]")
}

pub fn percent(tokens: usize, budget: usize) -> u32 {
    if budget == 0 {
        return 0;
    }
    ((tokens as f64 / budget as f64) * 100.0).round() as u32
}

/// Reminder tiers (percent of the budget). Each fires once and re-arms when
/// the context shrinks below it.
pub const TIERS: [u32; 3] = [25, 50, 75];
/// From here on, a reminder on every request.
pub const URGENT: u32 = 90;

const NOTE_CONTRACT: &str = "When you write a replacement note, copy facts forward from the text you replace: \
every ruled-out option with its reason and \"do not retry\"; the exact commands/queries already tried; \
exact values marked VERIFIED or UNVERIFIED; and a NEXT line.";

/// The reminder for this request, if any; updates the fired tiers.
pub fn nudge(tokens: usize, budget: usize, fired: &mut Vec<u32>) -> Option<String> {
    let pct = percent(tokens, budget);
    fired.retain(|t| pct >= *t);
    if pct >= URGENT {
        return Some(format!(
            "CONTEXT BUDGET (URGENT): ~{tokens}/{budget} tokens, {pct}% of your budget. Compact your context \
             file THIS TURN, before anything else: shorten or delete stale outputs and finished work. Past the \
             budget, the oldest tool outputs are withheld and then older history is cut. {NOTE_CONTRACT}"
        ));
    }
    let tier = TIERS
        .iter()
        .rev()
        .find(|t| pct >= **t && !fired.contains(t))
        .copied()?;
    let newly: Vec<u32> = TIERS
        .iter()
        .copied()
        .filter(|t| *t <= tier && !fired.contains(t))
        .collect();
    fired.extend(newly);
    Some(match tier {
        25 => format!(
            "CONTEXT BUDGET: ~{pct}% of your {budget}-token budget ({tokens} tokens). No action needed. \
             Keep track of what you've already tried."
        ),
        50 => format!(
            "CONTEXT BUDGET: ~{pct}% of your {budget}-token budget ({tokens} tokens). Finish the unit of work \
             in flight, then tidy your context file ONCE. {NOTE_CONTRACT}"
        ),
        _ => format!(
            "CONTEXT BUDGET: ~{pct}% of your {budget}-token budget ({tokens} tokens). Compact settled spans of \
             your context file now: summarize finished investigations and big outputs in place. Don't wipe \
             regions you still need: edits that keep very little of a region are usually followed by redoing \
             the deleted work. {NOTE_CONTRACT}"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_fire_once_and_rearm() {
        let mut fired = Vec::new();
        assert!(nudge(10, 100, &mut fired).is_none());
        assert!(nudge(30, 100, &mut fired).unwrap().contains("No action"));
        assert!(nudge(35, 100, &mut fired).is_none());
        // Jumping past two tiers gives only the higher one.
        assert!(nudge(80, 100, &mut fired)
            .unwrap()
            .contains("Compact settled"));
        assert!(nudge(60, 100, &mut fired).is_none());
        assert_eq!(fired, vec![25, 50]);
        assert!(nudge(20, 100, &mut fired).is_none());
        assert!(fired.is_empty(), "re-armed after shrinking");
        assert!(nudge(95, 100, &mut fired).unwrap().contains("URGENT"));
        assert!(
            nudge(95, 100, &mut fired).unwrap().contains("URGENT"),
            "urgent repeats"
        );
    }

    #[test]
    fn edges_of_the_reminder_maths() {
        assert_eq!(percent(0, 0), 0, "no budget known");
        assert_eq!(percent(250, 1000), 25);
        assert_eq!(percent(2000, 1000), 200);
        let mut fired = Vec::new();
        assert!(nudge(5, 0, &mut fired).is_none(), "no budget, no reminder");
        assert!(
            nudge(250, 1000, &mut fired).unwrap().contains("No action"),
            "exactly at a tier fires"
        );
        assert!(
            nudge(1500, 1000, &mut fired).unwrap().contains("URGENT"),
            "over the budget"
        );
        assert_eq!(
            readout(512, 1000, Some(3)),
            "[context: ~512/1000 tokens (51%), revision 3]"
        );
        assert_eq!(readout(5, 1000, None), "[context: ~5/1000 tokens (1%)]");
    }

    #[test]
    fn the_system_section_names_both_paths_and_the_revisions() {
        let text = system_section(
            "/mnt/v/context/c/LIVE_CONTEXT.md",
            "/mnt/v/context/c",
            32_000,
            Some(7),
            Some("Goal: fix the parser"),
        );
        assert!(text.contains("@context/LIVE_CONTEXT.md"));
        assert!(text.contains("`/mnt/v/context/c/LIVE_CONTEXT.md`"));
        assert!(text.contains("now revision 7"));
        assert!(text.contains("32000 tokens"));
        assert!(text.contains("@context/revisions/12.md#7"));
        assert!(text.contains("Goal: fix the parser"));
        assert!(text.contains("protected"));
        let bare = system_section("/f", "/d", 1, None, None);
        assert!(!bare.contains("now revision"));
        assert!(bare.contains("You have none yet"));
    }
}
