//! Rovik's tools mode, the same for every harness (connectors/mod.rs): a
//! harness that runs Rovik's tools over MCP (connectors/mcp.rs) is the
//! model behind Rovik's agent loop. When one of its messages calls Rovik's
//! tools, Rovik's turn ends with those calls as the reply's, while the
//! harness waits, its calls held open on the MCP server. Rovik runs them
//! and stores the results; its next turn finds the run waiting
//! ([`LiveRuns`]), gives each call its stored result ([`Matcher`]), and
//! reads on to the next message.
//!
//! However Rovik's run ends (done, stopped, paused, failed), the waiting
//! run goes with it (`end_live`, from the run's `Drop`).

use super::mcp::{McpCall, McpReply};
use super::Answers;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use tokio::sync::oneshot;

/// A connector's runs waiting for Rovik's results, by chat. Each connector
/// keeps its own, in a static.
pub(super) struct LiveRuns<T> {
    runs: Mutex<Option<HashMap<String, T>>>,
}

impl<T> LiveRuns<T> {
    /// None waiting yet: a connector's static.
    pub(super) const fn new() -> Self {
        LiveRuns {
            runs: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<HashMap<String, T>>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The chat's waiting run, taken to go on with it.
    pub(super) fn take(&self, dialog_id: &str) -> Option<T> {
        self.lock().as_mut()?.remove(dialog_id)
    }

    /// Keeps the chat's run, waiting for Rovik's results.
    pub(super) fn keep(&self, dialog_id: String, run: T) {
        self.lock()
            .get_or_insert_with(HashMap::new)
            .insert(dialog_id, run);
    }

    /// Ends the chat's waiting run, if any: Rovik's run is over. The run is
    /// dropped outside the lock.
    pub(super) fn end(&self, dialog_id: &str) {
        let ended = self.take(dialog_id);
        drop(ended);
    }

    /// Whether the chat has a run waiting (tests).
    #[cfg(test)]
    pub(super) fn has(&self, dialog_id: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|runs| runs.contains_key(dialog_id))
    }

    /// Ends every waiting run: the process is exiting.
    pub(super) fn end_all(&self) {
        let all: Vec<T> = self
            .lock()
            .as_mut()
            .map(|runs| runs.drain().map(|(_, run)| run).collect())
            .unwrap_or_default();
        drop(all);
    }
}

/// Matches a harness's MCP calls to the tool calls Rovik ran, and answers
/// each with its result once both are there.
#[derive(Default)]
pub(super) struct Matcher {
    /// The calls handed to Rovik at the last yield: id, name, arguments.
    pub(super) pending: Vec<(String, String, Value)>,
    /// Calls made, waiting for their result, by tool use id.
    waiting: HashMap<String, oneshot::Sender<McpReply>>,
    /// Results given, waiting for their call, by tool use id.
    answers: HashMap<String, McpReply>,
    /// Ids already matched to a call that named none.
    pub(super) matched: HashSet<String>,
}

impl Matcher {
    /// A call the harness made: answered if its result is there, else kept.
    pub(super) fn take(&mut self, call: McpCall) {
        let id = call.tool_use_id.clone().or_else(|| {
            // A version that doesn't say the id: the first pending call of
            // that name and those arguments.
            let found = self
                .pending
                .iter()
                .find(|(id, name, arguments)| {
                    *name == call.name && *arguments == call.arguments && !self.matched.contains(id)
                })
                .map(|(id, ..)| id.clone());
            if let Some(id) = &found {
                self.matched.insert(id.clone());
            }
            found
        });
        let Some(id) = id else {
            let _ = call
                .reply
                .send(McpReply::error("Rovik has no call like this to answer."));
            return;
        };
        match self.answers.remove(&id) {
            Some(answer) => {
                let _ = call.reply.send(answer);
            }
            None => {
                self.waiting.insert(id, call.reply);
            }
        }
    }

    /// Rovik's results for the pending calls, answering those made.
    pub(super) fn deliver(&mut self, answers: &Answers) {
        let count = self.pending.len();
        for (n, (id, ..)) in self.pending.iter().enumerate() {
            let mut text = answers
                .results
                .get(id)
                .cloned()
                .unwrap_or_else(|| "[Rovik didn't run this call.]".into());
            // What Rovik said after the results (a check's failure, a
            // message the user sent) goes with the last.
            if n + 1 == count && !answers.notes.trim().is_empty() {
                text = format!("{text}\n\n---\n{}", answers.notes.trim());
            }
            let answer = McpReply::text(text);
            match self.waiting.remove(id) {
                Some(reply) => {
                    let _ = reply.send(answer);
                }
                None => {
                    self.answers.insert(id.clone(), answer);
                }
            }
        }
    }

    /// The calls a yield handed to Rovik, which its next turn answers.
    pub(super) fn await_answers(&mut self, made: Vec<(String, String, String)>) {
        self.pending = made
            .into_iter()
            .map(|(id, name, arguments)| {
                let arguments = serde_json::from_str(&arguments).unwrap_or(Value::Null);
                (id, name, arguments)
            })
            .collect();
        self.matched.clear();
    }

    /// Whether Rovik's turn goes on with the run this matches for: the
    /// chat's last message is the one with the calls it's waiting on.
    pub(super) fn takes(&self, answers: Option<&Answers>) -> bool {
        let Some(answers) = answers else {
            return false;
        };
        !self.pending.is_empty()
            && self
                .pending
                .iter()
                .all(|(id, ..)| answers.calls.iter().any(|(call, _)| call == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn calls_are_matched_by_their_tool_use_or_their_arguments() {
        let mut m = Matcher {
            pending: vec![
                ("t1".into(), "read".into(), json!({ "path": "a" })),
                ("t2".into(), "read".into(), json!({ "path": "b" })),
            ],
            ..Matcher::default()
        };
        let call = |id: Option<&str>, path: &str| {
            let (reply, rx) = oneshot::channel();
            (
                McpCall {
                    tool_use_id: id.map(str::to_string),
                    name: "read".into(),
                    arguments: json!({ "path": path }),
                    reply,
                },
                rx,
            )
        };
        // Made before the results: waits.
        let (c1, mut r1) = call(Some("t1"), "a");
        m.take(c1);
        assert!(r1.try_recv().is_err());
        let answers = Answers {
            calls: vec![("t1".into(), "read".into()), ("t2".into(), "read".into())],
            results: HashMap::from([
                ("t1".to_string(), "A".to_string()),
                ("t2".to_string(), "B".to_string()),
            ]),
            notes: "Checks failed.".into(),
        };
        assert!(m.takes(Some(&answers)));
        m.deliver(&answers);
        assert_eq!(r1.try_recv().unwrap(), McpReply::text("A"));
        // Made after, naming no id: matched by its arguments; the notes go with the last.
        let (c2, mut r2) = call(None, "b");
        m.take(c2);
        assert_eq!(
            r2.try_recv().unwrap(),
            McpReply::text("B\n\n---\nChecks failed.")
        );
        // A call nothing matches is an error.
        let (c3, mut r3) = call(None, "zzz");
        m.take(c3);
        assert!(r3.try_recv().unwrap().is_error);
    }

    #[test]
    fn a_run_goes_on_only_with_the_answers_to_its_own_calls() {
        let mut m = Matcher::default();
        m.await_answers(vec![("t1".into(), "read".into(), r#"{"path":"a"}"#.into())]);
        assert_eq!(m.pending[0].2, json!({ "path": "a" }));
        let other = Answers {
            calls: vec![("t9".into(), "read".into())],
            ..Answers::default()
        };
        assert!(!m.takes(Some(&other)), "another reply's calls");
        assert!(!m.takes(None));
        let own = Answers {
            calls: vec![("t1".into(), "read".into())],
            ..Answers::default()
        };
        assert!(m.takes(Some(&own)));
    }

    #[test]
    fn runs_are_kept_by_chat_and_ended_one_or_all() {
        let runs: LiveRuns<String> = LiveRuns::new();
        assert_eq!(runs.take("d1"), None);
        runs.keep("d1".into(), "one".into());
        runs.keep("d2".into(), "two".into());
        assert_eq!(runs.take("d1").as_deref(), Some("one"));
        assert_eq!(runs.take("d1"), None, "taken");
        runs.keep("d1".into(), "again".into());
        runs.end("d1");
        assert_eq!(runs.take("d1"), None);
        assert!(runs.has("d2"));
        runs.end_all();
        assert!(!runs.has("d2"));
    }
}
