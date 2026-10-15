//! Keeps one agent run (a user message and every tool call that follows)
//! from going on forever, without capping legitimate long tasks.
//!
//! Two mechanisms:
//!   - Checkpoints: every N tool calls (a setting; "never" allowed) the run
//!     pauses and the user decides whether it goes on. Pausing is not an
//!     error: the chat shows Continue, and the run picks up where it was.
//!   - Loop detection, independent of the count: the same call returning the
//!     same thing again and again, two calls taking turns with the same results
//!     each time, a streak of failures, the same error over and over, or an
//!     edit putting a file back the way it was. The first time a pattern fires
//!     the model gets a note in the tool result (models often recover when
//!     told); the same pattern again pauses the run with the reason. A call
//!     the user (or their settings, or their AI reviewer) rejected, made again,
//!     pauses it at once: the rejection already said not to retry.
//!
//! Every pattern needs identical results, never just similar calls: reading
//! ten different files, or polling a job whose output changes, isn't a loop.
//!
//! Pure bookkeeping, no I/O: the run feeds it each finished tool call.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

/// How a loop warning in a tool result starts, and what follows its reason.
const NOTE_START: &str = "[Note from the IDE: ";
const LOOP_MARK: &str = ". This looks like a loop.";

/// The reason of the loop warning in a tool result, if it has one (the
/// focus note names it again a few calls later; chat_db::history).
pub fn loop_warning(result: &str) -> Option<&str> {
    let end = result.rfind(LOOP_MARK)?;
    let start = result[..end].rfind(NOTE_START)? + NOTE_START.len();
    Some(&result[start..end])
}

/// Same call, same arguments, same result: this many times is a loop.
const IDENTICAL_LIMIT: usize = 3;
/// Two calls taking turns (A B A B A B), each with the same result every
/// time: this many rounds is a loop.
const ALTERNATION_ROUNDS: usize = 3;
/// ...within this many most recent calls.
const IDENTICAL_WINDOW: usize = 30;
/// Failed calls in a row.
const ERROR_STREAK_LIMIT: usize = 5;
/// The same error text this many times...
const SAME_ERROR_LIMIT: usize = 3;
/// ...within this many most recent calls.
const SAME_ERROR_WINDOW: usize = 15;
/// Earlier states remembered per edited file.
const FILE_STATES_KEPT: usize = 50;

pub struct FinishedCall<'a> {
    pub name: &'a str,
    pub args_json: &'a str,
    /// The result text the model gets.
    pub result: &'a str,
    /// A successful file write: the file before (None when it didn't exist) and after.
    pub write: Option<Write<'a>>,
    /// Rejected by the user, their AI reviewer or their permission settings
    /// (not run).
    pub rejected: bool,
}

pub struct Write<'a> {
    pub path: &'a str,
    pub before: Option<&'a str>,
    pub after: &'a str,
}

#[derive(Debug, PartialEq)]
pub enum GuardVerdict {
    Ok,
    /// Add `note` to this call's result: the model is warned.
    Nudge(String),
    /// Add `note` to the result; the run pauses after this turn's calls.
    Stop(String),
}

impl GuardVerdict {
    pub fn note(&self) -> Option<&str> {
        match self {
            GuardVerdict::Ok => None,
            GuardVerdict::Nudge(n) | GuardVerdict::Stop(n) => Some(n),
        }
    }
}

/// Stored in a message's meta where a run paused (`meta.paused`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseInfo {
    /// "checkpoint" or "loop"; "move" for a chat moving to or from a host
    /// (6.8), "drain" for an upgrade of the server's Rovik (R02).
    pub kind: &'static str,
    /// Tool calls run so far in this run, Continues included.
    pub tool_calls: u32,
    /// Why the run paused, which the model is asked to sum up first (a
    /// loop's). A move or a drain has none, so it asks nothing (K29).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The host a chat moves to, which the transcript names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

pub struct Guard {
    /// Tool calls in this run so far, counting those before a Continue.
    pub tool_calls: u32,
    next_checkpoint: Option<u32>,
    recent_calls: VecDeque<(String, String, u64)>,
    recent_errors: VecDeque<String>,
    error_streak: usize,
    file_states: HashMap<String, Vec<u64>>,
    /// The latest calls as (call and result, description), for alternation.
    /// Writes don't clear it: the results tell whether anything changed.
    sequence: VecDeque<(u64, String)>,
    /// Calls rejected in this run (name, arguments).
    rejected: Vec<(String, String)>,
    /// Patterns the model was already warned about in this run.
    nudged: HashSet<&'static str>,
    stop_reason: Option<String>,
    /// The pattern behind the latest Nudge or Stop, and its reason (logs).
    pub finding: Option<(&'static str, String)>,
}

impl Guard {
    /// `checkpoint_every`: pause after every this many tool calls (None: never).
    /// `start_at`: tool calls already made (continuing a paused run).
    pub fn new(checkpoint_every: Option<u32>, start_at: u32) -> Self {
        Guard {
            tool_calls: start_at,
            next_checkpoint: checkpoint_every
                .filter(|n| *n > 0)
                .map(|n| (start_at / n + 1) * n),
            recent_calls: VecDeque::new(),
            recent_errors: VecDeque::new(),
            error_streak: 0,
            file_states: HashMap::new(),
            sequence: VecDeque::new(),
            rejected: Vec::new(),
            nudged: HashSet::new(),
            stop_reason: None,
            finding: None,
        }
    }

    /// Records one finished tool call.
    pub fn record(&mut self, call: FinishedCall) -> GuardVerdict {
        self.tool_calls += 1;
        let Some((pattern, reason)) = self.detect(&call) else {
            return GuardVerdict::Ok;
        };
        self.finding = Some((pattern, reason.clone()));

        // A rejected call made again was already told not to retry: no warning.
        if pattern != "rejectedAgain" && self.nudged.insert(pattern) {
            // Fresh evidence is needed for the next verdict, so a model that does
            // change course isn't paused by what it did before the note.
            self.reset_evidence();
            return GuardVerdict::Nudge(format!(
                "\n\n{NOTE_START}{reason}{LOOP_MARK} Step back before going on: \
                 re-read the relevant code, try a different approach, or stop and ask the user. \
                 If it happens again, the run is paused for the user.]"
            ));
        }
        let note = format!(
            "\n\n[Note from the IDE: the run was paused here for the user, because {reason}.]"
        );
        self.stop_reason = Some(reason);
        GuardVerdict::Stop(note)
    }

    /// Whether to pause before the next request to the model, and why.
    pub fn pause(&self) -> Option<PauseInfo> {
        if let Some(reason) = &self.stop_reason {
            return Some(PauseInfo {
                kind: "loop",
                tool_calls: self.tool_calls,
                reason: Some(reason.clone()),
                host: None,
            });
        }
        match self.next_checkpoint {
            Some(at) if self.tool_calls >= at => Some(PauseInfo {
                kind: "checkpoint",
                tool_calls: self.tool_calls,
                reason: None,
                host: None,
            }),
            _ => None,
        }
    }

    /// The pattern a call completes, by name ("rejectedAgain", "revert",
    /// "identical", "alternating", "errorStreak", "sameError"), and the
    /// reason the model is told.
    fn detect(&mut self, call: &FinishedCall) -> Option<(&'static str, String)> {
        let failed = call.result.starts_with("Error:");

        if call.rejected {
            let key = (call.name.to_string(), canonical_args(call.args_json));
            if self.rejected.contains(&key) {
                return Some((
                    "rejectedAgain",
                    format!(
                        "{} was made again after it was rejected",
                        describe_call(call)
                    ),
                ));
            }
            self.rejected.push(key);
        }
        // Every call goes into the sequence, whichever check fires first.
        let alternation = self.record_in_sequence(call);

        // An edit that went through changes what reads and commands return:
        // repeating them afterwards is expected, not a loop.
        if let Some(write) = &call.write {
            self.recent_calls.clear();
            if let Some(revert) = self.record_write(write) {
                return Some(("revert", revert));
            }
        }

        // 1. The same call returning the same result again.
        let key = (
            call.name.to_string(),
            canonical_args(call.args_json),
            hash(call.result),
        );
        self.recent_calls.push_back(key.clone());
        if self.recent_calls.len() > IDENTICAL_WINDOW {
            self.recent_calls.pop_front();
        }
        let repeats = self.recent_calls.iter().filter(|k| **k == key).count();
        if call.write.is_none() && repeats >= IDENTICAL_LIMIT {
            return Some((
                "identical",
                format!(
                    "{} was called {repeats} times with the same arguments and returned the same result each time",
                    describe_call(call)
                ),
            ));
        }

        // 2. Two calls taking turns, the same results each time.
        if let Some(reason) = alternation {
            return Some(("alternating", reason));
        }

        // 3. Failures in a row.
        self.error_streak = if failed { self.error_streak + 1 } else { 0 };
        if self.error_streak >= ERROR_STREAK_LIMIT {
            return Some((
                "errorStreak",
                format!("the last {} tool calls all failed", self.error_streak),
            ));
        }

        // 4. The same failure again, whatever the arguments.
        let error = if failed {
            call.result.trim().to_string()
        } else {
            String::new()
        };
        self.recent_errors.push_back(error.clone());
        if self.recent_errors.len() > SAME_ERROR_WINDOW {
            self.recent_errors.pop_front();
        }
        if failed {
            let same = self.recent_errors.iter().filter(|e| **e == error).count();
            if same >= SAME_ERROR_LIMIT {
                let first = first_line(&error, 160);
                return Some((
                    "sameError",
                    format!(
                        "the same error came back {same} times: \"{}\"",
                        first.strip_suffix('.').unwrap_or(&first)
                    ),
                ));
            }
        }
        None
    }

    /// A write that puts the file back into a state it had earlier in the run.
    fn record_write(&mut self, write: &Write) -> Option<String> {
        let states = self.file_states.entry(write.path.to_string()).or_default();
        if states.is_empty() {
            if let Some(before) = write.before {
                states.push(hash(before));
            }
        }
        let after = hash(write.after);
        let back_to_earlier = states.last() != Some(&after) && states.contains(&after);
        states.push(after);
        if states.len() > FILE_STATES_KEPT {
            states.remove(0);
        }
        back_to_earlier.then(|| {
            format!(
                "this change put {} back exactly the way it was earlier in this run, undoing the changes in between",
                write.path
            )
        })
    }

    /// Adds the call to the sequence; the reason when the latest calls are
    /// two different ones taking turns, each returning what it did before.
    fn record_in_sequence(&mut self, call: &FinishedCall) -> Option<String> {
        let key = hash(&format!(
            "{}\u{0}{}\u{0}{}",
            call.name,
            canonical_args(call.args_json),
            call.result
        ));
        self.sequence.push_back((key, describe_call(call)));
        let span = 2 * ALTERNATION_ROUNDS;
        if self.sequence.len() > span {
            self.sequence.pop_front();
        }
        if self.sequence.len() < span {
            return None;
        }
        let (a, b) = (&self.sequence[0], &self.sequence[1]);
        let alternates = a.0 != b.0
            && self
                .sequence
                .iter()
                .enumerate()
                .all(|(i, s)| s.0 == if i % 2 == 0 { a.0 } else { b.0 });
        alternates.then(|| {
            format!(
                "{} and {} took turns {ALTERNATION_ROUNDS} times, each returning the same result every time",
                a.1, b.1
            )
        })
    }

    fn reset_evidence(&mut self) {
        self.sequence.clear();
        self.recent_calls.clear();
        self.recent_errors.clear();
        self.error_streak = 0;
        self.file_states.clear();
    }
}

/// Arguments with keys sorted, so {"a":1,"b":2} and {"b":2,"a":1} are the same call.
fn canonical_args(args_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(value) => sorted(&value),
        Err(_) => args_json.trim().to_string(),
    }
}

fn sorted(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => format!(
            "[{}]",
            items.iter().map(sorted).collect::<Vec<_>>().join(",")
        ),
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(k.clone()),
                        sorted(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        other => other.to_string(),
    }
}

/// "read (src/App.tsx)", "bash (npm test)": for the notes.
fn describe_call(call: &FinishedCall) -> String {
    let args = super::permissions::parse_args(call.args_json);
    let detail = ["path", "command", "query", "url"]
        .iter()
        .filter_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .find(|v| !v.trim().is_empty());
    match detail {
        Some(d) => format!("{} ({})", call.name, first_line(d, 80)),
        None => call.name.to_string(),
    }
}

pub fn first_line(text: &str, max: usize) -> String {
    let line = text.split('\n').next().unwrap_or("");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Results and file contents aren't kept whole, only their hashes.
fn hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<'a>(name: &'a str, args: &'a str, result: &'a str) -> FinishedCall<'a> {
        FinishedCall {
            name,
            args_json: args,
            result,
            write: None,
            rejected: false,
        }
    }

    #[test]
    fn identical_calls_nudge_then_stop() {
        let mut g = Guard::new(None, 0);
        assert_eq!(
            g.record(call("read", r#"{"path":"a","x":1}"#, "same")),
            GuardVerdict::Ok
        );
        assert_eq!(
            g.record(call("read", r#"{"x":1,"path":"a"}"#, "same")),
            GuardVerdict::Ok
        );
        let v = g.record(call("read", r#"{"path":"a","x":1}"#, "same"));
        assert!(
            matches!(&v, GuardVerdict::Nudge(n) if n.contains("read (a) was called 3 times")),
            "{v:?}"
        );
        assert_eq!(g.finding.as_ref().map(|f| f.0), Some("identical"));
        assert!(g.pause().is_none());
        // Evidence was reset: two more are fine, the third stops.
        g.record(call("read", r#"{"path":"a"}"#, "same"));
        g.record(call("read", r#"{"path":"a"}"#, "same"));
        assert!(matches!(
            g.record(call("read", r#"{"path":"a"}"#, "same")),
            GuardVerdict::Stop(_)
        ));
        assert_eq!(g.pause().unwrap().kind, "loop");
    }

    #[test]
    fn the_warning_reason_can_be_read_back() {
        let mut g = Guard::new(None, 0);
        let mut result = String::from("file text");
        for _ in 0..3 {
            if let Some(note) = g.record(call("read", r#"{"path":"a"}"#, "same")).note() {
                result.push_str(note);
            }
        }
        assert_eq!(
            loop_warning(&result),
            Some("read (a) was called 3 times with the same arguments and returned the same result each time")
        );
        assert_eq!(
            loop_warning("file text [Note from the IDE: something else.]"),
            None
        );
        assert_eq!(loop_warning("This looks like a loop."), None);
    }

    #[test]
    fn two_calls_taking_turns_with_the_same_results() {
        // The agent writes a file, a formatter puts it back: each write
        // changes the file, so the identical check never sees the repeats.
        let mut g = Guard::new(None, 0);
        let write = |g: &mut Guard| {
            let w = Some(Write {
                path: "a.ts",
                before: Some("formatted"),
                after: "mine",
            });
            g.record(FinishedCall {
                name: "write",
                args_json: r#"{"path":"a.ts"}"#,
                result: "wrote 4 bytes",
                write: w,
                rejected: false,
            })
        };
        let format =
            |g: &mut Guard| g.record(call("bash", r#"{"command":"npm run fmt"}"#, "exit 0"));
        for _ in 0..2 {
            assert_eq!(write(&mut g), GuardVerdict::Ok);
            assert_eq!(format(&mut g), GuardVerdict::Ok);
        }
        assert_eq!(write(&mut g), GuardVerdict::Ok);
        let v = format(&mut g);
        assert!(
            matches!(&v, GuardVerdict::Nudge(n) if n.contains("write (a.ts) and bash (npm run fmt) took turns 3 times")),
            "{v:?}"
        );
        assert_eq!(g.finding.as_ref().map(|f| f.0), Some("alternating"));
    }

    #[test]
    fn taking_turns_needs_the_same_results() {
        let mut g = Guard::new(None, 0);
        // Polling a job whose output changes.
        for i in 0..10 {
            let (status, log) = (format!("running {i}%"), format!("line {i}"));
            assert_eq!(
                g.record(call("bash", r#"{"command":"status"}"#, &status)),
                GuardVerdict::Ok
            );
            assert_eq!(
                g.record(call("bash", r#"{"command":"tail log"}"#, &log)),
                GuardVerdict::Ok
            );
        }
        // Reading different files.
        let mut g = Guard::new(None, 0);
        for i in 0..6 {
            let args = format!(r#"{{"path":"f{i}"}}"#);
            assert_eq!(
                g.record(call("read", &args, &format!("text {i}"))),
                GuardVerdict::Ok,
                "read {i}"
            );
        }
        // A third call in between breaks the turn-taking.
        let mut g = Guard::new(None, 0);
        let calls = [
            ("a", "x"),
            ("b", "y"),
            ("a", "x"),
            ("c", "z"),
            ("b", "y"),
            ("d", "w"),
        ];
        for (name, result) in calls {
            assert_eq!(g.record(call(name, "{}", result)), GuardVerdict::Ok);
        }
    }

    #[test]
    fn each_pattern_is_warned_about_once() {
        let mut g = Guard::new(None, 0);
        for _ in 0..3 {
            g.record(call("read", r#"{"path":"a"}"#, "same"));
        }
        assert_eq!(g.finding.as_ref().map(|f| f.0), Some("identical"));
        // Another pattern: its own warning, not a pause.
        for i in 0..2 {
            assert_eq!(
                g.record(call("x", &i.to_string(), "Error: boom.")),
                GuardVerdict::Ok
            );
        }
        assert!(matches!(
            g.record(call("x", "3", "Error: boom.")),
            GuardVerdict::Nudge(_)
        ));
        assert!(g.pause().is_none());
        // The first pattern again: paused.
        for _ in 0..2 {
            assert_eq!(
                g.record(call("read", r#"{"path":"a"}"#, "same")),
                GuardVerdict::Ok
            );
        }
        assert!(matches!(
            g.record(call("read", r#"{"path":"a"}"#, "same")),
            GuardVerdict::Stop(_)
        ));
        assert!(g.pause().unwrap().reason.unwrap().contains("read (a)"));
    }

    #[test]
    fn a_rejected_call_made_again_pauses_at_once() {
        let mut g = Guard::new(None, 0);
        let rejected = |args: &'static str| FinishedCall {
            name: "bash",
            args_json: args,
            result: "Error: the user denied this \"bash\" call.",
            write: None,
            rejected: true,
        };
        assert_eq!(
            g.record(rejected(r#"{"command":"rm -rf build"}"#)),
            GuardVerdict::Ok
        );
        // Another rejected call is the user steering, not a loop.
        assert_eq!(
            g.record(rejected(r#"{"command":"git push"}"#)),
            GuardVerdict::Ok
        );
        let v = g.record(rejected(r#"{ "command": "rm -rf build" }"#));
        assert!(
            matches!(&v, GuardVerdict::Stop(n) if n.contains("bash (rm -rf build) was made again after it was rejected")),
            "{v:?}"
        );
        assert_eq!(g.finding.as_ref().map(|f| f.0), Some("rejectedAgain"));
        assert_eq!(g.pause().unwrap().kind, "loop");
        // The same call that merely failed twice is no rejection.
        let mut g = Guard::new(None, 0);
        g.record(call("bash", r#"{"command":"make"}"#, "Error: exit 2"));
        assert_eq!(
            g.record(call("bash", r#"{"command":"make"}"#, "Error: exit 2")),
            GuardVerdict::Ok
        );
    }

    #[test]
    fn error_streaks_and_same_errors() {
        let mut g = Guard::new(None, 0);
        for i in 0..2 {
            assert_eq!(
                g.record(call("x", &i.to_string(), "Error: boom.")),
                GuardVerdict::Ok
            );
        }
        let v = g.record(call("x", "3", "Error: boom."));
        assert!(
            matches!(&v, GuardVerdict::Nudge(n) if n.contains("the same error came back 3 times: \"Error: boom\""))
        );
        assert_eq!(g.finding.as_ref().map(|f| f.0), Some("sameError"));
    }

    #[test]
    fn reverting_a_file_is_noticed() {
        let mut g = Guard::new(None, 0);
        let w = |before, after| {
            Some(Write {
                path: "a.ts",
                before,
                after,
            })
        };
        let r = g.record(FinishedCall {
            name: "write",
            args_json: "{}",
            result: "ok",
            write: w(Some("v1"), "v2"),
            rejected: false,
        });
        assert_eq!(r, GuardVerdict::Ok);
        let r = g.record(FinishedCall {
            name: "write",
            args_json: "{}",
            result: "ok",
            write: w(Some("v2"), "v1"),
            rejected: false,
        });
        assert!(matches!(r, GuardVerdict::Nudge(n) if n.contains("put a.ts back")));
    }

    #[test]
    fn checkpoints_count_from_where_a_run_paused() {
        let mut g = Guard::new(Some(25), 25);
        for _ in 0..24 {
            g.record(call("read", &format!("{}", g.tool_calls), "r"));
        }
        assert!(g.pause().is_none());
        g.record(call("read", "last", "r"));
        assert_eq!(
            g.pause(),
            Some(PauseInfo {
                kind: "checkpoint",
                tool_calls: 50,
                reason: None,
                host: None,
            })
        );
    }

    #[test]
    fn different_errors_in_a_row_are_a_streak() {
        let mut g = Guard::new(None, 0);
        for i in 0..4 {
            let err = format!("Error: problem {i}");
            assert_eq!(g.record(call("x", &i.to_string(), &err)), GuardVerdict::Ok);
        }
        assert!(
            matches!(g.record(call("x", "9", "Error: other")), GuardVerdict::Nudge(n) if n.contains("last 5 tool calls all failed"))
        );
    }

    #[test]
    fn a_success_breaks_the_streak_and_unparseable_args_still_compare() {
        let mut g = Guard::new(None, 0);
        for i in 0..4 {
            g.record(call("x", &i.to_string(), &format!("Error: {i}")));
        }
        g.record(call("x", "ok", "fine"));
        assert_eq!(g.record(call("x", "5", "Error: 5")), GuardVerdict::Ok);
        let mut g = Guard::new(None, 0);
        g.record(call("x", "{broken", "r"));
        g.record(call("x", " {broken ", "r"));
        assert!(matches!(
            g.record(call("x", "{broken", "r")),
            GuardVerdict::Nudge(_)
        ));
    }

    #[test]
    fn writes_reset_repeated_reads_and_no_checkpoint_means_none() {
        let mut g = Guard::new(None, 0);
        g.record(call("read", "{}", "same"));
        g.record(call("read", "{}", "same"));
        let w = FinishedCall {
            name: "write",
            args_json: "{}",
            result: "ok",
            write: Some(Write {
                path: "a",
                before: None,
                after: "1",
            }),
            rejected: false,
        };
        g.record(w);
        // Reading again after a write is expected.
        assert_eq!(g.record(call("read", "{}", "same")), GuardVerdict::Ok);
        assert_eq!(g.record(call("read", "{}", "same")), GuardVerdict::Ok);
        for _ in 0..500 {
            g.record(call("x", &g.tool_calls.to_string(), "r"));
        }
        assert!(g.pause().is_none(), "no checkpoint set");
        assert!(Guard::new(Some(0), 0).pause().is_none());
    }
}
