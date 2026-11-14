//! Preparation uses the existing model, chat tools and stop signal without
//! giving that run implementation authority or command execution.

use super::*;

#[tokio::test]
async fn preparation_host_capability_is_required_for_durable_continuations() {
    let h = harness(&[]).await;
    let unused_url = format!("http://127.0.0.1:{NOTHING_LISTENS}");
    let mut initial = run::new_message("user", "Prepare the plan.");
    initial.meta = Some(json!({"mode":"agent","verifiedPreparation":"issued-preparation"}));
    let mut rows = vec![initial];
    rows.extend((0..510).map(|_| run::new_message("assistant", "Preparation progress.")));
    let mut steering = run::new_message("user", "Please continue.");
    steering.meta = Some(json!({"steer":true}));
    rows.push(steering);
    let dialog = h.dialog_id.clone();
    h.core
        .db
        .write(move |db| store::append_messages(db, &dialog, &rows).map(|_| ()))
        .await
        .unwrap();
    for start in [
        StartKind::Retry,
        StartKind::ContinueReply,
        StartKind::ContinueRun,
        StartKind::Resume,
    ] {
        let request = h.request(&unused_url, start, quiet_permissions());
        assert!(crate::agent_run::requires_preparation(&h.core, &request)
            .await
            .unwrap());
    }
    let ordinary = h.request(
        &unused_url,
        Harness::send("Inspect a different file."),
        quiet_permissions(),
    );
    assert!(!crate::agent_run::requires_preparation(&h.core, &ordinary)
        .await
        .unwrap());
    let dialog = h.dialog_id.clone();
    h.core
        .db
        .write(move |db| {
            store::append_messages(
                db,
                &dialog,
                &[run::new_message("user", "A new ordinary request")],
            )
            .map(|_| ())
        })
        .await
        .unwrap();
    let retry = h.request(&unused_url, StartKind::Retry, quiet_permissions());
    assert!(!crate::agent_run::requires_preparation(&h.core, &retry)
        .await
        .unwrap());
}

async fn private_run(h: &Harness, url: &str) -> (run::Run, run::Entry) {
    let request = h.request(
        url,
        Harness::send("Prepare verification for the plan."),
        quiet_permissions(),
    );
    let StartKind::Send { message } = &request.start else {
        unreachable!()
    };
    let stored = crate::agent_run::stored_message(message);
    let id = h.dialog_id.clone();
    h.core
        .db
        .write(move |db| store::append_messages(db, &id, &[stored]).map(|_| ()))
        .await
        .unwrap();
    h.core.agents.set_config(request.config.clone().unwrap());
    let controls = h.core.agents.register(&h.dialog_id, "p::m", None).unwrap();
    let (mut run, entry) = run::Run::prepare(&h.core, request, controls)
        .await
        .unwrap()
        .unwrap();
    let private = h._dir.0.join("preparation");
    std::fs::create_dir_all(private.join(".rovik-preparation")).unwrap();
    std::fs::write(private.join("source.txt"), "original").unwrap();
    run.root = Some(private.to_string_lossy().into_owned());
    run.preparation = Some(
        crate::agent_run::verified_preparation::Preparation::for_test(h.project.clone(), private),
    );
    (run, entry)
}

#[tokio::test]
async fn preparation_writes_only_drafts_and_text_cannot_claim_completion() {
    let h = harness(&[("source.txt", "original")]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "source",
            name: "write",
            args: json!({"path":"source.txt","content":"implementation"}),
        },
        Reply::Call {
            id: "escape",
            name: "write",
            args: json!({"path":h.project.join("source.txt"),"content":"escaped"}),
        },
        Reply::Call {
            id: "draft",
            name: "write",
            args: json!({"path":".rovik-preparation/recipe.json","content":"{}"}),
        },
        Reply::Text("Everything is implemented and verified."),
    ])
    .await;
    let (mut run, entry) = private_run(&h, &url).await;
    assert_eq!(run.go(entry).await, "error");
    assert_eq!(
        std::fs::read_to_string(h.project.join("source.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(
            run.root
                .as_ref()
                .map(PathBuf::from)
                .unwrap()
                .join("source.txt")
        )
        .unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(h._dir.0.join("preparation/.rovik-preparation/recipe.json"))
            .unwrap(),
        "{}"
    );
    let rows = h.messages().await;
    for id in ["source", "escape"] {
        assert!(rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .call_failed());
    }
    assert!(!rows
        .iter()
        .find(|row| row.tool_call_id.as_deref() == Some("draft"))
        .unwrap()
        .call_failed());
    assert!(rows.iter().all(|row| row
        .meta
        .as_ref()
        .is_none_or(|meta| meta.get("planImplementation").is_none())));
    let requests = seen.lock().unwrap();
    let names: Vec<_> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"prepare_verified_plan"));
    for forbidden in [
        "bash",
        "task",
        "mcp",
        "background",
        "submit_plan",
        "diagnostics",
    ] {
        assert!(!names.contains(&forbidden));
    }
    assert!(
        h.events_of("fileWritten").is_empty(),
        "private drafts must not appear as editor source changes"
    );
}

#[tokio::test]
async fn forged_preparation_commands_and_delegation_never_execute() {
    let h = harness(&[("source.txt", "original")]).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "command",
            name: "bash",
            args: json!({"command":"echo forged"}),
        },
        Reply::Call {
            id: "delegate",
            name: "task",
            args: json!({"kind":"worker","task":"Implement source"}),
        },
        Reply::Call {
            id: "submit",
            name: "prepare_verified_plan",
            args: json!({"input_path":".rovik-preparation/recipe.json","approve":true}),
        },
        Reply::Text("Ready."),
    ])
    .await;
    let (mut run, entry) = private_run(&h, &url).await;
    assert_eq!(run.go(entry).await, "error");
    let rows = h.messages().await;
    for id in ["command", "delegate", "submit"] {
        assert!(rows
            .iter()
            .find(|row| row.tool_call_id.as_deref() == Some(id))
            .unwrap()
            .call_failed());
    }
    assert!(h.events_of("subagentStarted").is_empty());
}

#[tokio::test]
async fn preparation_uses_the_existing_stop_signal() {
    let h = harness(&[("source.txt", "original")]).await;
    let (url, seen) = model(vec![Reply::Hang(None)]).await;
    let (mut run, entry) = private_run(&h, &url).await;
    let task = tokio::spawn(async move { run.go(entry).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while seen.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(agent_stop(&h.core, h.dialog_id.clone()).await);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap(),
        "stopped"
    );
}

#[tokio::test]
async fn ordinary_agent_cannot_submit_a_verified_preparation() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "forged",
            name: "prepare_verified_plan",
            args: json!({"input_path":"recipe.json"}),
        },
        Reply::Text("No preparation was submitted."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Inspect the project"),
        quiet_permissions(),
    ))
    .await;
    h.run_end().await;
    assert!(h
        .messages()
        .await
        .iter()
        .find(|row| row.tool_call_id.as_deref() == Some("forged"))
        .unwrap()
        .call_failed());
    assert!(seen.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(
            |tool| tool.pointer("/function/name").and_then(Value::as_str)
                != Some("prepare_verified_plan")
        ));
}
