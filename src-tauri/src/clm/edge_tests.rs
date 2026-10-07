//! Edge cases of CLM mode end to end: a real database and an unmounted
//! volume, driven the way requests and the agent's tools drive them.

use super::*;
use crate::chat_db::store::{
    append_messages, create_dialog, delete_dialog, NewDialog, NewMessage, ToolCall,
};
use std::path::PathBuf;

const COST: Cost = Cost {
    vision: false,
    chars_per_token: 3.0,
};
const ROOMY: usize = 100_000;

fn message(id: &str, role: &str, content: &str) -> NewMessage {
    NewMessage {
        id: id.into(),
        role: role.into(),
        content: content.into(),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: vec![],
        created_at: store::now_ms(),
        finalized: true,
    }
}

/// An assistant message with one tool call, and its result.
fn tool_pair(n: u32, name: &str, args: &str, output: &str) -> Vec<NewMessage> {
    vec![
        NewMessage {
            tool_calls: Some(vec![ToolCall {
                id: format!("c{n}"),
                name: name.into(),
                arguments: args.into(),
                extra_content: None,
            }]),
            ..message(&format!("a{n}"), "assistant", "")
        },
        NewMessage {
            tool_call_id: Some(format!("c{n}")),
            ..message(&format!("t{n}"), "tool", output)
        },
    ]
}

/// A chat ("chat", or another id) with its database and volume.
struct Chat {
    db: DbState,
    vfs: VfsState,
    dir: PathBuf,
    id: String,
}

impl Chat {
    async fn new() -> Chat {
        let dir = std::env::temp_dir().join(format!("clm-edge-{}", uuid::Uuid::new_v4()));
        let db = DbState::open(&dir).unwrap();
        let chat = Chat {
            db,
            vfs: VfsState::default(),
            dir,
            id: "chat".into(),
        };
        chat.create("chat").await;
        chat
    }

    async fn create(&self, id: &str) {
        let id = id.to_string();
        self.db
            .write(move |conn| {
                create_dialog(
                    conn,
                    &NewDialog {
                        id,
                        title: "t".into(),
                        created_at: 1,
                        updated_at: 1,
                        project_id: None,
                    },
                )
            })
            .await
            .unwrap();
    }

    async fn add(&self, messages: Vec<NewMessage>) {
        let id = self.id.clone();
        self.db
            .write(move |conn| append_messages(conn, &id, &messages).map(|_| ()))
            .await
            .unwrap();
    }

    async fn prepare(&self, budget: usize) -> Prepared {
        prepare(&self.db, &self.vfs, self.id.clone(), budget, COST, None)
            .await
            .unwrap()
    }

    fn path(&self) -> String {
        context_file(&self.id)
    }

    fn file(&self) -> String {
        String::from_utf8(self.vfs.read_file(&self.path()).unwrap()).unwrap()
    }

    /// The agent (or a program) writes the context file.
    fn write(&self, text: &str) {
        self.vfs
            .write_unmounted(&self.path(), text.as_bytes())
            .unwrap();
    }

    async fn state(&self) -> store::State {
        let id = self.id.clone();
        self.db
            .read(move |conn| store::load_state(conn, &id))
            .await
            .unwrap()
    }

    async fn revisions(&self) -> Vec<(u64, String)> {
        let id = self.id.clone();
        self.db
            .read(move |conn| store::revisions(conn, &id))
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.number, r.kind))
            .collect()
    }

    async fn delete_message(&self, id: &str) {
        let id = id.to_string();
        self.db
            .write(move |conn| {
                conn.execute("DELETE FROM messages WHERE id = ?1", [id])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
    }

    async fn hint(&self, call_id: &str, name: &str, args: &str) -> Option<String> {
        let (id, call_id, name, args) = (
            self.id.clone(),
            call_id.to_string(),
            name.to_string(),
            args.to_string(),
        );
        self.db
            .read(move |conn| recall_hint(conn, &id, &call_id, &name, &args))
            .await
            .unwrap()
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    tauri::async_runtime::block_on(f)
}

fn ids(p: &Prepared) -> Vec<&str> {
    p.messages.iter().map(|m| m.id.as_str()).collect()
}

fn notes(p: &Prepared) -> String {
    p.notes.join("\n")
}

/// The text of block `turn` in the current file, and the file with it replaced.
fn with_block(doc: &str, turn: usize, body: &str) -> String {
    let start = doc.find(&format!("[[CTX_TURN {turn} ")).unwrap();
    let header_end = start + doc[start..].find('\n').unwrap() + 1;
    let end = doc[header_end..]
        .find("\n[[CTX_TURN ")
        .map_or(doc.len(), |i| header_end + i);
    format!("{}{body}{}", &doc[..header_end], &doc[end..])
}

/*
 * Requests without edits
 */

#[test]
fn an_empty_chat_renders_an_empty_document() {
    run(async {
        let chat = Chat::new().await;
        let p = chat.prepare(ROOMY).await;
        assert!(p.messages.is_empty());
        assert!(chat
            .file()
            .starts_with("[[LIVE_CONTEXT revision=1 turns=0]]"));
        chat.add(vec![message("u1", "user", "hi")]).await;
        let p = chat.prepare(ROOMY).await;
        assert_eq!(ids(&p), ["u1"]);
        assert!(
            !notes(&p).contains("edit"),
            "a new message isn't an edit: {}",
            notes(&p)
        );
    });
}

#[test]
fn nothing_new_writes_no_new_revision() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "hi")]).await;
        chat.prepare(ROOMY).await;
        let first = chat.file();
        let p = chat.prepare(ROOMY).await;
        assert_eq!(chat.file(), first);
        assert_eq!(chat.revisions().await, [(1, "context".to_string())]);
        assert!(p.notes.last().unwrap().contains("revision 1"));
        assert_eq!(chat.state().await.projection, None);
    });
}

#[test]
fn the_current_request_carries_the_editor_context() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "fix it")]).await;
        let p = prepare(
            &chat.db,
            &chat.vfs,
            "chat".into(),
            ROOMY,
            COST,
            Some("fix it\n\n[App.tsx:3-5 selected]".into()),
        )
        .await
        .unwrap();
        assert!(p.messages[0]
            .text
            .starts_with("fix it\n\n[App.tsx:3-5 selected]"));
        assert!(
            chat.file().contains("[App.tsx:3-5 selected]"),
            "the file shows what the model sees"
        );
    });
}

#[test]
fn failed_and_errored_messages_are_never_sent() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "q"),
            NewMessage {
                failed: true,
                ..message("x1", "assistant", "half a reply")
            },
            NewMessage {
                error: Some("timeout".into()),
                ..message("x2", "assistant", "")
            },
            message("a1", "assistant", "answer"),
        ])
        .await;
        let p = chat.prepare(ROOMY).await;
        assert_eq!(ids(&p), ["u1", "a1"]);
        assert!(!chat.file().contains("half a reply"));
    });
}

#[test]
fn a_call_left_without_a_result_is_answered() {
    run(async {
        let chat = Chat::new().await;
        let mut pair = tool_pair(1, "bash", r#"{"command":"ls"}"#, "");
        pair.pop();
        chat.add([vec![message("u1", "user", "q")], pair].concat())
            .await;
        let p = chat.prepare(ROOMY).await;
        assert_eq!(p.messages.len(), 3);
        assert_eq!(p.messages[2].role, "tool");
        assert!(p.messages[2].text.contains("never ran"));
    });
}

#[test]
fn chats_have_separate_files() {
    run(async {
        let a = Chat::new().await;
        a.add(vec![message("u1", "user", "chat A")]).await;
        a.create("other").await;
        let b = Chat {
            db: a.db.clone(),
            vfs: VfsState::default(),
            dir: PathBuf::new(),
            id: "other".into(),
        };
        let b_vfs_shared = &a.vfs;
        b.add(vec![message("v1", "user", "chat B")]).await;
        a.prepare(ROOMY).await;
        prepare(&b.db, b_vfs_shared, "other".into(), ROOMY, COST, None)
            .await
            .unwrap();
        assert_ne!(context_file("chat"), context_file("other"));
        let b_file = String::from_utf8(a.vfs.read_file(&context_file("other")).unwrap()).unwrap();
        assert!(a.file().contains("chat A") && !a.file().contains("chat B"));
        assert!(b_file.contains("chat B") && !b_file.contains("chat A"));
        // An edit to one doesn't touch the other.
        a.write(&with_block(&a.file(), 1, "chat A, edited"));
        b.add(vec![message("v2", "user", "more")]).await;
        let p = prepare(&b.db, b_vfs_shared, "other".into(), ROOMY, COST, None)
            .await
            .unwrap();
        assert!(!notes(&p).contains("edit applied"));
        std::mem::forget(b); // shares a's database directory
    });
}

#[test]
fn chat_ids_cannot_escape_the_context_folder() {
    assert_eq!(context_dir("../../etc"), "context/etc");
    assert_eq!(context_file("a/b\\c"), "context/abc/LIVE_CONTEXT.md");
}

/*
 * Edits
 */

#[test]
fn wiping_the_file_keeps_the_task() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "the task"),
            message("a1", "assistant", "working"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        chat.write("");
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("edit applied") && notes(&p).contains("protected"),
            "{}",
            notes(&p)
        );
        assert_eq!(ids(&p), ["u1"]);
        assert!(chat.file().contains("the task"), "the task is put back");
    });
}

#[test]
fn wiping_the_file_keeps_what_came_after() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "old task"),
            message("a1", "assistant", "old work"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        chat.write("");
        chat.add(vec![message("u2", "user", "new task")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("edit applied"), "{}", notes(&p));
        assert_eq!(ids(&p), ["u1", "u2"], "the user's messages are kept");
    });
}

#[test]
fn a_plain_text_rewrite_becomes_a_notes_block() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "big"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        chat.write("Goal: fix parser.\nNEXT: run tests");
        chat.add(vec![message("a9", "assistant", "Compacted.")])
            .await;
        let p = chat.prepare(ROOMY).await;
        assert_eq!(p.messages[0].text, "task", "the task leads");
        assert_eq!(
            p.messages[1].text,
            "[notes]\nGoal: fix parser.\nNEXT: run tests"
        );
        assert_eq!(ids(&p)[2], "a9");
        assert!(chat.file().contains("role=notes"));
    });
}

#[test]
fn an_edit_that_grows_but_fits_is_applied_with_a_warning() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        chat.write(&format!(
            "{}\n[[CTX_TURN 2 role=notes id=new-plan]]\n{}",
            chat.file(),
            "plan ".repeat(200)
        ));
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("edit applied") && notes(&p).contains("GREW"),
            "{}",
            notes(&p)
        );
        assert!(p.messages.iter().any(|m| m.text.contains("plan plan")));
    });
}

#[test]
fn an_edit_whose_anchor_was_retried_away_is_not_applied() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "task"),
            message("a1", "assistant", "first try"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 2, "summary"));
        // The user retries the reply the file was written after.
        chat.delete_message("a1").await;
        chat.add(vec![message("a2", "assistant", "second try")])
            .await;
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("not applied"), "{}", notes(&p));
        assert_eq!(ids(&p), ["u1", "a2"]);
    });
}

#[test]
fn an_applied_edit_is_dropped_when_its_anchor_disappears() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "task"),
            message("a1", "assistant", "long answer"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 2, "short"));
        chat.add(vec![message("u2", "user", "next")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("edit applied"));
        assert!(p.messages.iter().any(|m| m.text == "short"));
        chat.delete_message("a1").await;
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("stored history changed"),
            "{}",
            notes(&p)
        );
        assert_eq!(ids(&p), ["u1", "u2"]);
        assert_eq!(chat.state().await.projection, None);
    });
}

#[test]
fn deleting_the_file_is_not_an_edit_and_numbering_goes_on() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        chat.add(vec![message("a1", "assistant", "x")]).await;
        chat.prepare(ROOMY).await;
        chat.vfs.remove(&chat.path()).unwrap();
        chat.add(vec![message("u2", "user", "y")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(!notes(&p).contains("edit"), "{}", notes(&p));
        assert_eq!(ids(&p), ["u1", "a1", "u2"]);
        let numbers: Vec<u64> = chat.revisions().await.iter().map(|r| r.0).collect();
        assert_eq!(
            numbers,
            [1, 2, 3],
            "revisions keep counting up: {numbers:?}"
        );
    });
}

#[test]
fn an_edit_through_the_agents_edit_tool_is_picked_up() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, &"x".repeat(900)),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        let out = tool_edit(
            &chat.vfs,
            "chat",
            "@context/LIVE_CONTEXT.md",
            &"x".repeat(900),
            "[a: 900 x's]",
            None,
            None,
        )
        .unwrap();
        assert!(out.contains(APPLIES), "{out}");
        chat.add(tool_pair(
            2,
            "edit",
            r#"{"path":"@context/LIVE_CONTEXT.md"}"#,
            &out,
        ))
        .await;
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("edit applied as revision 2"),
            "{}",
            notes(&p)
        );
        assert!(
            notes(&p).contains("1 tool call(s)"),
            "the shortened result's call became text"
        );
        assert!(p
            .messages
            .iter()
            .all(|m| !m.text.contains(&"x".repeat(900))));
        // The edit's own call and result follow, structured.
        assert_eq!(&ids(&p)[ids(&p).len() - 2..], ["a2", "t2"]);
        assert!(p.messages[p.messages.len() - 2].tool_calls.is_some());
    });
}

#[test]
fn a_second_edit_builds_on_the_first() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "task"),
            message("a1", "assistant", "one"),
            message("u2", "user", "more"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 2, "1"));
        chat.add(vec![message("a2", "assistant", "two")]).await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 4, "2"));
        chat.add(vec![message("u3", "user", "go on")]).await;
        let p = chat.prepare(ROOMY).await;
        let texts: Vec<&str> = p.messages.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.starts_with(&["task", "1", "more", "2"]), "{texts:?}");
        assert!(texts.last().unwrap().starts_with("go on"));
        let kinds: Vec<String> = chat.revisions().await.into_iter().map(|r| r.1).collect();
        assert_eq!(kinds, ["context", "edit", "context", "edit", "context"]);
    });
}

/*
 * Budget
 */

#[test]
fn over_budget_the_oldest_new_outputs_are_withheld_and_the_latest_kept() {
    run(async {
        let chat = Chat::new().await;
        let big = "y".repeat(3000); // ~1000 tokens each
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, &big),
                tool_pair(2, "read", r#"{"path":"b"}"#, &big),
                tool_pair(3, "read", r#"{"path":"c"}"#, &big),
            ]
            .concat(),
        )
        .await;
        let p = chat.prepare(1_800).await;
        let t = |id: &str| p.messages.iter().find(|m| m.id == id).unwrap().text.clone();
        assert!(t("t1").starts_with("[Output withheld"), "oldest first");
        assert!(t("t2").starts_with("[Output withheld"));
        assert_eq!(t("t3"), big, "the latest output stays whole");
        assert!(
            notes(&p).contains("2 older tool output(s)"),
            "{}",
            notes(&p)
        );
        assert!(p.info.trimmed);
        assert!(
            chat.file().contains("[Output withheld"),
            "the file shows what was sent"
        );
    });
}

#[test]
fn outputs_the_agent_kept_are_not_withheld_and_trimming_is_the_last_resort() {
    run(async {
        let chat = Chat::new().await;
        let big = "z".repeat(3000);
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, &big),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        // The agent shortens the task line and keeps the big output.
        chat.write(&with_block(&chat.file(), 1, "t"));
        chat.add(tool_pair(2, "read", r#"{"path":"b"}"#, &big))
            .await;
        let p = chat.prepare(1_200).await;
        assert!(notes(&p).contains("edit applied"), "{}", notes(&p));
        assert!(
            !notes(&p).contains("were withheld"),
            "nothing after the edit can be withheld but the latest"
        );
        assert!(notes(&p).contains("STILL OVER"), "{}", notes(&p));
        assert!(notes(&p).contains("still over budget"), "{}", notes(&p));
        assert!(p
            .messages
            .iter()
            .all(|m| !m.text.starts_with("[Output withheld")));
    });
}

#[test]
fn an_edit_that_shrinks_is_taken_even_when_still_over_and_one_that_grows_is_not() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", &"q".repeat(1_500)),
            message("a1", "assistant", &"r".repeat(1_500)),
        ])
        .await;
        chat.prepare(ROOMY).await;
        // Over a 400-token budget either way; this one shrinks (~1000 -> ~500).
        chat.write(&with_block(&chat.file(), 2, &"r".repeat(10)));
        let p = chat.prepare(400).await;
        assert!(
            notes(&p).contains("edit applied") && notes(&p).contains("STILL OVER"),
            "{}",
            notes(&p)
        );
        assert!(p.messages.iter().all(|m| m.text != "r".repeat(1_500)));
        // This one grows past the budget: rejected.
        chat.write(&format!(
            "{}\n[[CTX_TURN 3 role=notes id=new-x]]\n{}",
            chat.file(),
            "n".repeat(600)
        ));
        let p = chat.prepare(400).await;
        assert!(notes(&p).contains("REJECTED"), "{}", notes(&p));
        assert!(p.messages.iter().all(|m| !m.text.contains("nnnn")));
    });
}

#[test]
fn reminders_fire_once_per_tier_and_rearm_after_an_edit() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", &"w".repeat(900))])
            .await; // ~300 tokens
        let p = chat.prepare(1_000).await;
        assert!(notes(&p).contains("No action needed"), "{}", notes(&p));
        chat.add(vec![message("a1", "assistant", "ok")]).await;
        let p = chat.prepare(1_000).await;
        assert!(
            !notes(&p).contains("CONTEXT BUDGET"),
            "same tier, no reminder: {}",
            notes(&p)
        );
        chat.add(tool_pair(2, "read", "{}", &"w".repeat(900)))
            .await; // ~60%
        let p = chat.prepare(1_000).await;
        assert!(
            notes(&p).contains("tidy your context file ONCE"),
            "{}",
            notes(&p)
        );
        assert_eq!(chat.state().await.nudged, [25, 50]);
        // An accepted edit re-arms them.
        chat.write(&with_block(&chat.file(), 4, "short"));
        chat.add(vec![message("a3", "assistant", "done")]).await;
        chat.prepare(1_000).await;
        let state = chat.state().await;
        assert!(state.nudged.len() <= 1, "re-armed: {:?}", state.nudged);
    });
}

#[test]
fn near_the_budget_every_request_gets_the_urgent_reminder() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", &"v".repeat(2_800))])
            .await; // ~93%
        for _ in 0..2 {
            let p = chat.prepare(1_000).await;
            assert!(notes(&p).contains("URGENT"), "{}", notes(&p));
        }
    });
}

/*
 * Restore and restart
 */

#[test]
fn restoring_an_older_revision_brings_tool_calls_back() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "full content"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 3, "[a: summarized]"));
        chat.add(vec![message("u2", "user", "next")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(
            p.messages.iter().all(|m| m.tool_calls.is_none()),
            "flattened by the edit"
        );

        restore_revision(&chat.db, &chat.vfs, "chat", 1)
            .await
            .unwrap();
        assert!(chat.revisions().await.iter().any(|r| r.1 == "restore"));
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("edit applied"), "{}", notes(&p));
        assert_eq!(
            ids(&p),
            ["u1", "a1", "t1", "u2"],
            "what came after revision 1 stays"
        );
        assert!(
            p.messages[1].tool_calls.is_some(),
            "the call is structured again"
        );
        assert_eq!(p.messages[2].text, "full content");
    });
}

#[test]
fn restoring_a_revision_that_is_not_kept_fails_cleanly() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        let err = restore_revision(&chat.db, &chat.vfs, "chat", 99)
            .await
            .unwrap_err();
        assert!(err.contains("isn't kept"), "{err}");
    });
}

#[test]
fn the_agent_restores_a_revision_or_the_stored_chat() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "full content"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 3, "[a: summarized]"));
        chat.add(vec![message("u2", "user", "next")]).await;
        chat.prepare(ROOMY).await;
        assert!(chat.state().await.projection.is_some());

        let arg = |v: &str| serde_json::Value::String(v.into());
        let said = tool_restore(&chat.db, &chat.vfs, "chat", Some(&arg("1")))
            .await
            .unwrap();
        assert!(said.starts_with("Revision 1 is your context file again as revision"), "{said}");
        let p = chat.prepare(ROOMY).await;
        assert_eq!(ids(&p), ["u1", "a1", "t1", "u2"]);
        assert_eq!(p.messages[2].text, "full content");
        assert!(p.messages[1].tool_calls.is_some());

        // Summarized again, then "stored": the chat as stored, the edit's
        // version kept as a revision.
        chat.write(&with_block(&chat.file(), 3, "[a: summarized again]"));
        chat.prepare(ROOMY).await;
        assert!(chat.state().await.projection.is_some());
        let said = tool_restore(&chat.db, &chat.vfs, "chat", Some(&arg("stored")))
            .await
            .unwrap();
        assert!(said.contains("as stored") && said.contains("revision"), "{said}");
        assert!(chat.state().await.projection.is_none());
        let p = chat.prepare(ROOMY).await;
        assert!(!notes(&p).contains("edit applied"), "{}", notes(&p));
        assert_eq!(p.messages[2].text, "full content");

        // Garbage is refused with what to pass.
        for bad in [Some(arg("latest")), Some(serde_json::json!(true)), None] {
            let err = tool_restore(&chat.db, &chat.vfs, "chat", bad.as_ref())
                .await
                .unwrap_err();
            assert!(err.contains("\"stored\""), "{err}");
        }
        let err = tool_restore(&chat.db, &chat.vfs, "chat", Some(&serde_json::json!(99)))
            .await
            .unwrap_err();
        assert!(err.contains("isn't kept"), "{err}");
    });
}

#[test]
fn reset_drops_an_edit_not_sent_yet() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "full content"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 3, "[a: summarized]"));
        reset(&chat.db, &chat.vfs, "chat").await.unwrap();
        let p = chat.prepare(ROOMY).await;
        assert!(!notes(&p).contains("edit applied"), "{}", notes(&p));
        assert_eq!(p.messages[2].text, "full content");
        assert!(chat.state().await.projection.is_none());
    });
}

#[test]
fn listing_the_context_folder() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        tool_write(&chat.vfs, "chat", "@context/notes.md", "plan").unwrap();
        tool_write(&chat.vfs, "chat", "@context/data/big.txt", "x").unwrap();

        let list = tool_list(&chat.db, &chat.vfs, "chat", "@context").await.unwrap();
        let lines: Vec<&str> = list.lines().collect();
        assert!(lines[0].starts_with("@context/LIVE_CONTEXT.md (your context: ~"), "{list}");
        assert!(lines[0].contains("revision 1"), "{list}");
        assert!(lines[1].starts_with("@context/revisions/ (1 revisions"), "{list}");
        assert!(list.contains("@context/data/\n") || list.ends_with("@context/data/"), "{list}");
        assert!(list.contains("@context/notes.md (4 bytes)"), "{list}");
        assert_eq!(list.matches("LIVE_CONTEXT.md").count(), 1, "{list}");

        let sub = tool_list(&chat.db, &chat.vfs, "chat", "@context/data").await.unwrap();
        assert_eq!(sub, "@context/data/big.txt (1 bytes)");
        let revisions = tool_list(&chat.db, &chat.vfs, "chat", "@context/revisions").await.unwrap();
        assert!(revisions.starts_with("Revisions of your context file"), "{revisions}");
        let err = tool_list(&chat.db, &chat.vfs, "chat", "@context/revisions/1.md").await.unwrap_err();
        assert!(err.contains("read"), "{err}");
        let err = tool_list(&chat.db, &chat.vfs, "chat", "@context/missing").await.unwrap_err();
        assert!(err.contains("not found"), "{err}");
    });
}

#[test]
fn after_a_restart_numbering_continues_and_old_revisions_read_from_the_database() {
    run(async {
        let mut chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        chat.add(vec![message("a1", "assistant", "x")]).await;
        chat.prepare(ROOMY).await;
        chat.vfs = VfsState::default(); // the app restarts
        let listing = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions",
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            listing.contains("   1 ") && listing.contains("   2 "),
            "{listing}"
        );
        chat.add(vec![message("u2", "user", "y")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(
            !notes(&p).contains("edit"),
            "a refilled volume isn't an edit: {}",
            notes(&p)
        );
        assert!(
            p.notes.last().unwrap().contains("revision 3"),
            "{}",
            notes(&p)
        );
        let old = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions/1.md",
            None,
            None,
        )
        .await
        .unwrap();
        assert!(old.contains("task") && !old.contains("role=assistant"));
    });
}

/*
 * The agent's file tools
 */

#[test]
fn reading_the_context_folder() {
    run(async {
        let chat = Chat::new().await;
        let err = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/LIVE_CONTEXT.md",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("doesn't exist yet"));
        let none = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions",
            None,
            None,
        )
        .await
        .unwrap();
        assert!(none.starts_with("No revisions yet"));

        chat.add(vec![
            message("u1", "user", "task"),
            message("a1", "assistant", "answer"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        let whole = tool_read(&chat.db, &chat.vfs, "chat", "@context", None, None)
            .await
            .unwrap();
        assert!(
            whole.contains("[[CTX_TURN 2 role=assistant id=a1]]")
                && whole.contains("don't read it whole again")
        );
        let lines = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/LIVE_CONTEXT.md",
            Some(3),
            Some(4),
        )
        .await
        .unwrap();
        assert_eq!(lines, "[[CTX_TURN 1 role=user id=u1]]\ntask");
        let block = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions/1.md#2",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(block, "[[CTX_TURN 2 role=assistant id=a1]]\nanswer");
        let err = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions/1.md#3",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("no block 3"));
        let err = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions/7.md",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("isn't kept"));
        let err = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/revisions/latest.md",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("no such revision"));
        // Past the end: nothing, not an error.
        let past = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context",
            Some(500),
            Some(600),
        )
        .await
        .unwrap();
        assert_eq!(past, "");
    });
}

#[test]
fn editing_and_writing_the_context_folder() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![
            message("u1", "user", "same"),
            message("a1", "assistant", "same"),
        ])
        .await;
        chat.prepare(ROOMY).await;
        let err = tool_edit(
            &chat.vfs,
            "chat",
            "@context/LIVE_CONTEXT.md",
            "same",
            "x",
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_lowercase().contains("2"), "ambiguous: {err}");
        let err = tool_edit(
            &chat.vfs,
            "chat",
            "@context/revisions/1.md",
            "same",
            "x",
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("can't be changed"));
        let err = tool_write(&chat.vfs, "chat", "@context/revisions/1.md", "x").unwrap_err();
        assert!(err.contains("can't be changed"));
        let err = tool_write(&chat.vfs, "chat", "@context/../../etc/x", "x").unwrap_err();
        assert!(err.contains("invalid path"));
        assert!(
            tool_write(&chat.vfs, "chat", "src/main.rs", "x").is_err(),
            "not the context folder"
        );

        // The agent's own notes: written, edited, read; not its context.
        let out = tool_write(
            &chat.vfs,
            "chat",
            "@context/notes/plan.md",
            "step 1\nstep 2",
        )
        .unwrap();
        assert!(!out.contains(APPLIES));
        tool_edit(
            &chat.vfs,
            "chat",
            "@context/notes/plan.md",
            "step 2",
            "step 2 (done)",
            None,
            None,
        )
        .unwrap();
        let notes_file = tool_read(
            &chat.db,
            &chat.vfs,
            "chat",
            "@context/notes/plan.md",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(notes_file, "step 1\nstep 2 (done)");
        let err = tool_edit(
            &chat.vfs,
            "chat",
            "@context/missing.md",
            "a",
            "b",
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("not found"));
        chat.add(vec![message("u2", "user", "next")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(
            !notes(&p).contains("edit"),
            "notes files aren't the context: {}",
            notes(&p)
        );

        // Writing the context file whole is an edit like any other.
        let out = tool_write(
            &chat.vfs,
            "chat",
            "@context/LIVE_CONTEXT.md",
            "Just the plan.",
        )
        .unwrap();
        assert!(out.contains(APPLIES));
        chat.add(vec![message("u3", "user", "go")]).await;
        let p = chat.prepare(ROOMY).await;
        assert_eq!(p.messages[0].text, "same", "the user's messages are kept");
        assert!(p.messages.iter().any(|m| m.text == "[notes]\nJust the plan."));
        assert!(!p.messages.iter().any(|m| m.role == "assistant"));
    });
}

#[test]
fn agent_paths_in_every_spelling() {
    let vfs = VfsState::default();
    let is = |p: &str| target(&vfs, "c", p);
    assert!(matches!(is("@context"), Ok(Target::Live)));
    assert!(matches!(is("@context/"), Ok(Target::Live)));
    assert!(matches!(is(" @context/LIVE_CONTEXT.md "), Ok(Target::Live)));
    assert!(matches!(
        is("@context\\revisions\\3.md"),
        Ok(Target::Revision(3, None))
    ));
    assert!(matches!(
        is("@context/revisions/r3"),
        Ok(Target::Revision(3, None))
    ));
    assert!(matches!(
        is("@context/revisions/3.md#turn=2"),
        Ok(Target::Revision(3, Some(2)))
    ));
    assert!(matches!(
        is("@context/revisions/3.md#turn-2"),
        Ok(Target::Revision(3, Some(2)))
    ));
    assert!(is("@context/revisions/3.md#x").is_err());
    assert!(is("@context/a//b").is_err());
    assert!(
        is("/home/user/project/file").is_err(),
        "a real path outside the folder (volume not mounted)"
    );
}

/*
 * Recall hints
 */

#[test]
fn recall_hints_only_when_the_result_is_gone() {
    run(async {
        let chat = Chat::new().await;
        let args = r#"{"command":"cargo test"}"#;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "bash", args, "test result: 2 failed"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        assert_eq!(
            chat.hint("c9", "bash", args).await,
            None,
            "no edits yet"
        );

        // The agent edits something else and keeps the output.
        chat.write(&with_block(&chat.file(), 1, "task: make tests pass"));
        chat.add(vec![message("a5", "assistant", "next")]).await;
        chat.prepare(ROOMY).await;
        assert_eq!(
            chat.hint("c9", "bash", args).await,
            None,
            "still visible"
        );

        // Now it removes the output and runs the command again.
        chat.write(&with_block(&chat.file(), 3, "[tests: 2 failed in parser]"));
        chat.add(vec![message("a6", "assistant", "again")]).await;
        chat.prepare(ROOMY).await;
        let hint = chat
            .hint(
                "c9",
                "bash",
                r#"{"command":"cargo test","timeout_seconds":60}"#,
            )
            .await
            .unwrap();
        assert!(hint.contains("still holds it"), "{hint}");
        assert!(hint.contains("#3"), "points at the block: {hint}");
        assert_eq!(
            chat.hint("c9", "bash", r#"{"command":"cargo build"}"#)
                .await,
            None
        );
        assert_eq!(
            chat.hint("c1", "bash", args).await,
            None,
            "a call isn't a repeat of itself"
        );
    });
}

/*
 * Database
 */

#[test]
fn modes_default_to_unset_and_go_with_the_chat() {
    run(async {
        let chat = Chat::new().await;
        let mode = chat
            .db
            .read(|conn| store::context_mode(conn, "chat"))
            .await
            .unwrap();
        assert_eq!(mode, None);
        chat.db
            .write(|conn| store::set_context_mode(conn, "chat", MODE_CLM))
            .await
            .unwrap();
        assert_eq!(
            chat.db
                .read(|conn| store::context_mode(conn, "chat"))
                .await
                .unwrap()
                .as_deref(),
            Some(MODE_CLM)
        );
        assert_eq!(
            chat.db
                .read(|conn| store::context_mode(conn, "nope"))
                .await
                .unwrap(),
            None
        );

        chat.add(vec![message("u1", "user", "x")]).await;
        chat.prepare(ROOMY).await;
        chat.db
            .write(|conn| delete_dialog(conn, "chat"))
            .await
            .unwrap();
        let left: i64 = chat
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT (SELECT COUNT(*) FROM clm_state) + (SELECT COUNT(*) FROM clm_revisions)",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        assert_eq!(
            left, 0,
            "deleting a chat deletes its context state and revisions"
        );
    });
}

#[test]
fn old_snapshots_are_pruned_but_edits_and_their_backups_kept() {
    run(async {
        let chat = Chat::new().await;
        let rev = |n: u64, kind: &str| store::NewRevision {
            number: n,
            kind: kind.into(),
            created_at: n as i64,
            tokens: 1,
            content: format!("r{n}"),
        };
        // 300 snapshots, with an edit at 10 (so 9 is its backup) and at 200.
        let rows: Vec<store::NewRevision> = (1..=300)
            .map(|n| {
                rev(
                    n,
                    if n == 10 || n == 200 {
                        "edit"
                    } else {
                        "context"
                    },
                )
            })
            .collect();
        chat.db
            .write(move |conn| store::insert_revisions(conn, "chat", &rows))
            .await
            .unwrap();
        let numbers: Vec<u64> = chat.revisions().await.iter().map(|r| r.0).collect();
        for kept in [9, 10, 199, 200, 300, 269] {
            assert!(numbers.contains(&kept), "{kept} kept: {numbers:?}");
        }
        assert!(
            !numbers.contains(&5) && !numbers.contains(&150),
            "plain old snapshots go"
        );
        assert!(numbers.len() <= 160);
        // Inserting a number that exists changes nothing.
        chat.db
            .write(move |conn| store::insert_revisions(conn, "chat", &[rev(300, "edit")]))
            .await
            .unwrap();
        assert_eq!(
            chat.revisions().await.last().unwrap(),
            &(300, "context".to_string())
        );
    });
}

#[test]
fn finding_a_message_in_revisions_treats_ids_literally() {
    run(async {
        let chat = Chat::new().await;
        let rows = vec![
            store::NewRevision {
                number: 1,
                kind: "context".into(),
                created_at: 1,
                tokens: 1,
                content: "[[CTX_TURN 1 role=tool id=aXb]]\nwrong".into(),
            },
            store::NewRevision {
                number: 2,
                kind: "context".into(),
                created_at: 2,
                tokens: 1,
                content: "[[CTX_TURN 1 role=tool id=a_b]]\nright".into(),
            },
            store::NewRevision {
                number: 3,
                kind: "context".into(),
                created_at: 3,
                tokens: 1,
                content: "[[CTX_TURN 1 role=tool id=a%b]]\npercent".into(),
            },
        ];
        chat.db
            .write(move |conn| store::insert_revisions(conn, "chat", &rows))
            .await
            .unwrap();
        let found = |id: &'static str| {
            chat.db
                .read(move |conn| store::revisions_with_message(conn, "chat", id, 10))
        };
        let a_b: Vec<u64> = found("a_b")
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(a_b, [2], "_ is not a wildcard");
        let a_pct: Vec<u64> = found("a%b")
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(a_pct, [3], "% is not a wildcard");
    });
}

#[test]
fn state_round_trips() {
    run(async {
        let chat = Chat::new().await;
        assert_eq!(chat.state().await.rendered, None, "no state yet");
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "out"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        let s = chat.state().await;
        assert_eq!(s.rendered.as_deref(), Some(chat.file().as_str()));
        assert_eq!(s.rendered_anchor_id.as_deref(), Some("t1"));
        assert_eq!(s.rendered_revision, Some(1));
        let rendered = s.rendered_messages.unwrap();
        assert_eq!(rendered.len(), 3);
        assert!(
            rendered[1].tool_calls.is_some(),
            "tool calls survive the database"
        );
        assert_eq!(s.budget, ROOMY);
        assert!(s.tokens > 0);
    });
}

#[test]
fn restored_text_appends_only_newer_messages() {
    let m = |id: &str, text: &str| CtxMsg {
        id: id.into(),
        role: "user".into(),
        label: None,
        text: text.into(),
        tool_calls: None,
        tool_call_id: None,
        attachments: vec![],
        harness: false,
    };
    let stored: Vec<String> = ["u1", "a1", "u2", "a2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let old = document::render(&[m("u1", "one"), m("a1", "two")], 1);
    // The current context: a1 rewritten, its notes, then u2 and a2.
    let current = vec![
        m("u1", "one"),
        m("a1~e", "2"),
        m("new-1-1", "notes"),
        m("u2", "three"),
        m("a2", "four"),
    ];
    let text = restored_text(&old, &current, &stored);
    let (back, _) = document::parse_back(&text, &[], &current);
    let ids: Vec<&str> = back.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["u1", "a1", "u2", "a2"],
        "the agent's newer notes and rewrite aren't carried over"
    );
    // Nothing newer: the revision as it was.
    assert_eq!(restored_text(&old, &current[..2], &stored), old);
}

#[test]
fn a_later_edit_does_not_report_old_flattened_calls_again() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message("u1", "user", "task")],
                tool_pair(1, "read", r#"{"path":"a"}"#, "content"),
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        chat.write(&with_block(&chat.file(), 3, "[a: short]"));
        chat.add(vec![message("u2", "user", "next")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("1 tool call(s)"), "{}", notes(&p));
        // A second edit elsewhere.
        chat.write(&with_block(&chat.file(), 4, "next (rephrased)"));
        chat.add(vec![message("u3", "user", "go")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(notes(&p).contains("edit applied"), "{}", notes(&p));
        assert!(
            !notes(&p).contains("tool call(s)"),
            "already text before this edit: {}",
            notes(&p)
        );
        assert!(p
            .messages
            .iter()
            .all(|m| m.tool_calls.is_none() && m.role != "tool"));
    });
}

/*
 * Keeping the task: the user's messages and the task card
 */

#[test]
fn the_users_messages_survive_any_edit() {
    run(async {
        let chat = Chat::new().await;
        chat.add(
            [
                vec![message(
                    "u1",
                    "user",
                    "Fix the parser; don't touch the lexer",
                )],
                tool_pair(1, "read", r#"{"path":"a"}"#, &"code ".repeat(300)),
                vec![
                    message("a2", "assistant", "Found it."),
                    message("u2", "user", "Keep the public API"),
                    message("a3", "assistant", "OK."),
                ],
            ]
            .concat(),
        )
        .await;
        chat.prepare(ROOMY).await;
        // The agent compacts everything into one note.
        chat.write("Bug in parse_expr. NEXT: write the test.");
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("edit applied") && notes(&p).contains("protected"),
            "{}",
            notes(&p)
        );
        assert_eq!(ids(&p)[..2], ["u1", "u2"]);
        assert_eq!(p.messages[0].text, "Fix the parser; don't touch the lexer");
        assert!(p.messages[2].text.contains("Bug in parse_expr"));
        assert!(!p.messages.iter().any(|m| m.text.contains("code code")));
        // And the file shows them back, so the next edit starts from them.
        assert!(chat.file().contains("don't touch the lexer"));
        assert!(chat.file().contains("Keep the public API"));
    });
}

#[test]
fn the_task_card_lives_outside_the_context_file() {
    run(async {
        let mut chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        chat.prepare(ROOMY).await;
        tool_write(
            &chat.vfs,
            "chat",
            "@context/TASK.md",
            "Goal: fix parser\nNEXT: tests",
        )
        .unwrap();
        // Wiping the context file doesn't touch it.
        chat.write("");
        chat.add(vec![message("a1", "assistant", "x")]).await;
        chat.prepare(ROOMY).await;
        assert_eq!(
            chat.state().await.task.as_deref(),
            Some("Goal: fix parser\nNEXT: tests")
        );

        // It survives a restart.
        chat.vfs = VfsState::default();
        chat.add(vec![message("u2", "user", "more")]).await;
        let p = chat.prepare(ROOMY).await;
        assert!(!notes(&p).contains("was deleted"), "{}", notes(&p));
        assert!(
            notes(&p).contains("your task card") && notes(&p).contains("NEXT: tests"),
            "shown with every request's notes: {}",
            notes(&p)
        );
        let card = String::from_utf8(chat.vfs.read_file(&task_file("chat")).unwrap()).unwrap();
        assert_eq!(card, "Goal: fix parser\nNEXT: tests");

        // Deleting it is pointed out, with its last text.
        chat.vfs.remove(&task_file("chat")).unwrap();
        let p = chat.prepare(ROOMY).await;
        assert!(
            notes(&p).contains("was deleted") && notes(&p).contains("Goal: fix parser"),
            "{}",
            notes(&p)
        );
        assert_eq!(chat.state().await.task, None);
        let p = chat.prepare(ROOMY).await;
        assert!(!notes(&p).contains("task card"), "said once: {}", notes(&p));
    });
}

#[test]
fn the_system_section_is_the_same_on_every_request() {
    run(async {
        let chat = Chat::new().await;
        chat.add(vec![message("u1", "user", "task")]).await;
        let section = |c: &Chat| prompt::system_section("/v/LIVE_CONTEXT.md", &c.id);
        let first = section(&chat);
        chat.prepare(ROOMY).await;
        tool_write(&chat.vfs, "chat", "@context/TASK.md", "Goal").unwrap();
        chat.add(vec![message("a1", "assistant", &"x".repeat(900))])
            .await;
        let p = chat.prepare(5_000).await;
        // Size, budget, revision and the card change; the prompt doesn't.
        assert!(
            notes(&p).contains("/5000 tokens") && notes(&p).contains("revision 2"),
            "{}",
            notes(&p)
        );
        assert!(notes(&p).contains("Goal"), "{}", notes(&p));
        assert_eq!(section(&chat), first);
    });
}
