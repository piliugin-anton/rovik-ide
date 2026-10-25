//! Runs in which the agent reaches for a restricted file
//! (restricted_files.rs, restricted.rs): it's refused with the reason,
//! before any approval, and nothing of the file reaches the model.

use super::*;

const SECRET: &str = "sk-live-0123456789";

fn env_file() -> String {
    format!("API_KEY={SECRET}\n")
}

/// Everything the model was sent, as one text.
fn sent(seen: &Arc<Mutex<Vec<Value>>>) -> String {
    seen.lock().unwrap().iter().map(Value::to_string).collect()
}

/// Saves the project's list of restricted files, as Settings → Restricted
/// files does, beside its other settings.
fn restrict(h: &Harness, entries: Value) {
    let root = h.project.to_string_lossy().into_owned();
    let config = &h.core.paths.config_dir;
    let mut settings = crate::project_profile::load_project_settings(config, &root);
    if !settings.is_object() {
        settings = json!({});
    }
    settings["restrictedFiles"] = entries;
    crate::project_profile::save_project_settings(config, root, settings).unwrap();
}

#[tokio::test]
async fn reading_an_env_file_is_refused_with_the_reason_however_it_is_tried() {
    let env = env_file();
    let h = harness(&[
        (".env", &env),
        ("src/app.ts", "export const key = process.env.API_KEY;\n"),
    ])
    .await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": ".env" }),
        },
        Reply::Call {
            id: "r2",
            name: "bash",
            args: json!({ "command": "cat .env" }),
        },
        Reply::Call {
            id: "r3",
            name: "search_code",
            args: json!({ "query": "API_KEY", "path_glob": ".env*, *.ts" }),
        },
        Reply::Call {
            id: "r4",
            name: "bash",
            args: json!({ "command": "grep -rn API_KEY ." }),
        },
        Reply::Text("I can't read .env: ask the user."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("What's my API key?"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let read = &result_of(&messages, "r1").content;
    assert!(
        read.starts_with("Error: .env is a restricted file: the agent may not read it"),
        "{read}"
    );
    assert!(read.contains("Settings → Restricted files"), "{read}");
    let cat = &result_of(&messages, "r2").content;
    assert!(
        cat.starts_with("Error: `cat .env` would read .env, a restricted file"),
        "{cat}"
    );
    // The search shows the code's match, and only that the file matched.
    let search = &result_of(&messages, "r3").content;
    assert!(search.contains("src/app.ts"), "{search}");
    assert!(
        search.contains(
            "[1 match in restricted files left out (.env): the agent may not read them.]"
        ),
        "{search}"
    );
    let grep = &result_of(&messages, "r4").content;
    assert!(
        grep.starts_with("Error: `grep -rn API_KEY .` would search .env"),
        "{grep}"
    );
    assert!(
        !sent(&seen).contains(SECRET),
        "the secret never reached the model"
    );
}

#[tokio::test]
async fn a_file_the_user_lets_the_agent_read_is_read() {
    let env = env_file();
    let h = harness(&[(".env", &env)]).await;
    restrict(
        &h,
        json!([{ "path": ".env", "blockRead": false, "blockWrite": true }]),
    );
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": ".env" }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": ".env", "content": "API_KEY=other\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Show my .env"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert_eq!(result_of(&messages, "r1").content, env);
    // Still not its to change.
    assert!(result_of(&messages, "w1")
        .content
        .contains(".env is a restricted file: the agent may not change it"));
    assert_eq!(
        std::fs::read_to_string(h.project.join(".env")).unwrap(),
        env
    );
}

#[tokio::test]
async fn changing_a_restricted_file_is_refused_without_asking_for_approval() {
    let env = env_file();
    let h = harness(&[(".env", &env), ("config/master.key", "KEY\n")]).await;
    let (url, _) = model(vec![
        Reply::Calls(vec![
            (
                "w1",
                "write",
                json!({ "path": ".env", "content": "API_KEY=new\n" }),
            ),
            ("c1", "bash", json!({ "command": "echo X=1 >> .env" })),
            ("c2", "bash", json!({ "command": "rm config/master.key" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    // Edits and commands would wait for the user: these never do.
    let permissions = Permissions {
        allow_any_command: true,
        command_approval: ask_approval("commands").command_approval,
        ..ask_approval("edits")
    };
    h.start(h.request(&url, Harness::send("Reset my settings"), permissions))
        .await;
    assert_eq!(h.run_end().await, "done");

    assert!(h.events_of("approvalRequested").is_empty());
    let messages = h.messages().await;
    for id in ["w1", "c1", "c2"] {
        let result = &result_of(&messages, id).content;
        assert!(result.contains("the agent may not change it"), "{result}");
    }
    assert_eq!(
        std::fs::read_to_string(h.project.join(".env")).unwrap(),
        env
    );
    assert!(h.project.join("config/master.key").exists());
}

#[tokio::test]
async fn the_ai_reviewer_never_sees_an_edit_of_a_file_the_agent_may_not_read() {
    let env = env_file();
    let h = harness(&[(".env", &env)]).await;
    // The user lets the agent write it, not read it.
    restrict(
        &h,
        json!([{ "path": ".env", "blockRead": true, "blockWrite": false }]),
    );
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": ".env", "content": "API_KEY=new\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let (reviewer, reviewed) = model(vec![Reply::Text(
        "{\"decision\": \"approve\", \"reason\": \"Fine.\"}",
    )])
    .await;
    let mut request = h.request(
        &url,
        Harness::send("Set a new key"),
        Permissions {
            edit_approval: permissions::ApprovalRule {
                required: true,
                by: permissions::Approver::Ai,
            },
            ..quiet_permissions()
        },
    );
    let mut profile = request.profile.clone();
    profile.base_url = reviewer;
    profile.label = "Reviewer".into();
    request.config.as_mut().unwrap().approval_profiles = vec![profile];
    h.start(request).await;

    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert!(
        asked["request"]["note"]
            .as_str()
            .unwrap()
            .starts_with("Edits to restricted files"),
        "{asked}"
    );
    agent_resolve_approval(
        &h.core,
        h.dialog_id.clone(),
        "w1".into(),
        ApprovalDecision::Allow,
    )
    .await;
    assert_eq!(h.run_end().await, "done");
    assert!(
        reviewed.lock().unwrap().is_empty(),
        "the reviewer wasn't asked"
    );
    assert_eq!(
        std::fs::read_to_string(h.project.join(".env")).unwrap(),
        "API_KEY=new\n"
    );
}

#[tokio::test]
async fn a_restricted_read_never_runs_beside_the_other_reads() {
    let env = env_file();
    let h = harness(&[(".env", &env), ("a.txt", "alpha\n"), ("b.txt", "beta\n")]).await;
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("c1", "read", json!({ "path": "a.txt" })),
            ("c2", "read", json!({ "path": ".env" })),
            ("c3", "read", json!({ "path": "b.txt" })),
            ("c4", "outline", json!({ "path": "a.txt" })),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(
        parallel::batches_of(&h.dialog_id)
            .iter()
            .all(|batch| !batch.iter().any(|id| id == "c2")),
        "the refused read went through the permission check"
    );
    let messages = h.messages().await;
    let results = tool_results(&messages);
    let order: Vec<&str> = results.iter().map(|(id, _)| *id).collect();
    assert_eq!(order, ["c1", "c2", "c3", "c4"]);
    assert_eq!(results[0].1, "alpha\n");
    assert!(results[1].1.starts_with("Error: .env is a restricted file"));
    assert_eq!(results[2].1, "beta\n");
    assert!(!sent(&seen).contains(SECRET));
}

#[tokio::test]
async fn a_restricted_read_tried_again_pauses_the_run() {
    let env = env_file();
    let h = harness(&[(".env", &env)]).await;
    let read = |id: &'static str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": ".env" }),
    };
    let (url, _) = model(vec![
        read("r1"),
        read("r2"),
        Reply::Text("- I tried to read .env twice; it's restricted."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Read .env"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "paused-loop");
    let messages = h.messages().await;
    let second = &result_of(&messages, "r2").content;
    assert!(
        second.contains("the run was paused here for the user"),
        "{second}"
    );
}

#[tokio::test]
async fn the_repository_map_leaves_out_code_the_user_restricted() {
    let h = harness(&[
        (
            "src/keys.ts",
            "export function stripeKey() { return 'sk_live_abc'; }\n",
        ),
        ("src/app.ts", "export function startApp() {}\n"),
    ])
    .await;
    restrict(
        &h,
        json!([{ "path": "src/keys.ts", "blockRead": true, "blockWrite": true }]),
    );
    // Without the user's entry, the map would show it.
    let focus = crate::repo_map::Focus {
        files: Vec::new(),
        words: vec!["stripeKey".into(), "startApp".into()],
    };
    let unrestricted =
        crate::repo_map::repo_map(&h.project, &focus, &crate::repo_map::Options::default())
            .unwrap();
    assert!(unrestricted.contains("src/keys.ts"), "{unrestricted}");

    let (url, seen) = model(vec![Reply::Text("Done.")]).await;
    h.start(h.request(
        &url,
        Harness::send("Look at stripeKey and startApp"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let sent = sent(&seen);
    assert!(sent.contains("## Repository map"), "the map is sent");
    assert!(sent.contains("src/app.ts"), "{sent}");
    assert!(!sent.contains("src/keys.ts"), "{sent}");
    assert!(!sent.contains("sk_live_abc"), "{sent}");
}

#[tokio::test]
async fn a_refused_write_doesnt_show_a_file_the_agent_may_not_read() {
    let env = env_file();
    let h = harness(&[(".env", &env)]).await;
    // The user lets the agent write it, not read it.
    restrict(
        &h,
        json!([{ "path": ".env", "blockRead": true, "blockWrite": false }]),
    );
    // Another chat changed the file; this one never saw it.
    let other = crate::file_guard::Seen::new("other-chat");
    other.record(&h.project.join(".env"), env.as_bytes());
    let changed = format!("{env}OTHER={SECRET}\n");
    crate::file_guard::write_for_chat(&h.project.join(".env"), changed.as_bytes(), &other).unwrap();
    let (url, seen) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": ".env", "content": "API_KEY=new\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Set a new key"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let refused = &result_of(&messages, "w1").content;
    assert!(
        refused.starts_with("Error: .env changed since you last read it (by another chat); your write was not applied. It's a restricted file"),
        "{refused}"
    );
    assert!(!sent(&seen).contains(SECRET));
    assert_eq!(
        std::fs::read_to_string(h.project.join(".env")).unwrap(),
        changed
    );
}

/// Claude Code with its own tools: its reads and edits of the files are
/// denied by its own rules, and what it asks Rovik about (its shell, its
/// edits) is refused as Rovik's own calls are.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_is_kept_from_restricted_files_too() {
    let env = env_file();
    let h = harness(&[(".env", &env), ("src/app.ts", "const x = 1;\n")]).await;
    let asks = json!([
        ["Bash", { "command": "cat .env" }, "toolu_1"],
        ["Bash", { "command": "grep -rn useState ." }, "toolu_2"],
        ["Write", { "file_path": ".env", "content": "API_KEY=new\n" }, "toolu_3"],
    ]);
    let Some(program) = crate::connectors::fake_claude_asking(&h._dir.0, &asks) else {
        eprintln!("no python3: skipped");
        return;
    };
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    let mut request = h.request(
        "connector://claude-code",
        Harness::send("Look"),
        permissions,
    );
    request.profile.id = "cc::sonnet".into();
    request.profile.model = "sonnet".into();
    request.profile.connector = Some(json!({ "program": program }));
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");

    let decisions: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("decisions.json")).unwrap())
            .unwrap();
    assert_eq!(decisions[0]["behavior"], "deny", "{decisions:#?}");
    let why = decisions[0]["message"].as_str().unwrap();
    assert!(
        why.contains("`cat .env` would read .env, a restricted file"),
        "{why}"
    );
    // A search that prints nothing of it runs.
    assert_eq!(decisions[1]["behavior"], "allow", "{decisions:#?}");
    assert_eq!(decisions[2]["behavior"], "deny", "{decisions:#?}");
    let why = decisions[2]["message"].as_str().unwrap();
    assert!(why.contains("the agent may not change it"), "{why}");
    assert!(h.events_of("approvalRequested").is_empty());

    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(args.contains("--disallowedTools"), "{args}");
    assert!(
        args.contains("Read(./.env)") && args.contains("Edit(./.env)"),
        "{args}"
    );
    assert_eq!(
        std::fs::read_to_string(h.project.join(".env")).unwrap(),
        env
    );
}

/// Claude Code's own shell, in a mode of its own that would let anything
/// run: a command that would read a restricted file is denied before it
/// runs by Rovik's hook, with the reason, and the others go on.
#[cfg(unix)]
#[tokio::test]
async fn claude_codes_own_shell_is_denied_reading_a_restricted_file() {
    let env = env_file();
    let h = harness(&[(".env", &env), ("src/app.ts", "const x = 1;\n")]).await;
    let commands = json!([
        ["cat .env", null, "toolu_1"],
        ["ls src", null, "toolu_2"],
    ]);
    let Some(program) = crate::connectors::fake_claude_with_hooks(&h._dir.0, &commands) else {
        eprintln!("no python3: skipped");
        return;
    };
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    let mut request = h.request("connector://claude-code", Harness::send("Look"), permissions);
    request.profile.id = "cc::sonnet".into();
    request.profile.model = "sonnet".into();
    // As the app resolves a connector running its own tools.
    request.profile.no_tools = true;
    request.profile.connector =
        Some(json!({ "program": program, "permissionMode": "bypassPermissions" }));
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");

    let answers: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("hooks.json")).unwrap())
            .unwrap();
    let denied = &answers[0]["answer"]["hookSpecificOutput"];
    assert_eq!(answers[0]["use"], "toolu_1", "{answers:#?}");
    assert_eq!(denied["hookEventName"], "PreToolUse");
    assert_eq!(denied["permissionDecision"], "deny", "{answers:#?}");
    let why = denied["permissionDecisionReason"].as_str().unwrap();
    assert!(why.contains("`cat .env` would read .env, a restricted file"), "{why}");
    assert_eq!(answers[1]["use"], "toolu_2");
    assert_eq!(answers[1]["answer"], json!({}), "{answers:#?}");
}
