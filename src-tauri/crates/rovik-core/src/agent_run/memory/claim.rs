//! What a run's finding is, said as a claim: a failure's signature, the repair,
//! what it applies to and what validated it — and a cause only when something
//! actually showed one.
//!
//! Kept apart from the store so the shape of a claim is a pure function of what
//! the run established: the same evidence always yields the same text, the same
//! identity and therefore no duplicate, which is what makes rewriting a run's
//! memory idempotent.

/// A normalized failure signature: the command, and the keys it failed on.
///
/// Two different failures of the same command — `npm test` failing on
/// `checkout.rs` one day and `queue.rs` the next — have different signatures,
/// so their lessons stay distinct; the same failure, whose keys are stable
/// across runs (`command_output::failure_keys`), keeps one identity however
/// often it is repaired.
#[must_use]
pub(super) fn failure_signature(command: &str, keys: &[String]) -> String {
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut keys: Vec<String> = keys
        .iter()
        .map(|key| key.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|key| !key.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    format!("{command}|{}", keys.join(";"))
}

/// A repair, as the memory says it: what failed, what was done about it, where
/// it applies, and what showed that it worked.
///
/// A cause is [`Option`] on purpose. A run knows the failure and its own change;
/// it does not know *why* the code was broken unless something it read said so,
/// and a remembered cause that was guessed is worse than a remembered gap — the
/// next run would trust it. So the cause is written only when a caller has one
/// to give, and its absence is stated rather than filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Repair {
    /// One line: the failure this is about.
    pub title: String,
    /// What failed: the command and the keys it failed on.
    pub problem: String,
    /// Why, when something established it.
    pub cause: Option<String>,
    /// What was changed, and where.
    pub fix: String,
    /// The files this applies to.
    pub applies_to: Vec<String>,
    /// What showed it worked.
    pub validated_by: String,
}

impl Repair {
    /// The repair of a failed check: what failed on, what the run changed, and
    /// the checks that passed after.
    pub(super) fn of(
        command: &str,
        keys: &[String],
        changed: &[String],
        validated_by: String,
    ) -> Repair {
        let mut problem = format!("`{}` failed", brief(command, 80));
        if !keys.is_empty() {
            let shown: Vec<String> = keys.iter().take(3).cloned().collect();
            let more = keys.len().saturating_sub(shown.len());
            problem.push_str(&format!(" on: {}", shown.join("; ")));
            if more > 0 {
                problem.push_str(&format!(" (and {more} more)"));
            }
        }
        Repair {
            title: format!("Making `{}` pass", brief(command, TITLE_CHARS)),
            problem,
            cause: None,
            fix: if changed.is_empty() {
                "no files were changed".to_string()
            } else {
                format!("changed {}", changed.join(", "))
            },
            applies_to: changed.to_vec(),
            validated_by,
        }
    }

    /// The claim as a memory says it: the sections a later run needs, in the
    /// order it needs them.
    #[must_use]
    pub(super) fn render(&self) -> String {
        let mut text = format!("Problem: {}\n", self.problem);
        match self.cause.as_deref() {
            Some(cause) => text.push_str(&format!("Cause: {cause}\n")),
            // Said, not omitted: a reader has to know whether the cause was
            // established and found unimportant, or never established at all.
            None => text.push_str("Cause: not established by this run — check the code before relying on a cause.\n"),
        }
        text.push_str(&format!("Fix: {}\n", self.fix));
        if !self.applies_to.is_empty() {
            text.push_str(&format!("Applies to: {}\n", self.applies_to.join(", ")));
        }
        text.push_str(&format!("Validated by: {}\n", self.validated_by));
        text
    }

    /// The same claim, said to be about `scope` rather than about everything the
    /// run touched.
    ///
    /// A repair applies to the files the failure points at. A run that also
    /// tidied a README did not repair a different failure, so what it edited
    /// elsewhere must not change the claim's identity — a later repair of the
    /// same failure in the same place has to find this record and revise it.
    #[must_use]
    pub(super) fn scoped_to(mut self, scope: &[String]) -> Repair {
        if !scope.is_empty() {
            self.applies_to = scope.to_vec();
        }
        self
    }

    /// What this claim *is* within the project: the failure's signature and the
    /// files it applies to. A later repair of the same failure in the same place
    /// revises this one; a different failure, or the same one elsewhere, is a
    /// claim of its own.
    #[must_use]
    pub(super) fn identity(&self, signature: &str) -> String {
        let mut scope: Vec<String> = self
            .applies_to
            .iter()
            .map(|file| file.replace('\\', "/"))
            .collect();
        scope.sort_unstable();
        scope.dedup();
        format!("repair:{signature}|{}", scope.join(","))
    }
}

/// A record's title is one line; a command can be long.
fn brief(text: &str, chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = line.chars().take(chars).collect();
    if cut.chars().count() < line.chars().count() {
        cut.push('…');
    }
    cut
}

/// How much of a command a lesson's title carries.
const TITLE_CHARS: usize = 60;
