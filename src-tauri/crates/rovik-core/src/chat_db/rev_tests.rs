//! Revisions (schema v10, 6.3): every write function of the chat store,
//! the CLM store and the projects raises the revision of each chat row it
//! touches, so a mirror's pull finds it, and leaves the others' as they
//! were.

use super::attachments;
use super::location::{self, Location};
use super::projects;
use super::schema;
use super::store::*;
use crate::clm::store as clm;
use crate::testing::temp_dir;
use rusqlite::{params, Connection};
use serde_json::json;

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::configure(&conn, true).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn
}

fn clock(conn: &Connection) -> i64 {
    conn.query_row("SELECT rev FROM sync_clock", [], |r| r.get(0))
        .unwrap()
}

fn dialog_rev(conn: &Connection, id: &str) -> i64 {
    conn.query_row("SELECT rev FROM dialogs WHERE id = ?1", [id], |r| r.get(0))
        .unwrap()
}

fn message_rev(conn: &Connection, id: &str) -> i64 {
    conn.query_row("SELECT rev FROM messages WHERE id = ?1", [id], |r| r.get(0))
        .unwrap()
}

fn clm_state_rev(conn: &Connection, id: &str) -> i64 {
    conn.query_row(
        "SELECT rev FROM clm_state WHERE dialog_id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn clm_revision_rev(conn: &Connection, id: &str, number: i64) -> i64 {
    conn.query_row(
        "SELECT rev FROM clm_revisions WHERE dialog_id = ?1 AND number = ?2",
        params![id, number],
        |r| r.get(0),
    )
    .unwrap()
}

/// Runs `write`, then checks that each row `touched` reads has a revision
/// newer than the clock before it, and that each row `untouched` reads has
/// the revision it had.
fn check(
    conn: &mut Connection,
    what: &str,
    write: impl FnOnce(&mut Connection),
    touched: &[&dyn Fn(&Connection) -> i64],
    untouched: &[&dyn Fn(&Connection) -> i64],
) {
    let before = clock(conn);
    let kept: Vec<i64> = untouched.iter().map(|rev| rev(conn)).collect();
    write(conn);
    for (i, rev) in touched.iter().enumerate() {
        let now = rev(conn);
        assert!(
            now > before,
            "{what}: row {i} has {now}, the clock was {before}"
        );
    }
    for (i, (rev, was)) in untouched.iter().zip(kept).enumerate() {
        assert_eq!(rev(conn), was, "{what}: untouched row {i}");
    }
}

fn new_dialog(id: &str, project: Option<&str>) -> NewDialog {
    NewDialog {
        id: id.into(),
        title: "Chat".into(),
        created_at: 1,
        updated_at: 1,
        project_id: project.map(String::from),
    }
}

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
        created_at: 1,
        finalized: false,
    }
}

const FILE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn the_chat_stores_writes_raise_the_rows_revisions() {
    let mut conn = db();
    let other = |c: &Connection| dialog_rev(c, "other");
    create_dialog(&mut conn, &new_dialog("other", None)).unwrap();

    check(
        &mut conn,
        "create_dialog",
        |c| create_dialog(c, &new_dialog("d", None)).unwrap(),
        &[&|c| dialog_rev(c, "d")],
        &[&other],
    );
    check(
        &mut conn,
        "create_child_dialog",
        |c| {
            let child = NewChildDialog {
                id: "kid".into(),
                parent_id: "d".into(),
                parent_call_id: "t1".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: 2,
            };
            create_child_dialog(c, &child).unwrap()
        },
        &[&|c| dialog_rev(c, "kid")],
        &[&other],
    );
    check(
        &mut conn,
        "rename_dialog",
        |c| rename_dialog(c, "d", "Renamed").unwrap(),
        &[&|c| dialog_rev(c, "d")],
        &[&other, &|c| dialog_rev(c, "kid")],
    );
    check(
        &mut conn,
        "set_chat_model",
        |c| {
            let model = ChatModel {
                provider_id: "p".into(),
                model_id: "m".into(),
                effort: None,
            };
            set_chat_model(c, "d", &model).unwrap()
        },
        &[&|c| dialog_rev(c, "d")],
        &[&other],
    );
    projects::ensure(&mut conn, "proj", "Project", std::path::Path::new("/p")).unwrap();
    check(
        &mut conn,
        "set_dialog_project",
        |c| set_dialog_project(c, "d", Some("proj")).unwrap(),
        &[&|c| dialog_rev(c, "d")],
        &[&other],
    );

    attachments::add_unavailable(&conn, FILE).unwrap();
    check(
        &mut conn,
        "append_messages",
        |c| {
            let with_file = NewMessage {
                attachments: vec![FILE.into()],
                finalized: true,
                ..message("u1", "user", "Look")
            };
            append_messages(c, "d", &[with_file, message("a1", "assistant", "")]).unwrap();
        },
        &[&|c| message_rev(c, "u1"), &|c| message_rev(c, "a1"), &|c| {
            dialog_rev(c, "d")
        }],
        &[&other],
    );
    check(
        &mut conn,
        "append_numbered",
        |c| {
            let rows = [
                (1, message("u1", "user", "Look")),
                (5000, message("t1", "tool", "x")),
            ];
            append_numbered(c, "d", &rows).unwrap();
        },
        &[&|c| message_rev(c, "t1")],
        &[&other, &|c| message_rev(c, "u1")],
    );
    let patches = [
        (
            "content",
            MessagePatch {
                content: Some("Hello".into()),
                ..Default::default()
            },
        ),
        (
            "tool calls",
            MessagePatch {
                tool_calls: Some(vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }]),
                ..Default::default()
            },
        ),
        (
            "meta",
            MessagePatch {
                meta: Some(json!({ "interrupted": true })),
                ..Default::default()
            },
        ),
        (
            "error",
            MessagePatch {
                error: Some("cut".into()),
                ..Default::default()
            },
        ),
        (
            "failed",
            MessagePatch {
                failed: Some(true),
                ..Default::default()
            },
        ),
        (
            "finalize",
            MessagePatch {
                finalize: true,
                ..Default::default()
            },
        ),
    ];
    for (what, patch) in patches {
        check(
            &mut conn,
            &format!("update_message: {what}"),
            |c| update_message(c, "a1", &patch).unwrap(),
            &[&|c| message_rev(c, "a1")],
            &[&other, &|c| message_rev(c, "u1")],
        );
    }
    // A long tool result goes to `message_bodies` as it's finalized.
    check(
        &mut conn,
        "update_message: a body of its own",
        |c| {
            let long = MessagePatch {
                content: Some("x".repeat(EXTERNAL_BODY_BYTES * 2)),
                finalize: true,
                ..Default::default()
            };
            update_message(c, "t1", &long).unwrap()
        },
        &[&|c| message_rev(c, "t1")],
        &[&other],
    );
    check(
        &mut conn,
        "save_tool_call_extras",
        |c| save_tool_call_extras(c, "a1", &[("c1".into(), json!({ "sig": "s" }))]).unwrap(),
        &[&|c| message_rev(c, "a1")],
        &[&other, &|c| message_rev(c, "u1")],
    );
    check(
        &mut conn,
        "location::set",
        |c| location::set(c, "d", &Location::Host("server".into())).unwrap(),
        &[&|c| dialog_rev(c, "d"), &|c| dialog_rev(c, "kid")],
        &[&other],
    );
    // The input's draft is this computer's alone: no chat row changes.
    check(
        &mut conn,
        "save_composer_draft",
        |c| save_composer_draft(c, "d", "[\"draft\"]").unwrap(),
        &[],
        &[&other, &|c| dialog_rev(c, "d")],
    );
}

/// Plain SQL gets revisions too: the triggers set them, not the store.
#[test]
fn a_write_in_plain_sql_raises_the_revision() {
    let mut conn = db();
    create_dialog(&mut conn, &new_dialog("d", None)).unwrap();
    append_messages(&mut conn, "d", &[message("m", "user", "Hi")]).unwrap();
    check(
        &mut conn,
        "an UPDATE of its own",
        |c| {
            c.execute("UPDATE messages SET content = 'Hey' WHERE id = 'm'", [])
                .unwrap();
        },
        &[&|c| message_rev(c, "m")],
        &[&|c| dialog_rev(c, "d")],
    );
    check(
        &mut conn,
        "an attachment row of its own",
        |c| {
            attachments::add_unavailable(c, FILE).unwrap();
            c.execute(
                "INSERT INTO message_attachments (message_id, position, attachment_id) VALUES ('m', 0, ?1)",
                [FILE],
            )
            .unwrap();
        },
        &[&|c| message_rev(c, "m")],
        &[&|c| dialog_rev(c, "d")],
    );
}

#[test]
fn the_clm_stores_writes_raise_the_rows_revisions() {
    let mut conn = db();
    create_dialog(&mut conn, &new_dialog("d", None)).unwrap();
    create_dialog(&mut conn, &new_dialog("other", None)).unwrap();
    let other = |c: &Connection| dialog_rev(c, "other");

    check(
        &mut conn,
        "set_context_mode",
        |c| clm::set_context_mode(c, "d", "clm").unwrap(),
        &[&|c| dialog_rev(c, "d")],
        &[&other],
    );
    let state = clm::State {
        tokens: 10,
        budget: 100,
        ..Default::default()
    };
    check(
        &mut conn,
        "save_state: a first one",
        |c| clm::save_state(c, "d", &state).unwrap(),
        &[&|c| clm_state_rev(c, "d")],
        &[&other],
    );
    check(
        &mut conn,
        "save_state: again",
        |c| {
            let more = clm::State {
                tokens: 20,
                ..state.clone()
            };
            clm::save_state(c, "d", &more).unwrap()
        },
        &[&|c| clm_state_rev(c, "d")],
        &[&other],
    );
    check(
        &mut conn,
        "clear_projection",
        |c| clm::clear_projection(c, "d").unwrap(),
        &[&|c| clm_state_rev(c, "d")],
        &[&other],
    );
    check(
        &mut conn,
        "insert_revisions",
        |c| {
            let revision = clm::NewRevision {
                number: 1,
                kind: "context".into(),
                created_at: 1,
                tokens: 5,
                content: "# Context".into(),
            };
            clm::insert_revisions(c, "d", &[revision]).unwrap()
        },
        &[&|c| clm_revision_rev(c, "d", 1)],
        &[&other, &|c| clm_state_rev(c, "d")],
    );
}

#[test]
fn the_projects_writes_raise_the_revisions_of_the_chats_they_move() {
    let mut conn = db();
    let (here, there) = (temp_dir(), temp_dir());
    projects::ensure(&mut conn, "current-project", "Current", &here.0).unwrap();
    projects::ensure(&mut conn, "target-project", "Target", &there.0).unwrap();
    projects::ensure(
        &mut conn,
        "missing-project",
        "Missing",
        std::path::Path::new("/gone"),
    )
    .unwrap();
    create_dialog(&mut conn, &new_dialog("d", Some("current-project"))).unwrap();
    create_dialog(&mut conn, &new_dialog("m", Some("missing-project"))).unwrap();
    create_dialog(&mut conn, &new_dialog("other", None)).unwrap();
    let other = |c: &Connection| dialog_rev(c, "other");

    // These change projects, not chats.
    check(
        &mut conn,
        "ensure",
        |c| projects::ensure(c, "current-project", "Renamed", &here.0).unwrap(),
        &[],
        &[&other, &|c| dialog_rev(c, "d")],
    );
    check(
        &mut conn,
        "forget_path",
        |c| projects::forget_path(c, "missing-project", std::path::Path::new("/old")).unwrap(),
        &[],
        &[&other, &|c| dialog_rev(c, "d")],
    );
    check(
        &mut conn,
        "decline",
        |c| projects::decline(c, &here.0, &["target-project".into()]).unwrap(),
        &[],
        &[&other, &|c| dialog_rev(c, "d")],
    );
    // These move chats to another project.
    check(
        &mut conn,
        "answer_moved",
        |c| {
            projects::answer_moved(c, &here.0, "current-project", "target-project").unwrap();
        },
        &[&|c| dialog_rev(c, "d")],
        &[&other, &|c| dialog_rev(c, "m")],
    );
    let found = temp_dir();
    projects::write_id_file(&found.0, "target-project").unwrap();
    check(
        &mut conn,
        "relink",
        |c| {
            projects::relink(c, "missing-project", &found.0, true).unwrap();
        },
        &[&|c| dialog_rev(c, "m")],
        &[&other],
    );
}
