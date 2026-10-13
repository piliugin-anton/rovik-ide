//! Store and search tests against a real SQLite database (in memory).

use super::schema;
use super::search::{parse_query, search};
use super::store::*;
use rusqlite::Connection;

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::configure(&conn, true).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn
}

const DAY: i64 = 86_400_000;

fn dialog(conn: &mut Connection, id: &str, title: &str, age_days: i64) {
    let t = now_ms() - age_days * DAY;
    create_dialog(
        conn,
        &NewDialog {
            id: id.into(),
            title: title.into(),
            created_at: t,
            updated_at: t,
            project_id: None,
        },
    )
    .unwrap();
    // Keep the intended age: appends move updated_at forward.
    conn.execute(
        "UPDATE dialogs SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![id, t],
    )
    .unwrap();
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
        created_at: now_ms(),
        finalized: true,
    }
}

fn add(conn: &mut Connection, dialog_id: &str, messages: Vec<NewMessage>) {
    let age: i64 = conn
        .query_row(
            "SELECT updated_at FROM dialogs WHERE id = ?1",
            [dialog_id],
            |r| r.get(0),
        )
        .unwrap();
    append_messages(conn, dialog_id, &messages).unwrap();
    conn.execute(
        "UPDATE dialogs SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![dialog_id, age],
    )
    .unwrap();
}

/// Renders highlights as [..] using UTF-16 ranges, like the frontend does.
fn mark(text: &str, ranges: &[[usize; 2]]) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = String::new();
    let mut at = 0;
    for [s, e] in ranges {
        out.push_str(&String::from_utf16_lossy(&units[at..*s]));
        out.push('[');
        out.push_str(&String::from_utf16_lossy(&units[*s..*e]));
        out.push(']');
        at = *e;
    }
    out.push_str(&String::from_utf16_lossy(&units[at..]));
    out
}

#[test]
fn paging_dialogs_by_cursor() {
    let mut conn = db();
    for i in 0..7 {
        dialog(&mut conn, &format!("d{i}"), &format!("Chat {i}"), i);
    }
    let first = list_dialogs(&conn, None, None, 3).unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        ["d0", "d1", "d2"]
    );
    let second = list_dialogs(&conn, None, first.next_cursor.as_ref(), 3).unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        ["d3", "d4", "d5"]
    );
    let third = list_dialogs(&conn, None, second.next_cursor.as_ref(), 3).unwrap();
    assert_eq!(third.items.len(), 1);
    assert!(third.next_cursor.is_none());
}

#[test]
fn large_tool_output_moves_out_and_loads_on_demand() {
    let mut conn = db();
    dialog(&mut conn, "d", "Tools", 0);
    let big = "line of output\n".repeat(1000); // ~15 KB
    let mut t = message("t1", "tool", &big);
    t.tool_call_id = Some("c1".into());
    add(&mut conn, "d", vec![message("u1", "user", "read it"), t]);

    let page = get_messages(&conn, "d", None, 50).unwrap();
    let tool = page.messages.iter().find(|m| m.id == "t1").unwrap();
    assert!(tool.body_external);
    assert_eq!(tool.body_bytes as usize, big.len());
    assert_eq!(tool.content.chars().count(), PREVIEW_CHARS);
    assert_eq!(get_message_body(&conn, "t1").unwrap(), big);

    // A long assistant reply is never cut: it is what the chat shows.
    let long_reply = "word ".repeat(2000);
    add(
        &mut conn,
        "d",
        vec![message("a1", "assistant", &long_reply)],
    );
    let reply = get_messages(&conn, "d", None, 50)
        .unwrap()
        .messages
        .pop()
        .unwrap();
    assert!(!reply.body_external);
    assert_eq!(reply.content, long_reply);
}

/// A flag set on a finalized tool message runs finalize again: the body
/// that moved out must stay, and its size must stay the body's.
#[test]
fn finalizing_an_external_body_again_keeps_it() {
    let mut conn = db();
    dialog(&mut conn, "d", "Tools", 0);
    // Multibyte: the preview's 2048 characters are more than 4096 bytes.
    let big = "строка вывода\n".repeat(600);
    let mut t = message("t1", "tool", &big);
    t.tool_call_id = Some("c1".into());
    add(&mut conn, "d", vec![t]);

    let patch = MessagePatch {
        error: Some("boom".into()),
        ..Default::default()
    };
    update_message(&mut conn, "t1", &patch).unwrap();

    let tool = get_messages(&conn, "d", None, 50)
        .unwrap()
        .messages
        .pop()
        .unwrap();
    assert!(tool.body_external && tool.finalized);
    assert_eq!(tool.body_bytes as usize, big.len());
    assert_eq!(tool.content.chars().count(), PREVIEW_CHARS);
    assert_eq!(get_message_body(&conn, "t1").unwrap(), big);
}

#[test]
fn streaming_message_is_searchable_only_once_finalized() {
    let mut conn = db();
    dialog(&mut conn, "d", "Streaming", 0);
    let mut pending = message("a1", "assistant", "");
    pending.finalized = false;
    add(
        &mut conn,
        "d",
        vec![message("u1", "user", "hello"), pending],
    );

    update_message(
        &mut conn,
        "a1",
        &MessagePatch {
            content: Some("zebra crossing".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        search(&conn, "zebra", None, 10).unwrap().is_empty(),
        "checkpoints are not indexed"
    );

    update_message(
        &mut conn,
        "a1",
        &MessagePatch {
            finalize: true,
            ..Default::default()
        },
    )
    .unwrap();
    let hits = search(&conn, "zebra", None, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id.as_deref(), Some("a1"));
    assert_eq!(
        get_dialog(&conn, "d").unwrap().unwrap().preview.as_deref(),
        Some("zebra crossing")
    );
}

#[test]
fn search_matches_the_old_in_memory_behaviour() {
    let mut conn = db();
    dialog(&mut conn, "a", "Tauri window events", 1);
    add(
        &mut conn,
        "a",
        vec![message(
            "a1",
            "user",
            "How do I listen to window close in tauri?",
        )],
    );
    dialog(&mut conn, "b", "Borsh layout question", 3);
    add(
        &mut conn,
        "b",
        vec![message(
            "b1",
            "user",
            "My IDL_ACCOUNT_LAYOUT fails with tauri plugin",
        )],
    );
    dialog(&mut conn, "c", "Настройка прокси", 2);
    add(
        &mut conn,
        "c",
        vec![message("c1", "user", "Как настроить прокси в reqwest?")],
    );
    dialog(&mut conn, "d", "Café menu", 5);
    add(
        &mut conn,
        "d",
        vec![message("d1", "user", "résumé formatting help")],
    );
    dialog(&mut conn, "f", "Tool noise", 1);
    let mut noise = message("f1", "tool", "tauri tauri tauri tauri");
    noise.tool_call_id = Some("x".into());
    add(
        &mut conn,
        "f",
        vec![message("f0", "user", "read the file"), noise],
    );

    let ids = |q: &str| {
        search(&conn, q, None, 10)
            .unwrap()
            .into_iter()
            .map(|h| h.dialog.id)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        ids("tauri"),
        ["a", "b"],
        "title hit first; tool output never matches"
    );
    assert_eq!(ids("tauri window"), ["a"], "every word must match");
    assert_eq!(
        ids("ПРОКСИ reqwest"),
        ["c"],
        "Cyrillic case-folding; words across title and message"
    );
    assert_eq!(ids("cafe"), ["d"], "accents folded");
    assert_eq!(ids("resume"), ["d"]);
    assert_eq!(ids("tauir"), ["a"], "typo fallback on titles");
    assert_eq!(ids("Borhs"), ["b"]);
    assert!(ids("tauri xyz").is_empty());

    let hit = search(&conn, "ПРОКСИ reqwest", None, 10).unwrap().remove(0);
    let snippet = hit.snippet.expect("message snippet");
    assert_eq!(
        mark(&snippet.text, &snippet.ranges),
        "Как настроить [прокси] в [reqwest]?"
    );
}

#[test]
fn quoted_phrases_and_short_terms() {
    let terms = parse_query(r#"  "window events"  a tauri TAURI "#);
    assert_eq!(terms.len(), 2, "1-char terms and duplicates dropped");
    assert!(terms[0].phrase && terms[0].text == "window events");
    assert!(!terms[1].phrase && terms[1].text == "tauri");
}

#[test]
fn failed_messages_leave_the_index_and_deletes_clean_up() {
    let mut conn = db();
    dialog(&mut conn, "d", "Pancakes", 0);
    add(
        &mut conn,
        "d",
        vec![message("u1", "user", "blueberry recipe")],
    );
    assert_eq!(search(&conn, "blueberry", None, 10).unwrap().len(), 1);

    update_message(
        &mut conn,
        "u1",
        &MessagePatch {
            failed: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(search(&conn, "blueberry", None, 10).unwrap().is_empty());

    delete_dialog(&mut conn, "d").unwrap();
    assert!(search(&conn, "pancakes", None, 10).unwrap().is_empty());
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_rows", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    let msgs: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msgs, 0, "messages cascade with the chat");
}

#[test]
fn rename_replaces_the_title_row() {
    let mut conn = db();
    dialog(&mut conn, "d", "Old name", 0);
    rename_dialog(&mut conn, "d", "Fresh title").unwrap();
    assert!(search(&conn, "old", None, 10).unwrap().is_empty());
    assert_eq!(search(&conn, "fresh", None, 10).unwrap().len(), 1);
}

#[test]
fn answered_and_around() {
    let mut conn = db();
    dialog(&mut conn, "d", "Q&A", 0);
    let mut errored = message("a1", "assistant", "");
    errored.error = Some("network".into());
    add(
        &mut conn,
        "d",
        vec![
            message("u1", "user", "first"),
            errored,
            message("u2", "user", "second"),
            message("a2", "assistant", "done"),
        ],
    );
    assert_eq!(
        messages_answered(&conn, &["u1".into(), "u2".into()]).unwrap(),
        ["u1", "u2"]
    );

    dialog(&mut conn, "e", "Unanswered", 0);
    add(&mut conn, "e", vec![message("e1", "user", "hello?")]);
    assert!(messages_answered(&conn, &["e1".into()]).unwrap().is_empty());

    let around = get_messages_around(&conn, "u2", 1, 1).unwrap();
    assert_eq!(
        around
            .messages
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["a1", "u2", "a2"]
    );
    assert!(around.has_older);
    assert!(!around.has_newer);
}

#[test]
fn attachments_are_listed_in_order_and_collected_when_unreferenced() {
    let mut conn = db();
    let dir = std::env::temp_dir().join(format!("chat-db-att-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(super::attachments::DIR)).unwrap();
    let (a, b) = ("a".repeat(64), "b".repeat(64));
    for id in [&a, &b] {
        std::fs::write(
            dir.join(super::attachments::DIR).join(format!("{id}.jpg")),
            b"x",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attachments (id, mime, bytes, created_at) VALUES (?1, 'image/jpeg', 1, 0)",
            [id],
        )
        .unwrap();
    }
    dialog(&mut conn, "d", "Shots", 0);
    let mut shot = message("t1", "tool", "Screenshot");
    shot.tool_call_id = Some("c".into());
    shot.attachments = vec![a.clone()];
    add(&mut conn, "d", vec![message("u1", "user", "look"), shot]);

    let page = get_messages(&conn, "d", None, 10).unwrap();
    assert_eq!(page.messages[1].attachments, [a.clone()]);

    // b is unreferenced and past its grace period; a is in use.
    assert_eq!(super::attachments::collect_garbage(&conn, &dir).unwrap(), 1);
    assert!(super::attachments::file_for(&dir, &a).is_some());
    assert!(super::attachments::file_for(&dir, &b).is_none());

    delete_dialog(&mut conn, "d").unwrap();
    assert_eq!(
        super::attachments::collect_garbage(&conn, &dir).unwrap(),
        1,
        "freed by the delete"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn composer_drafts_round_trip_and_clean_up() {
    let mut conn = db();
    assert_eq!(get_composer_draft(&conn, "d").unwrap(), None);
    dialog(&mut conn, "d", "Drafts", 0);

    save_composer_draft(&mut conn, "d", "half-typed message").unwrap();
    assert_eq!(
        get_composer_draft(&conn, "d").unwrap().as_deref(),
        Some("half-typed message")
    );

    // Overwrites, one row per chat.
    save_composer_draft(&mut conn, "d", "edited text").unwrap();
    assert_eq!(
        get_composer_draft(&conn, "d").unwrap().as_deref(),
        Some("edited text")
    );
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM composer_drafts WHERE dialog_id = 'd'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);

    // Blank text deletes the row.
    save_composer_draft(&mut conn, "d", "   ").unwrap();
    assert_eq!(get_composer_draft(&conn, "d").unwrap(), None);

    // Segment JSON is opaque to the store and round-trips as-is; an empty
    // segment list deletes the row like blank text does.
    let segments = r#"[{"type":"text","text":"hello "},{"type":"file","file":{"id":"f","path":"src/f.ts","relativePath":"src/f.ts","name":"f.ts"}},{"type":"pasted","text":"long paste"}]"#;
    save_composer_draft(&mut conn, "d", segments).unwrap();
    assert_eq!(
        get_composer_draft(&conn, "d").unwrap().as_deref(),
        Some(segments)
    );
    save_composer_draft(&mut conn, "d", "[]").unwrap();
    assert_eq!(get_composer_draft(&conn, "d").unwrap(), None);

    // Missing chats are ignored, and drafts die with their chat.
    save_composer_draft(&mut conn, "ghost", "boo").unwrap();
    assert_eq!(get_composer_draft(&conn, "ghost").unwrap(), None);
    save_composer_draft(&mut conn, "d", "back again").unwrap();
    delete_dialog(&mut conn, "d").unwrap();
    assert_eq!(get_composer_draft(&conn, "d").unwrap(), None);
}

fn child(conn: &mut Connection, id: &str, parent: &str, call: &str) {
    create_child_dialog(
        conn,
        &NewChildDialog {
            id: id.into(),
            parent_id: parent.into(),
            parent_call_id: call.into(),
            kind: "scout".into(),
            title: "Scout".into(),
            created_at: now_ms(),
        },
    )
    .unwrap();
}

fn create_found_child(conn: &mut Connection) -> String {
    let id = "3f2a9c1d-0000-4000-8000-000000000000".to_string();
    child(conn, &id, "p", "call-9");
    id
}

#[test]
fn subagent_chats_stay_out_of_lists_and_search() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k", "p", "call-1");
    add(
        &mut conn,
        "k",
        vec![message("k1", "assistant", "blueberry evidence")],
    );

    let listed: Vec<String> = list_dialogs(&conn, None, None, 50)
        .unwrap()
        .items
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(listed, ["p"]);
    assert!(!dialogs_in_project(&conn, None).unwrap().contains("k"));
    assert!(search(&conn, "blueberry", None, 10).unwrap().is_empty());
    assert!(search(&conn, "scout", None, 10).unwrap().is_empty());
    // P6.7: not even indexed.
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM search_rows WHERE dialog_id = 'k'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);

    let found = child_for_call(&conn, "p", "call-1").unwrap().unwrap();
    assert_eq!(found.id, "k");
    assert_eq!(found.kind, "scout");
    assert_eq!(found.parent_id, "p");
    assert_eq!(child_for_call(&conn, "p", "call-2").unwrap(), None);
    // Found by the handle a report gives ("kind-" and the id's start) or the call.
    let id = create_found_child(&mut conn);
    let handle = format!("scout-{}", &id[..8]);
    assert_eq!(
        child_by_handle(&conn, "p", &handle).unwrap().unwrap().id,
        id
    );
    assert_eq!(
        child_by_handle(&conn, "p", "call-1").unwrap().unwrap().id,
        "k"
    );
    assert_eq!(child_by_handle(&conn, "p", "scout-zzzzzzzz").unwrap(), None);
    assert_eq!(child_by_handle(&conn, "other", &handle).unwrap(), None);
    assert!(
        create_child_dialog(
            &mut conn,
            &NewChildDialog {
                id: "orphan".into(),
                parent_id: "missing".into(),
                parent_call_id: "c".into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: 0,
            },
        )
        .is_err(),
        "a child needs its parent"
    );
}

#[test]
fn deleting_a_chat_deletes_its_subagents_and_their_search_rows() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k", "p", "call-1");
    add(
        &mut conn,
        "k",
        vec![message("k1", "assistant", "blueberry")],
    );

    delete_dialog(&mut conn, "p").unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM dialogs"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM search_rows"), 0);
}

#[test]
fn a_chats_subagents_are_found_deepest_first() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k1", "p", "call-1");
    child(&mut conn, "k2", "p", "call-2");
    child(&mut conn, "g", "k1", "call-3");
    let mut found = descendants(&conn, "p").unwrap();
    let g = found.iter().position(|c| c == "g").unwrap();
    let k1 = found.iter().position(|c| c == "k1").unwrap();
    assert!(g < k1, "a grandchild before its parent");
    found.sort();
    assert_eq!(found, ["g", "k1", "k2"]);
    assert!(descendants(&conn, "k2").unwrap().is_empty());
}

/// Round 2, P6.7: parent links that loop (a damaged database) end.
#[test]
fn descendants_terminate_on_a_cycle() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k1", "p", "call-1");
    child(&mut conn, "g", "k1", "call-2");
    conn.execute("UPDATE dialogs SET parent_id = 'g' WHERE id = 'p'", [])
        .unwrap();
    let mut found = descendants(&conn, "p").unwrap();
    found.sort();
    assert_eq!(found, ["g", "k1"]);
}

/// Round 3: deleting a chat whose parent links loop ends, and leaves none
/// of the three chats or their search rows.
#[test]
fn deleting_a_chat_with_looping_parent_links_ends() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k1", "p", "call-1");
    child(&mut conn, "g", "k1", "call-2");
    conn.execute("UPDATE dialogs SET parent_id = 'g' WHERE id = 'p'", [])
        .unwrap();
    delete_dialog(&mut conn, "p").unwrap();
    let dialogs: i64 = conn
        .query_row("SELECT COUNT(*) FROM dialogs", [], |r| r.get(0))
        .unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_rows", [], |r| r.get(0))
        .unwrap();
    assert_eq!((dialogs, rows), (0, 0));
}

/// Round 2 (section 4.2): a handle shared by two subagents' ids finds the
/// newest; another chat's call id finds nothing here; upper-case hex finds
/// it too (SQLite's LIKE folds ASCII case).
#[test]
fn child_by_handle_prefers_the_newest_and_scopes_to_the_parent() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    dialog(&mut conn, "q", "Quiche", 0);
    let make = |conn: &mut Connection, id: &str, parent: &str, call: &str, at: i64| {
        create_child_dialog(
            conn,
            &NewChildDialog {
                id: id.into(),
                parent_id: parent.into(),
                parent_call_id: call.into(),
                kind: "scout".into(),
                title: "Scout".into(),
                created_at: at,
            },
        )
        .unwrap();
    };
    make(
        &mut conn,
        "abcdef12-0000-4000-8000-000000000001",
        "p",
        "call-1",
        1_000,
    );
    make(
        &mut conn,
        "abcdef12-0000-4000-8000-000000000002",
        "p",
        "call-2",
        2_000,
    );
    make(
        &mut conn,
        "99999999-0000-4000-8000-000000000003",
        "q",
        "call-q",
        3_000,
    );
    let found = child_by_handle(&conn, "p", "scout-abcdef12")
        .unwrap()
        .unwrap();
    assert_eq!(found.id, "abcdef12-0000-4000-8000-000000000002");
    assert!(child_by_handle(&conn, "p", "call-q").unwrap().is_none());
    assert!(child_by_handle(&conn, "p", "scout-99999999")
        .unwrap()
        .is_none());
    let upper = child_by_handle(&conn, "p", "scout-ABCDEF12")
        .unwrap()
        .unwrap();
    assert_eq!(upper.id, found.id);
    // By the call that started it, in this chat.
    let by_call = child_by_handle(&conn, "p", "call-1").unwrap().unwrap();
    assert_eq!(by_call.id, "abcdef12-0000-4000-8000-000000000001");
}

/// Round 2 (§4.2), P6.7: deleting a project with two chats, each with two
/// subagents, leaves no chat, message or search row behind.
#[test]
fn deleting_a_project_deletes_its_chats_subagents_and_search_rows() {
    let mut conn = db();
    conn.execute(
        "INSERT INTO projects (id, name, path, created_at, last_opened_at)
         VALUES ('proj', 'Kitchen', '/kitchen', 0, 0)",
        [],
    )
    .unwrap();
    for chat in ["c1", "c2"] {
        create_dialog(
            &mut conn,
            &NewDialog {
                id: chat.into(),
                title: format!("Pancakes {chat}"),
                created_at: now_ms(),
                updated_at: now_ms(),
                project_id: Some("proj".into()),
            },
        )
        .unwrap();
        add(
            &mut conn,
            chat,
            vec![message(&format!("{chat}-u"), "user", "blueberry recipe")],
        );
        for n in 1..=2 {
            let kid = format!("{chat}-k{n}");
            child(&mut conn, &kid, chat, &format!("call-{n}"));
            add(
                &mut conn,
                &kid,
                vec![message(
                    &format!("{kid}-m"),
                    "assistant",
                    "blueberry evidence",
                )],
            );
        }
    }
    let in_project: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dialogs WHERE project_id = 'proj'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_project, 6, "subagents are in their chat's project");

    super::projects::delete(&mut conn, "proj").unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM dialogs"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM search_rows"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM search_index"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM title_trigrams"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM projects"), 0);
}

/// Round 2 (§4.2), P6.7: pinned: a subagent's message finalised while it
/// had a parent stays out of search when the link goes (only a damaged
/// database loses it): nothing re-indexes it, nor does a later update
/// that doesn't finalise it again.
#[test]
fn a_subagents_message_stays_unindexed_after_its_parent_link_goes() {
    let mut conn = db();
    dialog(&mut conn, "p", "Pancakes", 0);
    child(&mut conn, "k", "p", "call-1");
    let mut pending = message("k1", "assistant", "");
    pending.finalized = false;
    add(&mut conn, "k", vec![pending]);
    update_message(
        &mut conn,
        "k1",
        &MessagePatch {
            content: Some("blueberry evidence".into()),
            finalize: true,
            ..Default::default()
        },
    )
    .unwrap();
    conn.execute("UPDATE dialogs SET parent_id = NULL WHERE id = 'k'", [])
        .unwrap();
    update_message(
        &mut conn,
        "k1",
        &MessagePatch {
            meta: Some(serde_json::json!({ "note": 1 })),
            ..Default::default()
        },
    )
    .unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM search_rows WHERE dialog_id = 'k'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert!(search(&conn, "blueberry", None, 10).unwrap().is_empty());
}

#[test]
fn chat_model_round_trips() {
    let mut conn = db();
    dialog(&mut conn, "d", "Model", 0);
    assert_eq!(chat_model(&conn, "d").unwrap(), None, "none until saved");

    let model = ChatModel {
        provider_id: "openrouter".into(),
        model_id: "anthropic/claude-sonnet-4.5".into(),
        effort: Some("high".into()),
    };
    set_chat_model(&conn, "d", &model).unwrap();
    assert_eq!(chat_model(&conn, "d").unwrap(), Some(model));

    // Replaced, effort may go back to unset.
    let other = ChatModel {
        provider_id: "ollama".into(),
        model_id: "qwen3".into(),
        effort: None,
    };
    set_chat_model(&conn, "d", &other).unwrap();
    assert_eq!(chat_model(&conn, "d").unwrap(), Some(other.clone()));

    // Missing chats are ignored.
    set_chat_model(&conn, "ghost", &other).unwrap();
    assert_eq!(chat_model(&conn, "ghost").unwrap(), None);
}
