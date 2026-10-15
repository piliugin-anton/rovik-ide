//! The mirror's changes against real databases (in memory): an owner's
//! chat built in rounds, pulled into mirrors page by page, in bigger pages
//! and in one go, which all end as the owner has it.

use super::*;
use crate::chat_db::mirror;
use crate::chat_db::projects;
use crate::chat_db::schema;
use crate::chat_db::store::{
    self, ChatModel, MessagePatch, NewChildDialog, NewDialog, NewMessage, ToolCall,
};
use crate::clm::store as clm;
use rusqlite::Connection;
use std::collections::BTreeSet;

const ROOT: &str = "chat";
const KID: &str = "kid";
const HOST: &str = "server";
const FILE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::configure(&conn, true).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn
}

/// The owner, with its chat.
fn owner() -> Connection {
    let mut conn = db();
    let dialog = NewDialog {
        id: ROOT.into(),
        title: "Chat".into(),
        created_at: 1,
        updated_at: 1,
        project_id: None,
    };
    store::create_dialog(&mut conn, &dialog).unwrap();
    conn
}

/// A computer's mirror of the chat, which moved to `HOST`: the chat's row,
/// in a project of this computer's.
fn mirror() -> Connection {
    let mut conn = db();
    projects::ensure(
        &mut conn,
        "local-project",
        "Local",
        std::path::Path::new("/p"),
    )
    .unwrap();
    let dialog = NewDialog {
        id: ROOT.into(),
        title: "Chat".into(),
        created_at: 1,
        updated_at: 1,
        project_id: Some("local-project".into()),
    };
    store::create_dialog(&mut conn, &dialog).unwrap();
    mirror::place(&mut conn, ROOT, HOST).unwrap();
    conn
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
        created_at: 2,
        finalized: true,
    }
}

/// The owner's chat, a round of changes at a time: messages with a tool
/// call, its extras, a long result and a file; a subagent's chat, CLM and
/// its revisions; a rename, a reply streamed then finished; one revision
/// deleted.
fn round(conn: &mut Connection, n: usize) {
    match n {
        0 => {
            let call = NewMessage {
                tool_calls: Some(vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"src/a.rs\"}".into(),
                    extra_content: None,
                }]),
                ..message("a1", "assistant", "")
            };
            let result = NewMessage {
                tool_call_id: Some("c1".into()),
                ..message("t1", "tool", &"x".repeat(store::EXTERNAL_BODY_BYTES * 2))
            };
            attachments::add_unavailable(conn, FILE).unwrap();
            let with_file = NewMessage {
                attachments: vec![FILE.into()],
                ..message("u2", "user", "And this screenshot")
            };
            let rows = [
                message("u1", "user", "Fix the parser"),
                call,
                result,
                with_file,
            ];
            store::append_messages(conn, ROOT, &rows).unwrap();
            let extra = ("c1".to_string(), serde_json::json!({ "signature": "s1" }));
            store::save_tool_call_extras(conn, "a1", &[extra]).unwrap();
        }
        1 => {
            let kid = NewChildDialog {
                id: KID.into(),
                parent_id: ROOT.into(),
                parent_call_id: "c2".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: 3,
            };
            store::create_child_dialog(conn, &kid).unwrap();
            let rows = [
                message("k1", "user", "Look"),
                message("k2", "assistant", "Seen"),
            ];
            store::append_messages(conn, KID, &rows).unwrap();
            clm::set_context_mode(conn, ROOT, "clm").unwrap();
            let state = clm::State {
                tokens: 10,
                budget: 100,
                task: Some("Fix the parser".into()),
                ..Default::default()
            };
            clm::save_state(conn, ROOT, &state).unwrap();
            let revisions: Vec<clm::NewRevision> = (1..=3)
                .map(|number| clm::NewRevision {
                    number,
                    kind: "context".into(),
                    created_at: 4,
                    tokens: 5,
                    content: format!("# Context {number}"),
                })
                .collect();
            clm::insert_revisions(conn, ROOT, &revisions).unwrap();
        }
        2 => {
            store::rename_dialog(conn, ROOT, "Parser fix").unwrap();
            let model = ChatModel {
                provider_id: "p".into(),
                model_id: "m".into(),
                effort: Some("high".into()),
            };
            store::set_chat_model(conn, ROOT, &model).unwrap();
            let streaming = NewMessage {
                finalized: false,
                ..message("a2", "assistant", "")
            };
            store::append_messages(conn, ROOT, &[streaming]).unwrap();
            let checkpoint = MessagePatch {
                content: Some("Half of it".into()),
                ..Default::default()
            };
            store::update_message(conn, "a2", &checkpoint).unwrap();
        }
        _ => {
            let done = MessagePatch {
                content: Some("The parser trims its fields now.".into()),
                finalize: true,
                ..Default::default()
            };
            store::update_message(conn, "a2", &done).unwrap();
            conn.execute(
                "DELETE FROM clm_revisions WHERE dialog_id = ?1 AND number = 2",
                [ROOT],
            )
            .unwrap();
            let state = clm::State {
                tokens: 20,
                budget: 100,
                ..Default::default()
            };
            clm::save_state(conn, ROOT, &state).unwrap();
        }
    }
}

/// Pulls the chat's changes into the mirror, `budget` bytes a page;
/// `between` runs on the owner after the first page. Returns the pages.
fn pull_with(
    owner: &mut Connection,
    mirror: &mut Connection,
    budget: usize,
    between: impl FnOnce(&mut Connection),
) -> Vec<ChangesPage> {
    let since = mirror::state(mirror, ROOT).unwrap().unwrap().rev;
    let mut between = Some(between);
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let page = changes_since(owner, ROOT, since, cursor.as_ref(), budget).unwrap();
        apply_pulled(mirror, ROOT, HOST, &page)
            .unwrap()
            .expect("the chat is on the host");
        cursor = page.next.clone();
        pages.push(page);
        if let Some(between) = between.take() {
            between(owner);
        }
        if cursor.is_none() {
            return pages;
        }
    }
}

fn pull(owner: &mut Connection, mirror: &mut Connection, budget: usize) -> Vec<ChangesPage> {
    pull_with(owner, mirror, budget, |_| {})
}

/// What a chat's rows are, apart from what each database keeps for itself
/// (the chat's project and location, revisions, the search index's row
/// numbers), in the order of their keys.
#[derive(Debug, PartialEq)]
struct Dump {
    dialogs: Vec<StoredDialog>,
    messages: Vec<StoredMessage>,
    states: Vec<StoredClmState>,
    revisions: Vec<StoredRevision>,
    searched: BTreeSet<(String, Option<String>)>,
}

fn dump(conn: &Connection) -> Dump {
    let all = |sql: &str| -> Vec<String> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let dialogs = conn
        .prepare(&format!("SELECT {DIALOG_COLUMNS} FROM dialogs ORDER BY id"))
        .unwrap()
        .query_map([], dialog_from_row)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let messages = all("SELECT id FROM messages ORDER BY dialog_id, seq, id")
        .iter()
        .map(|id| read_message(conn, id).unwrap().unwrap().0)
        .collect();
    let states = conn
        .prepare(&format!(
            "SELECT {CLM_STATE_COLUMNS} FROM clm_state ORDER BY dialog_id"
        ))
        .unwrap()
        .query_map([], clm_state_from_row)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let revisions = conn
        .prepare(&format!(
            "SELECT {REVISION_COLUMNS} FROM clm_revisions ORDER BY dialog_id, number"
        ))
        .unwrap()
        .query_map([], revision_from_row)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let searched = conn
        .prepare("SELECT dialog_id, message_id FROM search_rows")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    Dump {
        dialogs,
        messages,
        states,
        revisions,
        searched,
    }
}

/// The rows a page carries besides the chat rows of a first page.
fn stream_rows(page: &ChangesPage) -> usize {
    let c = &page.changes;
    c.clm_states.len() + c.clm_revisions.len() + c.messages.len()
}

#[test]
fn pages_and_pulls_end_as_one_full_export_does() {
    let mut owner = owner();
    // Pulled after every round, a row a page; after two rounds, in pages
    // of 2 KB; and once, at the end, in one page.
    let (mut by_row, mut by_page, mut at_once) = (mirror(), mirror(), mirror());
    for n in 0..4 {
        round(&mut owner, n);
        let pages = if n == 2 {
            // The owner changes during a pull: that comes with the next.
            pull_with(&mut owner, &mut by_row, 1, |owner| {
                store::append_messages(owner, ROOT, &[message("u3", "user", "Also this")]).unwrap();
            })
        } else {
            pull(&mut owner, &mut by_row, 1)
        };
        assert!(
            pages.iter().all(|p| stream_rows(p) <= 1),
            "round {n}: a row a page"
        );
        if n % 2 == 1 {
            pull(&mut owner, &mut by_page, 2048);
        }
    }
    // The change made during a pull comes with the next one.
    pull(&mut owner, &mut by_row, 1);
    let pages = pull(&mut owner, &mut at_once, PAGE_BYTES);
    assert_eq!(pages.len(), 1, "one page");

    let expected = dump(&owner);
    assert_eq!(expected.dialogs.len(), 2);
    assert_eq!(expected.revisions.len(), 2, "one deleted");
    assert!(expected.messages.iter().any(|m| m.id == "u3"));
    for (name, mirror) in [
        ("a row a page", &by_row),
        ("2 KB pages", &by_page),
        ("at once", &at_once),
    ] {
        assert_eq!(dump(mirror), expected, "{name}");
    }
    // The mirror is where the owner's clock was at its last pull.
    let rev = mirror::state(&by_row, ROOT).unwrap().unwrap().rev;
    assert_eq!(rev, clock(&owner).unwrap());
}

#[test]
fn applying_the_same_changes_twice_changes_nothing() {
    let mut owner = owner();
    let mut mirror = mirror();
    for n in 0..4 {
        round(&mut owner, n);
    }
    let page = changes_since(&owner, ROOT, 0, None, PAGE_BYTES).unwrap();
    let first = apply_pulled(&mut mirror, ROOT, HOST, &page)
        .unwrap()
        .unwrap();
    assert!(!first.is_empty());
    let (before, clock_before) = (dump(&mirror), clock(&mirror).unwrap());

    let again = apply_pulled(&mut mirror, ROOT, HOST, &page)
        .unwrap()
        .unwrap();

    assert!(again.is_empty(), "{again:?}");
    assert_eq!(dump(&mirror), before);
    assert_eq!(clock(&mirror).unwrap(), clock_before, "nothing written");
}

#[test]
fn a_body_stored_apart_arrives_whole() {
    let mut owner = owner();
    let mut mirror = mirror();
    round(&mut owner, 0);

    pull(&mut owner, &mut mirror, PAGE_BYTES);

    let body = store::get_message_body(&mirror, "t1").unwrap();
    assert_eq!(body, "x".repeat(store::EXTERNAL_BODY_BYTES * 2));
    let preview = store::get_messages(&mirror, ROOT, None, 10).unwrap();
    let t1 = preview.messages.iter().find(|m| m.id == "t1").unwrap();
    assert!(t1.body_external);
    assert_eq!(t1.content.chars().count(), store::PREVIEW_CHARS);
    assert_eq!(t1.attachments, Vec::<String>::new());
    let u2 = preview.messages.iter().find(|m| m.id == "u2").unwrap();
    assert_eq!(u2.attachments, [FILE], "with its file's row");
}

#[test]
fn a_subagents_chat_arrives_where_its_parent_is() {
    let mut owner = owner();
    let mut mirror = mirror();
    round(&mut owner, 0);
    round(&mut owner, 1);

    pull(&mut owner, &mut mirror, PAGE_BYTES);

    let (project, location): (Option<String>, Option<String>) = mirror
        .query_row(
            "SELECT project_id, location FROM dialogs WHERE id = ?1",
            [KID],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(project.as_deref(), Some("local-project"));
    assert_eq!(
        location::Location::parse(location.as_deref()),
        Location::Host(HOST.into())
    );
    // The chat's own project and location stay this computer's.
    let root_project = store::dialog_project(&mirror, ROOT).unwrap();
    assert_eq!(root_project.as_deref(), Some("local-project"));
    let kid = store::get_messages(&mirror, KID, None, 10)
        .unwrap()
        .messages;
    assert_eq!(kid.len(), 2);
}

#[test]
fn a_clm_revision_the_owner_deleted_is_deleted_here() {
    let mut owner = owner();
    let mut mirror = mirror();
    round(&mut owner, 1);
    pull(&mut owner, &mut mirror, PAGE_BYTES);
    let numbers = |conn: &Connection| -> Vec<u64> {
        clm::revisions(conn, ROOT)
            .unwrap()
            .into_iter()
            .map(|r| r.number)
            .collect()
    };
    assert_eq!(numbers(&mirror), [1, 2, 3]);

    owner
        .execute(
            "DELETE FROM clm_revisions WHERE dialog_id = ?1 AND number = 2",
            [ROOT],
        )
        .unwrap();
    pull(&mut owner, &mut mirror, PAGE_BYTES);

    assert_eq!(numbers(&mirror), [1, 3]);
}

#[test]
fn a_chat_that_moved_back_isnt_written_to() {
    let mut owner = owner();
    let mut mirror = mirror();
    round(&mut owner, 0);
    pull(&mut owner, &mut mirror, PAGE_BYTES);
    let rev = mirror::state(&mirror, ROOT).unwrap().unwrap().rev;
    // It came back to this computer; a pull that was on its way arrives.
    location::set(&mut mirror, ROOT, &Location::Here).unwrap();
    round(&mut owner, 2);
    let before = dump(&mirror);

    let page = changes_since(&owner, ROOT, rev, None, PAGE_BYTES).unwrap();
    let applied = apply_pulled(&mut mirror, ROOT, HOST, &page).unwrap();

    assert_eq!(applied, None);
    assert_eq!(dump(&mirror), before);
    assert_eq!(mirror::state(&mirror, ROOT).unwrap().unwrap().rev, rev);
}

#[test]
fn a_row_bigger_than_a_page_goes_on_a_page_of_its_own() {
    let mut owner = owner();
    let rows = [
        message("u1", "user", &"y".repeat(5000)),
        message("u2", "user", "short"),
    ];
    store::append_messages(&mut owner, ROOT, &rows).unwrap();

    let first = changes_since(&owner, ROOT, 0, None, 1000).unwrap();
    // The chat's row comes first, alone: the long message doesn't fit.
    assert_eq!(first.changes.dialogs.len(), 1);
    assert_eq!(stream_rows(&first), 0);
    let second = changes_since(&owner, ROOT, 0, first.next.as_ref(), 1000).unwrap();
    let ids: Vec<&str> = second
        .changes
        .messages
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["u1"], "alone, though bigger than the page");
    let third = changes_since(&owner, ROOT, 0, second.next.as_ref(), 1000).unwrap();
    let ids: Vec<&str> = third
        .changes
        .messages
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["u2"]);
    assert!(third.next.is_none());
    assert!(
        third.changes.clm_kept.is_some(),
        "the last page lists the revisions"
    );
}

/// A message a run's event brought here first, numbered here (P08),
/// takes the owner's number, whatever row had it.
#[test]
fn a_message_numbered_here_takes_the_owners_number() {
    let mut owner = owner();
    let mut mirror = mirror();
    store::append_messages(
        &mut owner,
        ROOT,
        &[
            message("note", "user", "Carried"),
            message("x", "user", "Hi"),
        ],
    )
    .unwrap();
    store::append_messages(&mut mirror, ROOT, &[message("x", "user", "Hi")]).unwrap();

    pull(&mut owner, &mut mirror, PAGE_BYTES);

    assert_eq!(dump(&mirror), dump(&owner));
}

/// What a page carries comes from one moment of the owner's: a pull reads
/// up to its clock when it began.
#[test]
fn a_pull_reads_up_to_where_the_owner_was_when_it_began() {
    let mut owner = owner();
    round(&mut owner, 0);
    let first = changes_since(&owner, ROOT, 0, None, 1).unwrap();
    let upto = first.next.as_ref().unwrap().upto;
    assert_eq!(upto, clock(&owner).unwrap());
    store::append_messages(&mut owner, ROOT, &[message("late", "user", "Later")]).unwrap();

    let mut cursor = first.next;
    let mut seen = Vec::new();
    while let Some(at) = cursor {
        let page = changes_since(&owner, ROOT, 0, Some(&at), 1).unwrap();
        assert_eq!(page.rev, upto);
        seen.extend(page.changes.messages.iter().map(|m| m.id.clone()));
        cursor = page.next;
    }
    assert!(!seen.contains(&"late".to_string()), "{seen:?}");
    let next = changes_since(&owner, ROOT, upto, None, PAGE_BYTES).unwrap();
    let ids: Vec<&str> = next
        .changes
        .messages
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["late"]);
}
