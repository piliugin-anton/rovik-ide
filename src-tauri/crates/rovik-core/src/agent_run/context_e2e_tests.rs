//! Chats of a few turns against the scripted model: what every request of
//! them carries, in Trimming and in CLM mode.
//!
//! What the model needs: the request in view, and every request ending with
//! the step it works from (its latest result, then the IDE's short notes),
//! not with a map of where to start. What the provider's prompt cache needs
//! (prompt_cache.rs): one prefix that only grows from step to step (system
//! prompt, tools, history, each user message as it was first sent, the
//! repository map on the request it was built for), with what changes
//! between steps (the notes) on the last message. What the window needs:
//! every request, notes and reasoning sent back included, leaves the reply
//! its room.

use super::*;
use crate::chat_db::history::DEFAULT_CHARS_PER_TOKEN;

/// The editor context the frontend put before the first request's text
/// (`meta.sentContent`).
const SENT: &str = "[Active editor context — file src/lib.rs (rust); cursor at line 1, col 1. \
                    Call read if you need the actual code.]\n\nWhere is alpha defined?";

/// A request typed with a file open in the editor and lines referenced with
/// "@", as the frontend sends it (ChatPanel's buildWireContent): the editor
/// context and the references' instructions come before what was typed.
const TYPED: &str = "parse.rs:1-3 keeps the spaces around each field; trim them.";
const REFERENCED: &str = concat!(
    "[Active editor context — file src/parse.rs (rust); cursor at line 2, col 5; inside parse_line(). ",
    "Call read if you need the actual code.]\n\n",
    "The user explicitly referenced these project files:\n",
    "- src/parse.rs (lines 1-3, shown in the message as parse.rs:1-3)\n\n",
    "Treat these as explicit file references from the user. Use the available file tools to inspect them when relevant.\n",
    "A reference with lines means the user is talking about exactly those lines. ",
    "Read them with read using that start_line and end_line.\n",
    "To change them, use write with that same start_line and end_line, quoting the current text as old_text ",
    "and its replacement as content: the edit then cannot change anything outside those lines. ",
    "Do not rewrite the whole file.\n\n",
    "parse.rs:1-3 keeps the spaces around each field; trim them."
);
const PARSE: &str = "pub fn parse_line(s: &str) -> Vec<String> {\n    s.split(',').map(|x| x.to_string()).collect()\n}\n";

/// A message's map and the text after it, when it starts with one: the map
/// goes on the request's message, before its text (the map itself has no
/// blank line).
fn map_and_text(text: &str) -> Option<(&str, &str)> {
    text.strip_prefix("## Repository map\n")?;
    text.split_once("\n\n")
}

/// The request's text, without the map that goes before it.
fn without_map(text: &str) -> &str {
    map_and_text(text).map_or(text, |(_, rest)| rest)
}

/// The text of a message: its content, or its content's text parts.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn messages_of(request: &Value) -> &Vec<Value> {
    request["messages"].as_array().unwrap()
}

/// A request's size by the app's own estimate (history.rs, agent.rs): its
/// text (contents, tool calls, reasoning sent back, tool schemas) at the
/// default ratio, as the scripted model reports no usage to learn one from,
/// and the framing of each message after the system prompt.
fn estimated_tokens(request: &Value) -> usize {
    let mut chars = request["tools"].to_string().chars().count();
    for m in messages_of(request) {
        chars += text_of(m).chars().count();
        for call in m["tool_calls"].as_array().into_iter().flatten() {
            chars += call["function"]["name"]
                .as_str()
                .unwrap_or("")
                .chars()
                .count();
            chars += call["function"]["arguments"]
                .as_str()
                .unwrap_or("")
                .chars()
                .count();
        }
        if let Some(r) = m["reasoning_content"].as_str() {
            chars += r.chars().count();
        }
        if let Some(r) = m.get("reasoning_details") {
            chars += r.to_string().chars().count();
        }
    }
    let framing = 4 * messages_of(request).len().saturating_sub(1);
    (chars as f64 / DEFAULT_CHARS_PER_TOKEN).ceil() as usize + framing
}

/// The room kept for the reply before any reply was measured (no effort:
/// 8 000 tokens, at most 40% of the window).
fn reply_room(window: usize) -> usize {
    crate::agent::reply_reserve::reserve("unmeasured|model", None, window, None)
}

/// Every request leaves the reply its room.
fn assert_fits(requests: &[Value], window: usize) {
    for (i, r) in requests.iter().enumerate() {
        let tokens = estimated_tokens(r);
        assert!(
            tokens + reply_room(window) <= window,
            "request {i}: ~{tokens} tokens and {} for the reply don't fit a {window}-token window",
            reply_room(window)
        );
    }
}

/// Where OpenRouter's breakpoint in the history goes (prompt_cache.rs).
fn history_breakpoint(request: &Value) -> Option<usize> {
    let mut hinted = request.clone();
    crate::prompt_cache::apply(&mut hinted, "https://openrouter.ai/api/v1", None);
    messages_of(&hinted)
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, m)| {
            m["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p.get("cache_control").is_some()))
        })
        .map(|(i, _)| i)
}

/// A turn's message: its text, and the form it was sent in if not that.
fn turn(text: &str, sent: Option<&str>) -> (StartKind, Option<String>) {
    let meta = sent.map(|s| json!({ "mode": "agent", "sentContent": s }));
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: text.into(),
        meta,
        attachments: Vec::new(),
    };
    (StartKind::Send { message }, sent.map(str::to_string))
}

/// Runs the turns one after the other in `mode`, with a `window`-token
/// context and `model`; each must end done.
async fn run_turns(
    h: &Harness,
    url: &str,
    mode: &str,
    window: usize,
    model: &str,
    turns: Vec<(StartKind, Option<String>)>,
) {
    let permissions = Permissions {
        checklist: ChecklistMode::Off,
        ..quiet_permissions()
    };
    for (i, (start, sent)) in turns.into_iter().enumerate() {
        h.events.lock().unwrap().clear();
        let mut request = h.request(url, start, permissions.clone());
        request.context_mode = Some(mode.into());
        request.profile.context_tokens = Some(window);
        request.profile.model = model.into();
        request.current_user_content = sent;
        h.start(request).await;
        assert_eq!(h.run_end().await, "done", "turn {}", i + 1);
    }
}

/// CLM needs the virtual volume. Its requests only read and write the
/// files in memory, so a volume that says it's mounted does for them.
fn pretend_volume(h: &Harness) {
    h.core.vfs.pretend_mounted(h._dir.0.join("volume"));
}

/*
 * Trimming
 */

#[tokio::test]
async fn three_turns_send_one_prefix_that_only_grows() {
    let h = harness(&[
        ("src/a.rs", "pub fn alpha_one() {}\n"),
        ("src/lib.rs", "mod a;\n"),
    ])
    .await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "src/a.rs" }),
        },
        Reply::Text("alpha_one is in src/a.rs."),
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "src/b.rs", "content": "pub fn beta_two() {}\n" }),
        },
        Reply::Text("Added src/b.rs."),
        Reply::Call {
            id: "s1",
            name: "search_code",
            args: json!({ "query": "beta_two" }),
        },
        Reply::Text("Only src/b.rs has it."),
    ])
    .await;
    let window = 100_000;
    run_turns(
        &h,
        &url,
        "trimming",
        window,
        "m",
        vec![
            turn("Where is alpha defined?", Some(SENT)),
            turn("Add a beta function", None),
            turn("Where is beta used?", None),
        ],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 6, "two requests a turn");
    let typed = [SENT, "Add a beta function", "Where is beta used?"];
    // The turn's request: the latest user message.
    let request_at = |r: &Value| {
        messages_of(r)
            .iter()
            .rposition(|m| m["role"] == "user")
            .unwrap()
    };
    let map_of = |r: &Value| {
        map_and_text(&text_of(&messages_of(r)[request_at(r)]))
            .map(|(map, _)| map.to_string())
            .expect("a map on the request")
    };

    for (i, r) in requests.iter().enumerate() {
        // The system prompt and the tools never change.
        assert_eq!(system_prompt(r), system_prompt(&requests[0]), "request {i}");
        assert_eq!(r["tools"], requests[0]["tools"], "request {i}");
        // The map goes on the turn's request, before its text; no other
        // message carries one.
        let at = request_at(r);
        let request = text_of(&messages_of(r)[at]);
        assert_eq!(without_map(&request), typed[i / 2], "request {i}");
        assert!(
            messages_of(r)
                .iter()
                .enumerate()
                .all(|(j, m)| j == at || !text_of(m).contains("## Repository map")),
            "request {i}: a map away from the request"
        );
        // The first request goes out as it was first sent, on every turn
        // (its map only in its own turn).
        let first = text_of(&messages_of(r)[1]);
        assert_eq!(without_map(&first), SENT, "request {i}");
        if i >= 2 {
            assert_eq!(first, SENT, "request {i}");
        }
    }
    // A run keeps the map it started with (a rebuild waits until it's two
    // minutes old); the next turn's map has the file written in turn 2.
    assert!(!map_of(&requests[2]).contains("beta_two"));
    assert_eq!(map_of(&requests[3]), map_of(&requests[2]));
    assert!(map_of(&requests[4]).contains("beta_two"));

    // Within a turn, each request repeats the whole of the one before it:
    // the map is in the prefix providers cache. From one turn to the next,
    // everything before the last turn's request, whose map moved on to the
    // new one.
    let users = |r: &Value| {
        messages_of(r)
            .iter()
            .filter(|m| m["role"] == "user")
            .count()
    };
    for (i, pair) in requests.windows(2).enumerate() {
        let shared = shared_messages(&pair[0], &pair[1]);
        let expected = if users(&pair[0]) == users(&pair[1]) {
            messages_of(&pair[0]).len()
        } else {
            request_at(&pair[0])
        };
        assert_eq!(shared, expected, "requests {i}-{}", i + 1);
    }

    // OpenRouter's breakpoint in the history lands on a message the next
    // step sends the same way, not on the last one, which can carry notes;
    // and moves forward a few blocks a step: within the provider's lookback.
    for (i, r) in requests.iter().enumerate() {
        assert_ne!(
            history_breakpoint(r),
            Some(messages_of(r).len() - 1),
            "request {i}: a breakpoint on the last message"
        );
    }
    assert_eq!(
        history_breakpoint(&requests[0]),
        None,
        "nothing to read back yet"
    );
    for (i, pair) in requests.windows(2).enumerate().skip(1) {
        let at = history_breakpoint(&pair[0]).expect("a breakpoint");
        if users(&pair[0]) == users(&pair[1]) {
            assert_eq!(
                messages_of(&pair[0])[at],
                messages_of(&pair[1])[at],
                "request {i}"
            );
        }
        let next = history_breakpoint(&pair[1]).unwrap();
        assert!(
            next >= at && next - at <= 10,
            "request {}: {at} -> {next}",
            i + 1
        );
    }

    assert_fits(&requests, window);
}

#[tokio::test]
async fn three_turns_on_a_small_window_are_trimmed_to_fit() {
    let notes = |c: char| format!("{}\n", c.to_string().repeat(74)).repeat(100); // 7 500 chars
    let h = harness(&[
        ("src/lib.rs", "pub fn alpha_one() {}\n"),
        ("notes/a.txt", &notes('a')),
        ("notes/b.txt", &notes('b')),
    ])
    .await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "notes/a.txt" }),
        },
        Reply::Text("Read a."),
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "notes/c.txt", "content": "c\n" }),
        },
        Reply::Text("Wrote c."),
        Reply::Call {
            id: "r2",
            name: "read",
            args: json!({ "path": "notes/b.txt" }),
        },
        Reply::Text("Read b."),
    ])
    .await;
    // The system prompt, the tools and the map take ~6 800 tokens and the
    // reply 7 200: ~4 000 for the history, which the two reads outgrow.
    let window = 18_000;
    run_turns(
        &h,
        &url,
        "trimming",
        window,
        "m",
        vec![
            turn("Read notes/a.txt", None),
            turn("Write notes/c.txt", None),
            turn("Read notes/b.txt", None),
        ],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 6);
    assert_fits(&requests, window);

    // The last request had to give way: the first turn's read is cut, the
    // latest one is whole, and every user message is there as typed.
    let last = messages_of(&requests[5]);
    let result = |id: &str| {
        last.iter()
            .find(|m| m["tool_call_id"] == id)
            .map(text_of)
            .unwrap()
    };
    assert!(result("r1").contains("[Truncated to fit the context window"));
    assert!(
        result("r2").starts_with(&notes('b')),
        "the latest result is whole"
    );
    let users: Vec<String> = last
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| without_map(&text_of(m)).to_string())
        .collect();
    assert_eq!(
        users,
        ["Read notes/a.txt", "Write notes/c.txt", "Read notes/b.txt"]
    );
    // Before that, nothing was cut.
    assert!(requests[..5].iter().all(|r| messages_of(r)
        .iter()
        .all(|m| !text_of(m).contains("[Truncated"))));
}

/// A message as the chat stored it before the run.
fn stored(role: &str, content: String) -> store::NewMessage {
    store::NewMessage {
        id: uuid::Uuid::new_v4().to_string(),
        role: role.into(),
        content,
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: vec![],
        created_at: 0,
        finalized: true,
    }
}

/// The earlier turn a request's history starts with ("question N"), once
/// the ones before it were dropped.
fn first_turn(request: &Value) -> usize {
    let first = text_of(&messages_of(request)[1]);
    first
        .split("question ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_long_run_over_a_long_chat_keeps_its_cut_until_the_budget() {
    let notes: Vec<(String, String)> = (1..=12)
        .map(|i| {
            (
                format!("notes/{i}.txt"),
                format!("{}\n", format!("note {i} ").repeat(15)).repeat(10),
            )
        })
        .collect();
    let files: Vec<(&str, &str)> = notes
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let h = harness(&files).await;
    // Thirty earlier turns of ~600 tokens: more than the history's budget.
    let earlier: Vec<store::NewMessage> = (1..=30)
        .flat_map(|i| {
            [
                stored("user", format!("question {i} {}", "q".repeat(900))),
                stored("assistant", format!("answer {i} {}", "a".repeat(900))),
            ]
        })
        .collect();
    let (db, id) = (h.core.db.clone(), h.dialog_id.clone());
    db.write(move |c| store::append_messages(c, &id, &earlier).map(|_| ()))
        .await
        .unwrap();
    // A run of twelve reads of ~360 tokens each.
    let mut replies: Vec<Reply> = (1..=12)
        .map(|i| Reply::Call {
            id: Box::leak(format!("r{i}").into_boxed_str()),
            name: "read",
            args: json!({ "path": format!("notes/{i}.txt") }),
        })
        .collect();
    replies.push(Reply::Text("Read them all."));
    let (url, seen) = model(replies).await;
    // The system prompt and the tools take ~5 500 tokens and the reply
    // 8 000: ~10 500 for the history, which the earlier turns outgrow.
    let window = 24_000;
    run_turns(
        &h,
        &url,
        "trimming",
        window,
        "m",
        vec![turn("Read the twelve notes", None)],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 13);
    assert_fits(&requests, window);

    // The earlier turn each request starts with. Once over the budget, the
    // history is cut down to 85% of it and keeps that cut while it fits:
    // the twelve reads (~4 500 tokens) outgrow that room (~1 500) at most
    // three times. Cutting afresh for every request moved it on every
    // step or two: each time the slack the last dropped turn left was used.
    let turns: Vec<usize> = requests.iter().map(first_turn).collect();
    assert!(
        turns.iter().all(|&t| t > 1),
        "earlier turns dropped: {turns:?}"
    );
    let moves = turns.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(moves <= 3, "the cut moved {moves} times: {turns:?}");
    // While it stays, each request repeats the one before it, all of it but
    // at most its last message (where the focus note rides).
    for (i, pair) in requests.windows(2).enumerate() {
        if turns[i] == turns[i + 1] {
            let n = messages_of(&pair[0]).len();
            assert!(
                shared_messages(&pair[0], &pair[1]) >= n - 1,
                "requests {i}-{}",
                i + 1
            );
        }
    }
}

/// A project to fix parse_line in, and a run of `reads` reads of its files
/// (a step each) before the fix, the fix, a read of the result and the
/// final reply.
async fn parse_project(reads: usize) -> (Harness, Vec<Reply>) {
    let h = harness(&[
        (
            "src/lib.rs",
            "mod parse;\nmod report;\npub use parse::parse_line;\n",
        ),
        ("src/parse.rs", PARSE),
        (
            "src/report.rs",
            "use crate::parse_line;\npub fn build_report(s: &str) -> usize { parse_line(s).len() }\n",
        ),
    ])
    .await;
    let files = ["src/lib.rs", "src/parse.rs", "src/report.rs"];
    let mut replies: Vec<Reply> = (0..reads)
        .map(|i| Reply::Call {
            id: Box::leak(format!("r{i}").into_boxed_str()),
            name: "read",
            args: json!({ "path": files[i % files.len()] }),
        })
        .collect();
    replies.push(Reply::Call {
        id: "w1",
        name: "write",
        args: json!({ "path": "src/parse.rs", "content": PARSE.replace("x.to_string()", "x.trim().to_string()") }),
    });
    replies.push(Reply::Call {
        id: "done",
        name: "read",
        args: json!({ "path": "src/parse.rs" }),
    });
    replies.push(Reply::Text("Trimmed."));
    (h, replies)
}

/// The IDE's reminder at the end of a request, if it has one.
fn reminder(request: &Value) -> Option<String> {
    let tail = tail_text(request);
    let at = tail.find("[Reminder from the IDE")?;
    Some(tail[at..].to_string())
}

#[tokio::test]
async fn a_long_run_ends_each_request_with_its_latest_step_and_quotes_the_request_as_typed() {
    let (h, replies) = parse_project(7).await;
    let (url, seen) = model(replies).await;
    run_turns(
        &h,
        &url,
        "trimming",
        100_000,
        "m",
        vec![turn(TYPED, Some(REFERENCED))],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        10,
        "seven reads, the fix, a read, the reply"
    );

    for (i, r) in requests.iter().enumerate() {
        let messages = messages_of(r);
        // The map goes with the request it was built for, before it as the
        // frontend sent it, on every step of the run: the same as on the
        // first, where that message ends the request with the user's words...
        let request = text_of(&messages[1]);
        assert!(
            map_and_text(&request).is_some_and(|(_, text)| text == REFERENCED),
            "request {i}: {request}"
        );
        assert_eq!(
            messages[1],
            messages_of(&requests[0])[1],
            "request {i}: the request is sent the same way on every step"
        );
        // ...and no other message carries one. Every later request ends
        // with the step the model works from: its latest result, and the
        // IDE's reminder once the run is long.
        assert!(
            messages
                .iter()
                .enumerate()
                .all(|(j, m)| j == 1 || !text_of(m).contains("## Repository map")),
            "request {i}: a map away from the request"
        );
        if i > 0 {
            let last = messages.last().unwrap();
            assert_eq!(last["role"], "tool", "request {i}");
            match reminder(r) {
                Some(note) => assert!(note.ends_with("[end of reminder]"), "request {i}: {note}"),
                None => assert!(i < 6, "request {i}: no reminder after six results"),
            }
        }
    }

    // The reminder restates what the user asked, as they typed it: not the
    // editor context and the references' instructions the frontend put
    // before it, which tell the model to go and read the referenced lines.
    let reminders: Vec<String> = requests.iter().filter_map(reminder).collect();
    assert_eq!(reminders.len(), 4, "requests 6 to 9");
    for note in &reminders {
        assert!(note.contains(&format!("> {TYPED}")), "{note}");
        assert!(
            !note.contains("Active editor context") && !note.contains("explicitly referenced"),
            "{note}"
        );
    }

    // Each request repeats the one before it, all of it but the last
    // message when that one carried the reminder: the map is in the
    // prefix providers cache, not in what changes.
    for (i, pair) in requests.windows(2).enumerate() {
        let n = messages_of(&pair[0]).len();
        let changed = usize::from(reminder(&pair[0]).is_some());
        assert_eq!(
            shared_messages(&pair[0], &pair[1]),
            n - changed,
            "requests {i}-{}",
            i + 1
        );
    }
}

#[tokio::test]
async fn a_short_follow_up_is_reminded_of_the_request_it_continues_as_typed() {
    let (h, mut replies) = parse_project(6).await;
    // Turn 1 is a question answered in words; turn 2 ("Go on") the run.
    replies.insert(0, Reply::Text("The spaces come from split(',')."));
    let (url, seen) = model(replies).await;
    let go_on = "[Active editor context — file src/parse.rs (rust); cursor at line 2, col 5. \
                 Call read if you need the actual code.]\n\nGo on";
    run_turns(
        &h,
        &url,
        "trimming",
        100_000,
        "m",
        vec![turn(TYPED, Some(REFERENCED)), turn("Go on", Some(go_on))],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    let note = reminder(requests.last().unwrap()).expect("a reminder");
    assert!(note.contains("The request (quoted):\n> Go on\n"), "{note}");
    assert!(
        note.contains(&format!(
            "The earlier request it continues (quoted):\n> {TYPED}\n"
        )),
        "{note}"
    );
    assert!(!note.contains("Active editor context"), "{note}");

    // Turn 2's map is on turn 2's request; turn 1's goes out as it was
    // first sent, without the map it had in turn 1.
    let messages = messages_of(requests.last().unwrap());
    assert_eq!(text_of(&messages[1]), REFERENCED);
    let request = messages.iter().rposition(|m| m["role"] == "user").unwrap();
    assert!(
        map_and_text(&text_of(&messages[request])).is_some_and(|(_, text)| text == go_on),
        "{:#?}",
        messages[request]
    );
}

/*
 * CLM
 */

#[tokio::test]
async fn three_clm_turns_keep_the_context_file_and_the_prompt_in_step() {
    let h = harness(&[
        ("src/a.rs", "pub fn alpha_one() {}\n"),
        ("src/lib.rs", "mod a;\n"),
    ])
    .await;
    pretend_volume(&h);
    let card = "Goal: add beta_two to src/b.rs\nNEXT: write src/b.rs";
    let note = "[read src/a.rs: alpha_one() is declared on line 1]";
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "src/a.rs" }),
        },
        Reply::Text("alpha_one is in src/a.rs."),
        Reply::Call {
            id: "t1",
            name: "write",
            args: json!({ "path": "@context/TASK.md", "content": card }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "src/b.rs", "content": "pub fn beta_two() {}\n" }),
        },
        Reply::Text("Added src/b.rs."),
        // Turn 3: the agent replaces the read's output with a note in its file.
        Reply::Call {
            id: "e1",
            name: "write",
            args: json!({ "path": "@context/LIVE_CONTEXT.md", "old_text": "pub fn alpha_one() {}", "content": note }),
        },
        Reply::Text("Tidied."),
    ])
    .await;
    let window = 100_000;
    run_turns(
        &h,
        &url,
        "clm",
        window,
        "m",
        vec![
            turn("Where is alpha defined?", Some(SENT)),
            turn("Add a beta function", None),
            turn("Tidy your context", None),
        ],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 7, "turns of 2, 3 and 2 requests");
    let typed = [SENT, "Add a beta function", "Tidy your context"];
    let turn_of = [0, 0, 1, 1, 1, 2, 2];
    // The turn's request: the latest user message that starts with it.
    let request_at = |i: usize| {
        messages_of(&requests[i])
            .iter()
            .rposition(|m| {
                m["role"] == "user" && without_map(&text_of(m)).starts_with(typed[turn_of[i]])
            })
            .unwrap()
    };

    for (i, r) in requests.iter().enumerate() {
        // The CLM section is in the system prompt, the same on every request.
        assert!(system_prompt(r).contains("LIVE_CONTEXT.md"), "request {i}");
        assert_eq!(system_prompt(r), system_prompt(&requests[0]), "request {i}");
        assert_eq!(r["tools"], requests[0]["tools"], "request {i}");
        // The notes (the card, the size readout) end the request; no
        // message before the last has them.
        let (last, earlier) = messages_of(r).split_last().unwrap();
        let tail = text_of(last);
        let readout = tail.find("[context: ~").expect("the size readout");
        assert!(
            earlier.iter().all(|m| {
                let t = text_of(m);
                !t.contains("[context: ~") && !t.contains("[your task card")
            }),
            "request {i}: notes before the last message"
        );
        // The map goes on the turn's request, before its text (and before
        // the notes, when that message is the last); no other has one.
        let at = request_at(i);
        assert!(
            map_and_text(&text_of(&messages_of(r)[at])).is_some(),
            "request {i}"
        );
        assert!(
            messages_of(r)
                .iter()
                .enumerate()
                .all(|(j, m)| j == at || !text_of(m).contains("## Repository map")),
            "request {i}: a map away from the request"
        );
        if let Some(map) = tail.find("## Repository map") {
            assert!(map < readout, "request {i}: the notes come after the map");
        }
        // The card shows once written (after the first step of turn 2).
        assert_eq!(tail.contains(card), i >= 3, "request {i}");
        assert_eq!(tail.contains("not written yet"), i < 3, "request {i}");
    }

    // Until the agent edits its file, each request repeats the one before
    // it but for its last message, within a turn; from one turn to the
    // next, everything before the last turn's request, whose map moved on.
    for (i, pair) in requests[..6].windows(2).enumerate() {
        let expected = if turn_of[i] == turn_of[i + 1] {
            messages_of(&pair[0]).len() - 1
        } else {
            request_at(i)
        };
        assert_eq!(
            shared_messages(&pair[0], &pair[1]),
            expected,
            "requests {i}-{}",
            i + 1
        );
    }

    // The edit applies to the next request: the read's output is the note,
    // its call is plain text now, so the prefix moves once, from that call.
    let (before, after) = (&requests[5], &requests[6]);
    assert_eq!(
        shared_messages(before, after),
        2,
        "system prompt and first request"
    );
    let sent_before: Vec<String> = messages_of(after)[..messages_of(after).len() - 1]
        .iter()
        .map(text_of)
        .collect();
    assert!(
        sent_before.iter().any(|t| t.contains(note)),
        "{sent_before:#?}"
    );
    assert!(
        sent_before
            .iter()
            .all(|t| !t.contains("pub fn alpha_one() {}")),
        "{sent_before:#?}"
    );
    assert!(tail_text(after).contains("[context: edit applied"));
    // The user's messages are all there, as they were sent (the edited
    // output goes in the user role too, under its label).
    let (_, earlier) = messages_of(after).split_last().unwrap();
    let users: Vec<String> = earlier
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| without_map(&text_of(m)).to_string())
        .filter(|t| !t.starts_with("[tool]"))
        .collect();
    assert_eq!(users, typed);

    // The file is the context as sent, so the agent edits what it sees: the
    // read's output is the note (only the call that replaced it quotes it).
    let vfs = &h.core.vfs;
    let file = String::from_utf8(
        vfs.read_file(&crate::clm::context_file(&h.dialog_id))
            .unwrap(),
    )
    .unwrap();
    assert!(file.contains(&format!("~e]]\n{note}\n")), "{file}");
    assert_eq!(file.matches("pub fn alpha_one() {}").count(), 1, "{file}");
    assert!(file.contains("Tidy your context"), "{file}");

    assert_fits(&requests, window);
}

#[tokio::test]
async fn three_clm_turns_on_a_small_window_leave_the_reply_its_room() {
    let notes = |c: char| format!("{}\n", c.to_string().repeat(74)).repeat(60); // 4 500 chars
    let h = harness(&[
        ("src/lib.rs", "pub fn alpha_one() {}\n"),
        ("notes/a.txt", &notes('a')),
        ("notes/b.txt", &notes('b')),
    ])
    .await;
    pretend_volume(&h);
    // DeepSeek gets every tool step's reasoning back on every later turn.
    let thought: &'static str = Box::leak("I should read it first. ".repeat(50).into_boxed_str());
    let card = format!(
        "Goal: read the notes\n{}NEXT: read notes/b.txt",
        "- keep the notes short\n".repeat(100)
    );
    let (url, seen) = model(vec![
        Reply::Thought(
            thought,
            Vec::new(),
            Box::new(Reply::Call {
                id: "r1",
                name: "read",
                args: json!({ "path": "notes/a.txt" }),
            }),
        ),
        Reply::Text("Read a."),
        Reply::Thought(
            thought,
            Vec::new(),
            Box::new(Reply::Call {
                id: "t1",
                name: "write",
                args: json!({ "path": "@context/TASK.md", "content": card }),
            }),
        ),
        Reply::Text("Card written."),
        Reply::Thought(
            thought,
            Vec::new(),
            Box::new(Reply::Call {
                id: "r2",
                name: "read",
                args: json!({ "path": "notes/b.txt" }),
            }),
        ),
        Reply::Text("Read b."),
    ])
    .await;
    // The system prompt with its CLM section, the tools and the map take
    // ~7 700 tokens and the reply 8 000: a few thousand for the context,
    // its task card and the reasoning sent back.
    let window = 20_000;
    run_turns(
        &h,
        &url,
        "clm",
        window,
        "deepseek-test",
        vec![
            turn("Read notes/a.txt", None),
            turn("Keep a task card", None),
            turn("Read notes/b.txt", None),
        ],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 6);
    assert_fits(&requests, window);

    let last = &requests[5];
    // It was tight: the context gave way (outputs withheld, older turns
    // cut) rather than the card, the reasoning or the reply's room.
    let tail = tail_text(last);
    assert!(tail.contains("withheld"), "{tail}");
    assert!(
        tail.contains(card.as_str()),
        "the card goes with every request"
    );
    let steps: Vec<&Value> = messages_of(last)
        .iter()
        .filter(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .collect();
    assert!(
        steps.iter().all(|m| m["reasoning_content"] == thought),
        "every step sent keeps its reasoning: {steps:#?}"
    );
    let read = messages_of(last)
        .iter()
        .find(|m| m["tool_call_id"] == "r2")
        .map(text_of)
        .unwrap();
    assert!(read.starts_with(&notes('b')), "the latest output is whole");
}

#[tokio::test]
async fn a_continued_clm_reply_has_its_notes_on_the_request_to_continue_it() {
    let h = harness(&[("src/lib.rs", "pub fn alpha_one() {}\n")]).await;
    pretend_volume(&h);
    let (url, seen) = model(vec![
        Reply::Text("alpha_one is"),
        Reply::Text(" a function."),
    ])
    .await;
    run_turns(
        &h,
        &url,
        "clm",
        100_000,
        "m",
        vec![
            turn("What is alpha_one?", None),
            (StartKind::ContinueReply, None),
        ],
    )
    .await;
    let requests = seen.lock().unwrap().clone();
    let messages = messages_of(&requests[1]);
    let n = messages.len();
    // The reply goes back as it was written: notes on it would read as the
    // model's own words, to be continued.
    assert_eq!(messages[n - 2]["role"], "assistant");
    assert_eq!(messages[n - 2]["content"], "alpha_one is");
    // The notes follow the request to continue it.
    let tail = text_of(&messages[n - 1]);
    assert!(
        tail.starts_with("Your previous reply was interrupted."),
        "{tail}"
    );
    assert!(tail.contains("[context: ~"), "{tail}");
    // The map stays on the user's request, as on the turn's first request.
    assert!(!tail.contains("## Repository map"), "{tail}");
    let request = text_of(&messages[n - 3]);
    assert!(
        map_and_text(&request).is_some_and(|(_, text)| text == "What is alpha_one?"),
        "{request}"
    );
}
