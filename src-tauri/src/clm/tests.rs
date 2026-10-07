//! A chat in CLM mode end to end: a real database, an unmounted volume.

use super::*;
use crate::chat_db::store::{append_messages, create_dialog, NewDialog, NewMessage, ToolCall};

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

fn call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.into(),
        extra_content: None,
    }
}

const COST: Cost = Cost {
    vision: false,
    chars_per_token: 3.0,
};

async fn add(db: &DbState, messages: Vec<NewMessage>) {
    db.write(move |conn| append_messages(conn, "chat", &messages).map(|_| ()))
        .await
        .unwrap();
}

#[test]
fn edits_become_the_context_and_survive_a_restart() {
    tauri::async_runtime::block_on(async {
        let dir = std::env::temp_dir().join(format!("clm-test-{}", uuid::Uuid::new_v4()));
        let db = DbState::open(&dir).unwrap();
        db.write(|conn| {
            create_dialog(
                conn,
                &NewDialog {
                    id: "chat".into(),
                    title: "t".into(),
                    created_at: 1,
                    updated_at: 1,
                    project_id: None,
                },
            )
        })
        .await
        .unwrap();
        let big = "x".repeat(3000);
        add(
            &db,
            vec![
                message("u1", "user", "Fix the test"),
                NewMessage {
                    tool_calls: Some(vec![call("c1", "read", r#"{"path":"a.rs"}"#)]),
                    ..message("a1", "assistant", "Reading it.")
                },
                NewMessage {
                    tool_call_id: Some("c1".into()),
                    ..message("t1", "tool", &big)
                },
            ],
        )
        .await;

        // First request: the file is written with the chat as stored.
        let vfs = VfsState::default();
        let p = prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();
        assert_eq!(p.messages.len(), 3);
        assert!(p.notes[0].starts_with("[context: ~"));
        let path = context_file("chat");
        let doc = String::from_utf8(vfs.read_file(&path).unwrap()).unwrap();
        assert!(doc.contains("[[CTX_TURN 3 role=tool id=t1]]"));

        // The agent shortens the big result (with its edit tool) and the
        // run goes on.
        let edited = doc.replace(&big, "[a.rs: main() is empty; fix = add the call]");
        vfs.write_unmounted(&path, edited.as_bytes()).unwrap();
        add(
            &db,
            vec![
                NewMessage {
                    tool_calls: Some(vec![call(
                        "c2",
                        "edit",
                        r#"{"path":"@context/LIVE_CONTEXT.md"}"#,
                    )]),
                    ..message("a2", "assistant", "Compacting.")
                },
                NewMessage {
                    tool_call_id: Some("c2".into()),
                    ..message("t2", "tool", "Edited.")
                },
            ],
        )
        .await;
        let p = prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();
        assert!(
            p.notes[0].contains("edit applied as revision 2"),
            "{:?}",
            p.notes
        );
        assert!(
            p.notes[0].contains("revision 1"),
            "names the backup: {:?}",
            p.notes
        );
        let texts: Vec<&str> = p.messages.iter().map(|m| m.text.as_str()).collect();
        assert!(
            texts.iter().all(|t| !t.contains(&big)),
            "the big output is gone"
        );
        assert!(texts.iter().any(|t| t.contains("main() is empty")));
        assert_eq!(
            p.messages.last().unwrap().id,
            "t2",
            "new messages follow the edited context"
        );
        assert!(p.info.estimated_tokens < p.info.full_tokens);

        // Revisions are in the database: the snapshot, the edit, the new snapshot.
        let revisions = db
            .read(|conn| store::revisions(conn, "chat"))
            .await
            .unwrap();
        let kinds: Vec<(u64, &str)> = revisions
            .iter()
            .map(|r| (r.number, r.kind.as_str()))
            .collect();
        assert_eq!(kinds, [(1, "context"), (2, "edit"), (3, "context")]);

        // Re-reading the result it removed points at the revision holding it.
        let args = r#"{"path":"a.rs"}"#;
        let hint = db
            .read(move |conn| recall_hint(conn, "chat", "c9", "read", args))
            .await
            .unwrap()
            .unwrap();
        // The newest revision still holding the original result: the snapshot before the edit.
        assert!(
            hint.contains("Revision 1 still holds it: read @context/revisions/1.md#3"),
            "{hint}"
        );
        let none = db
            .read(|conn| recall_hint(conn, "chat", "c9", "read", r#"{"path":"b.rs"}"#))
            .await
            .unwrap();
        assert_eq!(none, None, "a call never made before");

        // An app restart: a new, empty volume. The file and its history come back.
        let vfs = VfsState::default();
        let p = prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();
        assert!(p
            .messages
            .iter()
            .any(|m| m.text.contains("main() is empty")));
        let numbers: Vec<u64> = vfs
            .revisions(&path)
            .unwrap()
            .iter()
            .map(|r| r.number)
            .collect();
        assert_eq!(numbers.first(), Some(&1));
        assert!(String::from_utf8_lossy(&vfs.read_revision(&path, 1).unwrap()).contains(&big));

        // An edit that doesn't fit the budget is rejected.
        let doc = String::from_utf8(vfs.read_file(&path).unwrap()).unwrap();
        vfs.write_unmounted(&path, format!("{doc}\n{}", "y".repeat(9000)).as_bytes())
            .unwrap();
        let p = prepare(&db, &vfs, "chat".into(), 2_000, COST, None)
            .await
            .unwrap();
        assert!(p.notes[0].contains("REJECTED"), "{:?}", p.notes);
        assert!(p.messages.iter().all(|m| !m.text.contains("yyyy")));

        let _ = std::fs::remove_dir_all(dir);
    });
}

#[test]
fn agent_paths() {
    let vfs = VfsState::default();
    assert!(matches!(
        target(&vfs, "c", "@context/LIVE_CONTEXT.md"),
        Ok(Target::Live)
    ));
    assert!(matches!(
        target(&vfs, "c", "@context/revisions"),
        Ok(Target::Revisions)
    ));
    assert!(matches!(
        target(&vfs, "c", "@context/revisions/12.md"),
        Ok(Target::Revision(12, None))
    ));
    assert!(matches!(
        target(&vfs, "c", "@context/revisions/12.md#7"),
        Ok(Target::Revision(12, Some(7)))
    ));
    assert!(
        matches!(target(&vfs, "c", "@context/notes.md"), Ok(Target::Other(p)) if p == "notes.md")
    );
    assert!(target(&vfs, "c", "@context/../x").is_err());
    assert!(target(&vfs, "c", "src/main.rs").is_err());
}

#[test]
fn repeated_calls() {
    assert!(same_call(
        "read",
        r#"{"path":"a"}"#,
        r#"{"path":"a","start_line":3}"#
    ));
    assert!(!same_call(
        "read",
        r#"{"path":"a"}"#,
        r#"{"path":"b"}"#
    ));
    assert!(same_call(
        "bash",
        r#"{"command":"ls"}"#,
        r#"{"command":"ls","timeout_seconds":5}"#
    ));
    assert!(!same_call(
        "web_search",
        r#"{"query":"a"}"#,
        r#"{"query":"a","max_results":3}"#
    ));
}

#[test]
fn later_messages_are_added_once() {
    let m = |id: &str| CtxMsg {
        id: id.into(),
        role: "user".into(),
        label: None,
        text: id.into(),
        tool_calls: None,
        tool_call_id: None,
        attachments: vec![],
        harness: false,
    };
    // The agent's version already has m2 (rewritten) and m3.
    let base = vec![m("m1"), m("m2~e"), m("m3")];
    let later = vec![m("m2"), m("m3"), m("m4")];
    let ids: Vec<String> = with_later(base, &later).into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["m1", "m2~e", "m3", "m4"]);
}

/// The file was rewritten but the state saying so wasn't saved (a failed
/// database write): the newer rendering is read back as an edit, and must
/// not put the messages after the older one in the context twice.
#[test]
fn a_rendering_whose_state_was_not_saved_doubles_nothing() {
    tauri::async_runtime::block_on(async {
        let dir = std::env::temp_dir().join(format!("clm-test-{}", uuid::Uuid::new_v4()));
        let db = DbState::open(&dir).unwrap();
        db.write(|conn| {
            create_dialog(
                conn,
                &NewDialog {
                    id: "chat".into(),
                    title: "t".into(),
                    created_at: 1,
                    updated_at: 1,
                    project_id: None,
                },
            )
        })
        .await
        .unwrap();
        let vfs = VfsState::default();
        add(
            &db,
            vec![
                message("u1", "user", "first"),
                message("a1", "assistant", "one"),
            ],
        )
        .await;
        prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();

        add(
            &db,
            vec![
                message("u2", "user", "second"),
                message("a2", "assistant", "two"),
            ],
        )
        .await;
        let saved = db
            .read(|conn| store::load_state(conn, "chat"))
            .await
            .unwrap();
        prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();
        // That request's state never reached the database.
        db.write(move |conn| store::save_state(conn, "chat", &saved))
            .await
            .unwrap();

        add(&db, vec![message("u3", "user", "third")]).await;
        let p = prepare(&db, &vfs, "chat".into(), 100_000, COST, None)
            .await
            .unwrap();
        let ids: Vec<&str> = p.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["u1", "a1", "u2", "a2", "u3"],
            "every message once, in order"
        );

        let _ = std::fs::remove_dir_all(dir);
    });
}
