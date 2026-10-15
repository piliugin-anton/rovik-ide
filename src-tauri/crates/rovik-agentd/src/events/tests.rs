//! The events' routes, pace, sizes and ring (P04).

use super::*;
use rovik_core::agent_run::events::{OutMessage, Status};
use serde_json::Value;
use std::time::Duration;

fn events(limits: Limits) -> Arc<Events> {
    let events = Events::with_limits("boot-1".into(), limits);
    events.own("chat", "laptop");
    events
}

/// The frames the computer's connection would send, as JSON, from what
/// its subscription has now.
fn read(sub: &mut Subscription, computer: &str) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(p) = sub.live.try_recv() {
        if p.reaches(computer) {
            out.push(serde_json::from_str(&p.text).unwrap());
        }
    }
    out
}

fn kinds(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .map(|f| match f["ev"]["t"].as_str().unwrap() {
            "agent" => f["ev"]["type"].as_str().unwrap().to_string(),
            other => other.to_string(),
        })
        .collect()
}

fn started(dialog: &str) -> AgentEvent {
    AgentEvent::RunStarted {
        dialog_id: dialog.into(),
        run_id: "r".into(),
    }
}

fn ended(dialog: &str) -> AgentEvent {
    AgentEvent::RunEnded {
        dialog_id: dialog.into(),
        run_id: "r".into(),
        outcome: "done".into(),
    }
}

fn patched(message: &str, patch: Patch) -> AgentEvent {
    AgentEvent::MessagePatched {
        dialog_id: "chat".into(),
        message_id: message.into(),
        patch,
    }
}

fn content(text: &str) -> Patch {
    Patch {
        content: Some(text.into()),
        ..Default::default()
    }
}

fn added(dialog: &str, text: &str) -> AgentEvent {
    AgentEvent::MessagesAdded {
        dialog_id: dialog.into(),
        messages: vec![OutMessage {
            id: "m".into(),
            dialog_id: dialog.into(),
            seq: 1,
            role: "user".into(),
            content: text.into(),
            body_external: false,
            body_bytes: text.len() as i64,
            tool_calls: None,
            tool_call_id: None,
            meta: None,
            error: None,
            failed: false,
            finalized: true,
            attachments: Vec::new(),
            created_at: 1,
            pending: false,
        }],
    }
}

#[test]
fn a_chats_events_go_to_its_computer_and_a_subagents_with_its_lead() {
    let events = events(Limits::default());
    let mut laptop = events.subscribe("laptop", None);
    let mut desk = events.subscribe("desk", None);
    events.agent(&started("chat"));
    events.agent(&AgentEvent::SubagentStarted {
        dialog_id: "chat".into(),
        tool_call_id: "t1".into(),
        child_dialog_id: "kid".into(),
        kind: "scout".into(),
        model: "m".into(),
        goal: "g".into(),
    });
    events.agent(&added("kid", "From the scout"));
    // Nobody's chat, and a write the app would drop: not sent.
    events.agent(&started("unknown"));
    events.agent(&AgentEvent::FileWritten {
        dialog_id: Some("chat".into()),
        abs_path: "/p/a".into(),
        before: None,
        content: "x".into(),
        reviewed: true,
        deleted: false,
    });
    // What applies to the settings goes to every computer.
    events.agent(&route_learned());
    let got = read(&mut laptop, "laptop");
    assert_eq!(
        kinds(&got),
        [
            "runStarted",
            "subagentStarted",
            "messagesAdded",
            "apiRouteLearned"
        ]
    );
    let seqs: Vec<u64> = got.iter().map(|f| f["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    assert_eq!(kinds(&read(&mut desk, "desk")), ["apiRouteLearned"]);
}

/// K28: a long reply's patches, each with the whole text so far, go out
/// four a second at most, and the last has all of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_reply_sends_at_most_four_patches_a_second_the_last_whole() {
    let events = events(Limits::default());
    let mut sub = events.subscribe("laptop", None);
    let reply: String = (0..20_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let collector = tokio::spawn(async move {
        let mut got = Vec::new();
        while let Ok(Ok(p)) =
            tokio::time::timeout(Duration::from_millis(800), sub.live.recv()).await
        {
            got.push((
                Instant::now(),
                serde_json::from_str::<Value>(&p.text).unwrap(),
            ));
        }
        got
    });
    // 200 patches over two seconds.
    let streamed = Instant::now();
    for i in 1..=200 {
        events.agent(&patched("reply", content(&reply[..i * 100])));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let seconds = streamed.elapsed().as_secs_f64();
    let got = collector.await.unwrap();
    assert!(
        got.len() as f64 <= 4.0 * seconds + 2.0,
        "{} patches in {seconds:.1} s",
        got.len()
    );
    for pair in got.windows(2) {
        let gap = pair[1].0.duration_since(pair[0].0);
        assert!(gap >= Duration::from_millis(200), "{gap:?} apart");
    }
    let last = &got.last().unwrap().1;
    assert_eq!(last["ev"]["patch"]["content"], reply.as_str());
}

#[test]
fn a_held_patch_goes_before_its_chats_next_event_merged() {
    let events = events(Limits {
        patch_every: Duration::from_secs(60),
        ..Limits::default()
    });
    let mut sub = events.subscribe("laptop", None);
    events.agent(&patched("reply", content("He")));
    events.agent(&patched(
        "reply",
        Patch {
            status: Some(Some(Status::text("Writing"))),
            ..content("Hel")
        },
    ));
    events.agent(&patched("reply", content("Hello")));
    events.agent(&ended("chat"));
    let got = read(&mut sub, "laptop");
    assert_eq!(
        kinds(&got),
        ["messagePatched", "messagePatched", "runEnded"]
    );
    let held = &got[1]["ev"]["patch"];
    assert_eq!(held["content"], "Hello");
    assert_eq!(held["status"]["text"], "Writing", "{held}");
}

#[test]
fn an_event_too_big_for_a_frame_goes_as_resync_for_its_chat() {
    let events = events(Limits::default());
    let mut sub = events.subscribe("laptop", None);
    events.agent(&AgentEvent::SubagentStarted {
        dialog_id: "chat".into(),
        tool_call_id: "t1".into(),
        child_dialog_id: "kid".into(),
        kind: "scout".into(),
        model: "m".into(),
        goal: "g".into(),
    });
    let big = "x".repeat(2 << 20);
    events.agent(&added("chat", &big));
    events.agent(&added("kid", &big));
    // One about no chat has nothing to resync: it isn't sent.
    events.agent(&AgentEvent::ReasoningLearned {
        profile_id: big.clone(),
        accepted: Vec::new(),
        rejected: Vec::new(),
        unresolved: Vec::new(),
        reasons: false,
    });
    let got = read(&mut sub, "laptop");
    assert_eq!(kinds(&got), ["subagentStarted", "resync", "resync"]);
    assert_eq!(got[1]["ev"]["dialogId"], "chat");
    assert_eq!(got[2]["ev"]["dialogId"], "chat", "the lead's chat");
}

#[test]
fn a_client_back_with_its_since_gets_what_it_missed() {
    let events = events(Limits::default());
    let mut first = events.subscribe("laptop", None);
    events.agent(&started("chat"));
    events.agent(&added("chat", "Hi"));
    let seen = read(&mut first, "laptop");
    let since = seen.last().unwrap()["seq"].as_u64().unwrap();
    drop(first);
    events.agent(&ended("chat"));
    events.agent(&route_learned());

    let back = events.subscribe("laptop", Some(("boot-1".into(), since)));
    assert!(back.complete);
    let missed: Vec<Value> = back
        .replay
        .iter()
        .map(|p| serde_json::from_str(&p.text).unwrap())
        .collect();
    assert_eq!(kinds(&missed), ["runEnded", "apiRouteLearned"]);
    // From another process, or with no since: the database has it.
    let stale = events.subscribe("laptop", Some(("boot-0".into(), since)));
    assert!(!stale.complete && stale.replay.is_empty());
    assert!(!events.subscribe("laptop", None).complete);
    // Another computer gets none of this one's.
    let desk = events.subscribe("desk", Some(("boot-1".into(), since)));
    assert_eq!(desk.replay.len(), 1, "only what's everyone's");
}

fn route_learned() -> AgentEvent {
    AgentEvent::ApiRouteLearned {
        profile_id: "p::m".into(),
        route: "responses".into(),
    }
}

#[test]
fn once_the_ring_has_wrapped_complete_is_false() {
    let events = events(Limits {
        events: 5,
        ..Limits::default()
    });
    for _ in 0..10 {
        events.agent(&started("chat"));
    }
    assert!(
        !events
            .subscribe("laptop", Some(("boot-1".into(), 2)))
            .complete
    );
    let recent = events.subscribe("laptop", Some(("boot-1".into(), 6)));
    assert!(recent.complete);
    assert_eq!(recent.replay.len(), 4);

    // By size too.
    let events = events_with_bytes(2_000);
    for _ in 0..10 {
        events.agent(&added("chat", &"x".repeat(500)));
    }
    assert!(
        !events
            .subscribe("laptop", Some(("boot-1".into(), 1)))
            .complete
    );
    let recent = events.subscribe("laptop", Some(("boot-1".into(), 9)));
    assert!(recent.complete);
    assert_eq!(recent.replay.len(), 1);
}

fn events_with_bytes(bytes: usize) -> Arc<Events> {
    events(Limits {
        bytes,
        ..Limits::default()
    })
}

/// A long reply doesn't push everything else out of the ring: its patches
/// are one entry there, which has all they did.
#[test]
fn the_ring_keeps_one_patch_per_message() {
    let events = events(Limits {
        patch_every: Duration::ZERO,
        ..Limits::default()
    });
    events.agent(&started("chat"));
    events.agent(&patched(
        "reply",
        Patch {
            status: Some(Some(Status::text("Writing"))),
            ..content("He")
        },
    ));
    for text in ["Hel", "Hell", "Hello"] {
        events.agent(&patched("reply", content(text)));
    }
    let all = events.subscribe("laptop", Some(("boot-1".into(), 0)));
    let kept: Vec<Value> = all
        .replay
        .iter()
        .map(|p| serde_json::from_str(&p.text).unwrap())
        .collect();
    assert_eq!(kinds(&kept), ["runStarted", "messagePatched"]);
    assert_eq!(kept[1]["ev"]["patch"]["content"], "Hello");
    assert_eq!(kept[1]["ev"]["patch"]["status"]["text"], "Writing");
    assert_eq!(kept[1]["seq"], 5, "under the newest patch's seq");
}
