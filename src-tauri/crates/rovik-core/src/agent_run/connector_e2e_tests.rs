//! Whole runs on a connector's model. In Agent mode Claude Code runs its
//! own tools (a stand-in, connectors::fake_claude_asking, asks Rovik before
//! them); in Plan mode and Autopilot it's the model behind Rovik's agent
//! loop (connectors::fake_claude_with_mcp), and its calls are Rovik's tool
//! calls, run by Rovik.

#[cfg(unix)]
use super::permissions::{ApprovalRule, Approver};
use super::*;

/// Plan mode on Claude Code, whatever its permission mode: it's the model
/// behind Rovik's agent. Its call is the reply's, Rovik runs it, and Claude
/// Code, waiting on it over MCP, gets the result and writes its reply.
#[cfg(unix)]
#[tokio::test]
async fn in_plan_mode_claude_code_is_rovik_s_agent_and_rovik_runs_its_calls() {
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let read = json!({ "path": "src/alpha.rs" });
    let Some(program) = crate::connectors::fake_claude_with_mcp(&h._dir.0, "read", &read) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_send(
        &h,
        &program,
        "plan",
        "What does alpha return?",
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(
        roles,
        ["user", "assistant", "tool", "assistant"],
        "{messages:#?}"
    );
    // The call is the reply's, as Rovik stores a model's.
    let calls = messages[1].tool_calls.clone().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].name, "read");
    assert_eq!(calls[0].arguments, "{\"path\": \"src/alpha.rs\"}");
    // Rovik ran it: its result, as its `read` gives it.
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("toolu_1"));
    assert!(
        messages[2].content.contains("pub fn alpha()"),
        "{}",
        messages[2].content
    );
    // Claude Code got that result over MCP, and wrote its reply with it.
    let reply = &messages[3].content;
    assert!(
        reply.starts_with("Got: ") && reply.contains("pub fn alpha()"),
        "{reply}"
    );
    let meta = messages[3].meta.clone().unwrap();
    assert_eq!(meta["connector"]["rovikTools"], true);
    assert_eq!(meta["connector"]["session"], "s-9");
    assert_eq!(meta["connector"]["usage"]["cost"], 0.01);
    // The first reply carried its own tokens, and no cost.
    let first = messages[1].meta.clone().unwrap();
    assert_eq!(first["connector"]["usage"]["completion_tokens"], 9);
    assert!(first["connector"]["usage"].get("cost").is_none());

    // The system prompt was Rovik's, its tools named, with Plan mode's
    // instructions; the user's request was the message.
    let system = std::fs::read_to_string(h._dir.0.join("system.txt")).unwrap();
    assert!(system.contains("mcp__rovik__read"), "{system}");
    assert!(
        system.contains("## Tool use rules"),
        "the coding agent's tool rules: {system}"
    );
    assert!(system.contains("The user chose Plan mode"), "{system}");
    let prompt = std::fs::read_to_string(h._dir.0.join("prompt.txt")).unwrap();
    assert_eq!(prompt, "What does alpha return?");
    // Rovik's tools alone: none of its own, nor its permission mode.
    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(args.contains("--strict-mcp-config"), "{args}");
    assert!(!args.contains("bypassPermissions"), "{args}");
}

/// Its plan, submitted with Rovik's `submit_plan` in Plan mode, is saved in
/// the project's plans folder as any model's is, and its reply names it, to
/// be implemented; nothing else in the project changes.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_s_plan_is_saved_in_the_project_s_plans_folder() {
    const ALPHA: &str = "pub fn alpha() -> u32 {\n    42\n}\n";
    let h = harness(&[("src/alpha.rs", ALPHA), ("src/main.rs", "fn main() {}\n")]).await;
    let plan = "# Alpha\n\nMake alpha return 43.\n\n## Steps\n\
        1. Alpha\n   - Files: `src/alpha.rs`\n   - Make `alpha` return 43.\n\
        2. Caller\n   - Files: `src/main.rs`\n   - Print what `alpha` returns.\n\n\
        ## Decisions\n- Keep the signature.\n\n## Validation\n- `cargo test`";
    let submit = json!({ "plan": plan });
    let Some(program) = crate::connectors::fake_claude_with_mcp(&h._dir.0, "submit_plan", &submit)
    else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_send(
        &h,
        &program,
        "plan",
        "Make alpha return 43",
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "done");

    let plans = h.project.join(super::plan::PLANS_DIR);
    let saved: Vec<PathBuf> = std::fs::read_dir(&plans)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(
        std::fs::read_to_string(&saved[0]).unwrap(),
        format!("{plan}\n")
    );
    let messages = h.messages().await;
    let reply = messages.last().unwrap();
    assert_eq!(
        reply.meta.as_ref().unwrap()["planFile"],
        saved[0].to_string_lossy().as_ref()
    );
    assert_eq!(h.file("src/alpha.rs").as_deref(), Some(ALPHA));
}

/// One session of Claude Code's goes on whichever tools a message gets:
/// Rovik's in Plan mode, its own in Agent mode. A message that changes them
/// says first which it has now, since the ones it called before stay in its
/// conversation and it would call them again.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_is_told_when_the_chat_s_mode_changes_its_tools() {
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let read = json!({ "path": "src/alpha.rs" });
    let rovik_tools = || crate::connectors::fake_claude_with_mcp(&h._dir.0, "read", &read);
    let Some(program) = rovik_tools() else {
        eprintln!("no python3: skipped");
        return;
    };
    let file = |name: &str| std::fs::read_to_string(h._dir.0.join(name)).unwrap();
    let send = |mode: &str, text: &str| {
        h.events.lock().unwrap().clear();
        h.start(claude_code_send(&h, &program, mode, text))
    };

    // Plan mode, the chat's first message: a new session, nothing to say.
    send("plan", "What does alpha return?").await.unwrap();
    assert_eq!(h.run_end().await, "done");
    assert_eq!(file("prompt.txt"), "What does alpha return?");

    // Agent mode goes on in the session (s-9), with its own tools.
    crate::connectors::fake_claude(
        &h._dir.0,
        r#"here=$(dirname "$0")
echo "$@" > "$here/args.txt"
cat > "$here/prompt.txt"
echo '{"type":"system","subtype":"init","session_id":"s-9","model":"claude-sonnet-5","tools":["Read"]}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Made it 43.","usage":{},"num_turns":1}'
"#,
    );
    send("agent", "Make it 43").await.unwrap();
    assert_eq!(h.run_end().await, "done");
    assert!(
        file("args.txt").contains("--resume s-9"),
        "{}",
        file("args.txt")
    );
    let prompt = file("prompt.txt");
    assert!(
        prompt.starts_with("Rovik IDE: you work with your own tools from this message on.")
            && prompt.ends_with("\n\nMake it 43"),
        "{prompt}"
    );
    // A second message with its own tools: nothing changed.
    send("agent", "And document it").await.unwrap();
    assert_eq!(h.run_end().await, "done");
    assert_eq!(file("prompt.txt"), "And document it");

    // Plan mode again: Rovik's tools.
    rovik_tools().unwrap();
    send("plan", "Plan the tests").await.unwrap();
    assert_eq!(h.run_end().await, "done");
    let prompt = file("prompt.txt");
    assert!(
        prompt.starts_with("Rovik IDE: you work with Rovik's tools from this message on")
            && prompt.ends_with("\n\nPlan the tests"),
        "{prompt}"
    );
}

/// "Implement this plan" on Claude Code, in Agent mode, with its own
/// tools: it's sent the plan, and asked to keep its checklist with its own
/// tool, not with Rovik's `todo`, which it hasn't there.
#[cfg(unix)]
#[tokio::test]
async fn implementing_a_plan_on_claude_code_asks_for_its_own_checklist() {
    let plan = "# Alpha\n\nMake alpha return 43.\n";
    let h = harness(&[
        ("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n"),
        (".rovik/plans/alpha.md", plan),
    ])
    .await;
    let program = crate::connectors::fake_claude(
        &h._dir.0,
        r#"cat > "$(dirname "$0")/prompt.txt"
echo '{"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5","tools":["Read"]}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Done.","usage":{},"num_turns":1}'
"#,
    );
    let mut request = claude_code_send(
        &h,
        &program,
        "agent",
        "Implement the plan in `.rovik/plans/alpha.md`.",
    );
    if let StartKind::Send { message } = &mut request.start {
        let file = h.project.join(".rovik/plans/alpha.md");
        message.meta = Some(json!({
            "mode": "agent", "implementsPlan": true, "planPath": file.to_string_lossy(),
        }));
    }
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    let prompt = std::fs::read_to_string(h._dir.0.join("prompt.txt")).unwrap();
    assert!(prompt.contains("<plan>\n# Alpha"), "{prompt}");
    assert!(
        prompt.contains("with your own checklist tool, if you have one"),
        "{prompt}"
    );
    assert!(!prompt.contains("todo"), "{prompt}");
}

/// Claude Code with its own tools, under Rovik's permissions: each call it
/// would ask about comes to the run as Rovik's call of the same kind, and
/// is refused, allowed, or put to the user, as Rovik's own would be.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_asks_rovik_before_its_own_tools() {
    let h = harness(&[("src/a.txt", "one\ntwo\n")]).await;
    let asks = json!([
        ["Bash", { "command": "rm -rf build" }, "toolu_1"],
        ["Bash", { "command": "git status" }, "toolu_2"],
        ["Edit", { "file_path": "src/a.txt", "old_string": "two", "new_string": "TWO" }, "toolu_3"],
    ]);
    let Some(program) = crate::connectors::fake_claude_asking(&h._dir.0, &asks) else {
        eprintln!("no python3: skipped");
        return;
    };
    // Edits wait for the user; commands go by the allowed list.
    let permissions = Permissions {
        edit_approval: ApprovalRule {
            required: true,
            by: Approver::User,
        },
        ..quiet_permissions()
    };
    let mut request = h.request(
        "connector://claude-code",
        Harness::send("Tidy up"),
        permissions,
    );
    request.profile.id = "cc::sonnet".into();
    request.profile.model = "sonnet".into();
    request.profile.connector = Some(json!({ "program": program }));
    h.start(request).await.unwrap();

    // The edit is reviewed as Rovik's own: the file, and the change.
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let request = &asked["request"];
    assert_eq!(request["toolCallId"], "toolu_3");
    assert_eq!(request["toolName"], "write");
    assert_eq!(request["edit"]["current"], "one\ntwo\n");
    assert_eq!(request["edit"]["proposed"], "one\nTWO\n");
    let core = h.core.clone();
    let allowed = agent_resolve_approval(
        &core,
        h.dialog_id.clone(),
        "toolu_3".into(),
        ApprovalDecision::Allow,
    );
    assert!(allowed.await);
    assert_eq!(h.run_end().await, "done");

    let decisions: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("decisions.json")).unwrap())
            .unwrap();
    // A command outside the allowed list is refused, as Rovik's would be.
    assert_eq!(decisions[0]["behavior"], "deny");
    let why = decisions[0]["message"].as_str().unwrap();
    assert!(why.contains("permission settings"), "{why}");
    // One in it runs, with the input Claude Code gave.
    assert_eq!(decisions[1]["behavior"], "allow");
    assert_eq!(decisions[1]["updatedInput"]["command"], "git status");
    // The edit the user allowed, as it was.
    assert_eq!(decisions[2]["behavior"], "allow");
    assert_eq!(decisions[2]["updatedInput"]["new_string"], "TWO");

    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(
        args.contains("--permission-prompt-tool mcp__rovik__approve"),
        "{args}"
    );
    let messages = h.messages().await;
    let reply = messages.last().unwrap();
    assert_eq!(reply.content, "deny, allow, allow");
    // Working in the project itself, git is the user's to allow.
    assert!(!args.contains("Bash(git checkout *)"), "{args}");
}

/// Claude Code's own shell, around each command, whatever its permission
/// mode: Rovik's hooks. Before one runs, what it starts is made ready
/// (here everything is there: nothing to say). After one failed for want
/// of the project's dependencies, they're installed, as the user's
/// permissions let that install run unasked, and Claude Code is told to
/// run it again; a program nothing installs is the user's, and it's told
/// so. Needs npm.
#[cfg(unix)]
#[tokio::test]
// Its install runs for real: the lock keeps out other tests' cloud hosts,
// whose Node would come first on its commands' PATH, from a folder that
// goes when their test ends (`toolchains::env_for`). This test's one
// thread holds it across its awaits.
#[allow(clippy::await_holding_lock)]
async fn claude_codes_own_commands_have_what_they_need_made_ready() {
    let _no_host = crate::toolchains::test_lock();
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = project_without_its_dependencies().await;
    let commands = json!([
        ["npm test", null, "toolu_1"],
        [
            "npm test",
            "Exit code 127\nsh: 1: vitest: not found",
            "toolu_2"
        ],
        [
            "rovik-no-such-tool --help",
            "Exit code 127\n/bin/bash: line 1: rovik-no-such-tool: command not found",
            "toolu_3"
        ],
    ]);
    let Some(program) = crate::connectors::fake_claude_with_hooks(&h._dir.0, &commands) else {
        eprintln!("no python3: skipped");
        return;
    };
    let request = claude_code_send(&h, &program, "agent", "Run the tests");
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");

    let answers: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("hooks.json")).unwrap())
            .unwrap();
    let answer = |event: &str, use_id: &str| {
        answers
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["event"] == event && a["use"] == use_id)
            .map(|a| a["answer"].clone())
            .unwrap_or_else(|| panic!("no {event} for {use_id}: {answers:#?}"))
    };
    // `npm` is there: nothing to make ready, nothing said.
    assert_eq!(answer("PreToolUse", "toolu_1"), json!({}));
    // The dependencies weren't: installed, and the model is told so.
    let installed = answer("PostToolUseFailure", "toolu_2");
    assert_eq!(
        installed["hookSpecificOutput"]["hookEventName"],
        "PostToolUseFailure"
    );
    let said = installed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        said.starts_with("Rovik IDE: The project's dependencies weren't installed: `npm ci --no-audit --no-fund` ran"),
        "{said}"
    );
    assert!(said.ends_with("Run the command again."), "{said}");
    assert!(h.project.join("node_modules/.bin/vitest").exists());
    // On its call's line while it installed, then cleared.
    let notes: Vec<Value> = h
        .events_of("toolNote")
        .into_iter()
        .filter(|e| e["toolCallId"] == "toolu_2")
        .map(|e| e["note"].clone())
        .collect();
    assert_eq!(
        notes,
        [
            json!("Installing the dependencies: npm ci --no-audit --no-fund…"),
            Value::Null
        ],
        "{notes:?}"
    );
    // A program nothing installs: the user's.
    let missing = answer("PostToolUseFailure", "toolu_3");
    let said = missing["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        said.contains("`rovik-no-such-tool` isn't installed on this computer"),
        "{said}"
    );
    // Its hooks go with every run of its own tools, whatever its mode.
    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(args.contains("--settings "), "{args}");
    assert!(
        args.contains("--permission-mode bypassPermissions"),
        "{args}"
    );
}

/// On a cloud host, Claude Code's shell gets the host's toolchains from a
/// file it runs before each command (`CLAUDE_ENV_FILE`): the PATH its
/// shell has, with the shared Node in front and the user's package
/// managers at its end, as Rovik's own commands get them.
#[cfg(unix)]
#[tokio::test]
// The lock keeps other tests' hosts out; this test's one thread holds it
// across its awaits.
#[allow(clippy::await_holding_lock)]
async fn on_a_host_claude_codes_shell_gets_the_hosts_toolchains() {
    let _lock = crate::toolchains::test_lock();
    /// Forgets the host however the test ends, before the lock goes.
    struct Forget;
    impl Drop for Forget {
        fn drop(&mut self) {
            crate::toolchains::unset();
        }
    }
    let _forget = Forget;
    let host = crate::testing::temp_dir();
    let root = host.0.join("toolchains");
    crate::toolchains::tests::put_node(&root, "22.3.0");
    crate::toolchains::set(crate::toolchains::Shared {
        root: root.clone(),
        caches: None,
        installer: None,
        bin: Some(host.0.join("bin")),
    });
    let h = harness(&[("package.json", "{}")]).await;
    let commands = json!([["node --version", null, "toolu_1"]]);
    let Some(program) = crate::connectors::fake_claude_with_hooks(&h._dir.0, &commands) else {
        eprintln!("no python3: skipped");
        return;
    };
    let request = claude_code_send(&h, &program, "agent", "Which Node?");
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    let script = std::fs::read_to_string(h._dir.0.join("env.txt")).unwrap();
    let node = root.join("node/22.3.0/bin");
    assert!(
        script.starts_with(&format!(
            "export PATH='{}':\"$PATH\":'{}'",
            node.display(),
            host.0.join("bin").display()
        )),
        "{script}"
    );
}

/// In a chat's copy of the project (here a server's worktree, D8; an
/// isolated chat's overlay is the same), Claude Code's own git commands
/// that would change the repository are refused, whatever its permission
/// mode: by its deny rules, and by Rovik when it asks about a spelling the
/// rules don't match.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_in_a_chats_copy_is_refused_git_that_changes_the_repository() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    h.core.agents.set_on_server();
    let asks = json!([
        ["Bash", { "command": "git checkout main" }, "toolu_1"],
        ["Bash", { "command": "cd src && git -C .. push origin main" }, "toolu_2"],
        ["Bash", { "command": "git status" }, "toolu_3"],
    ]);
    let Some(program) = crate::connectors::fake_claude_asking(&h._dir.0, &asks) else {
        eprintln!("no python3: skipped");
        return;
    };
    // Every command would be allowed: the refusal isn't the settings'.
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    let mut request = h.request(
        "connector://claude-code",
        Harness::send("Ship it"),
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
    for (i, what) in [(0, "git checkout"), (1, "git push")] {
        assert_eq!(decisions[i]["behavior"], "deny", "{decisions:#?}");
        let why = decisions[i]["message"].as_str().unwrap();
        assert!(
            why.contains(&format!("`{what}` changes the repository")),
            "{why}"
        );
    }
    // Reading the repository is fine.
    assert_eq!(decisions[2]["behavior"], "allow");
    assert!(
        h.events_of("approvalRequested").is_empty(),
        "nothing was put to the user"
    );

    // Its own permission modes can't let them by: Claude Code is told to deny them.
    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(args.contains("--disallowedTools"), "{args}");
    assert!(args.contains("Bash(git checkout *)"), "{args}");
    assert!(args.contains("Bash(git push *)"), "{args}");
}

/// Whatever a harness's own permission mode, its command that would change
/// the repository of a chat's copy is refused before it runs, by Rovik's
/// hook: a spelling Claude Code's deny rules don't match too.
#[cfg(unix)]
#[tokio::test]
async fn a_harness_s_own_git_that_changes_a_chats_copy_is_refused_before_it_runs() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    h.core.agents.set_on_server();
    let commands = json!([
        ["git -C . checkout main", null, "toolu_1"],
        ["git status", null, "toolu_2"],
    ]);
    let Some(program) = crate::connectors::fake_claude_with_hooks(&h._dir.0, &commands) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_send(&h, &program, "agent", "Switch branches"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");
    let answers: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("hooks.json")).unwrap())
            .unwrap();
    assert_eq!(answers[0]["use"], "toolu_1");
    let refused = &answers[0]["answer"]["hookSpecificOutput"];
    assert_eq!(refused["permissionDecision"], "deny", "{answers:#?}");
    let why = refused["permissionDecisionReason"].as_str().unwrap();
    assert!(
        why.contains("`git checkout` changes the repository"),
        "{why}"
    );
    // Reading the repository is fine.
    assert_eq!(answers[1]["use"], "toolu_2");
    assert_eq!(answers[1]["answer"], json!({}));
}

/// A send in `mode` on Claude Code's model, as the app sends it: whose
/// tools it runs is the run's to decide, by the mode (`run::lead_profile`).
#[cfg(unix)]
fn claude_code_send(h: &Harness, program: &str, mode: &str, text: &str) -> StartRequest {
    let send = StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: text.into(),
            meta: Some(json!({ "mode": mode })),
            attachments: Vec::new(),
        },
    };
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    let mut request = h.request("connector://claude-code", send, permissions);
    request.profile.id = "cc::sonnet".into();
    request.profile.label = "Claude Code/sonnet".into();
    request.profile.model = "sonnet".into();
    // A mode of Claude Code's own that would let anything run.
    request.profile.connector =
        Some(json!({ "program": program, "permissionMode": "bypassPermissions" }));
    request
}

/// The kinds of a reply's steps, in order: each thought's text, each
/// tool's name.
fn step_kinds(steps: &[Value]) -> Vec<String> {
    steps
        .iter()
        .map(|s| match s["thinking"].as_str() {
            Some(text) => format!("thought:{text}"),
            None => format!("step:{}", s["name"].as_str().unwrap_or("")),
        })
        .collect()
}

/// With its own tools, Claude Code's thinking is asked for and shows in
/// its place among its steps, each stretch timed: in the reply's record,
/// not as a reasoning of the reply's own.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_s_thinking_shows_in_its_place_among_its_steps() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    let event =
        |e: Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null}).to_string();
    let thought = |text: &str| {
        [
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":text}}),
            ),
            event(json!({"type":"content_block_stop","index":0})),
        ]
    };
    let mut lines = vec![
        json!({"type":"system","subtype":"init","session_id":"s-5","model":"claude-sonnet-5"})
            .to_string(),
        event(json!({"type":"message_start","message":{"id":"m1"}})),
    ];
    lines.extend(thought("Read a.txt first."));
    lines.extend([
        json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/a.txt"}}]},"parent_tool_use_id":null}).to_string(),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"one"}]},"parent_tool_use_id":null}).to_string(),
        event(json!({"type":"message_start","message":{"id":"m2"}})),
    ]);
    lines.extend(thought("It says one."));
    lines.extend([
        event(json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"It says one."}})),
        json!({"type":"result","subtype":"success","is_error":false,"result":"It says one.","usage":{},"num_turns":2}).to_string(),
    ]);
    let script = format!(
        "echo \"$@\" > \"$(dirname \"$0\")/args.txt\"\ncat > /dev/null\ncat <<'EOF'\n{}\nEOF\n",
        lines.join("\n")
    );
    let program = crate::connectors::fake_claude(&h._dir.0, &script);
    h.start(claude_code_send(
        &h,
        &program,
        "agent",
        "What does a.txt say?",
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let reply = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert_eq!(reply.content, "It says one.");
    let meta = reply.meta.clone().unwrap();
    let steps = meta["connector"]["steps"].as_array().unwrap();
    assert_eq!(
        step_kinds(steps),
        [
            "thought:Read a.txt first.",
            "step:Read",
            "thought:It says one."
        ]
    );
    assert!(
        steps[0]["ms"].is_u64() && steps[2]["ms"].is_u64(),
        "{steps:#?}"
    );
    assert!(meta.get("reasoning").is_none(), "{meta:#}");
    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(args.contains("--thinking-display summarized"), "{args}");
}

/// The real Claude Code with its own tools: its thinking comes back, as a
/// summary, among the steps of its reply.
///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored --nocapture
#[tokio::test]
#[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
async fn real_claude_code_s_thinking_shows_among_its_steps() {
    if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
        return;
    }
    let h = harness(&[("prices.txt", "bat and ball: 1.10 in total\n")]).await;
    let send = Harness::send(
        "Read prices.txt. The bat costs 1.00 more than the ball: what does the ball cost? Think it through, then answer in one line.",
    );
    let mut request = h.request("connector://claude-code", send, quiet_permissions());
    request.profile.id = "cc::haiku".into();
    request.profile.model = "haiku".into();
    request.profile.no_tools = true;
    request.profile.connector = Some(json!({ "permissionMode": "bypassPermissions" }));
    request.options = Some(serde_json::from_value(json!({ "reasoning_effort": "high" })).unwrap());
    h.start(request).await;
    let end = h.run_end_within(Duration::from_secs(300)).await;
    let messages = h.messages().await;
    let reply = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .unwrap();
    let meta = reply.meta.clone().unwrap_or_default();
    let steps = meta["connector"]["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    eprintln!("{end}: {:#?}\n{}", step_kinds(&steps), reply.content);
    assert!(
        steps
            .iter()
            .any(|s| s["thinking"].as_str().is_some_and(|t| !t.trim().is_empty())),
        "no thinking among its steps: {steps:#?}"
    );
    assert!(meta.get("reasoning").is_none(), "{meta:#}");
}

/// A connector's harness runs as Rovik's agent in Plan mode and Autopilot
/// (`run::lead_profile`): no mode is refused on its model, whatever its
/// profile says of tools. A model marked "no tools" can't run Autopilot,
/// whose stages work with Rovik's tools.
#[test]
fn no_mode_is_refused_on_a_connector_and_autopilot_is_on_a_model_without_tools() {
    let request = |base_url: &str, mode: &str, no_tools: bool| StartRequest {
        dialog_id: "d".into(),
        start: StartKind::Send {
            message: UserMessage {
                id: "m".into(),
                content: "x".into(),
                meta: Some(json!({ "mode": mode })),
                attachments: Vec::new(),
            },
        },
        profile: serde_json::from_value(json!({
            "id": "p::m", "label": "P/m", "model": "m",
            "baseUrl": base_url, "noTools": no_tools,
        }))
        .unwrap(),
        options: None,
        mode: None,
        context_mode: None,
        root: None,
        current_user_content: None,
        local_date: None,
        config: None,
        isolate: None,
        project_settings: None,
        folders: Vec::new(),
    };
    let refusal = |r: StartRequest| super::super::mode_refusal(&r);
    for url in ["connector://claude-code", "connector://codex"] {
        for mode in ["agent", "plan", "autopilot"] {
            for no_tools in [false, true] {
                assert_eq!(refusal(request(url, mode, no_tools)), None, "{url} {mode}");
            }
        }
    }
    let provider = "https://api.example.com/v1";
    let refused = refusal(request(provider, "autopilot", true)).unwrap();
    assert!(
        refused.contains("Autopilot works with Rovik's tools"),
        "{refused}"
    );
    assert_eq!(refusal(request(provider, "plan", true)), None);
    assert_eq!(refusal(request(provider, "autopilot", false)), None);
}

/// The real Claude Code through a plan and its implementation, in one
/// chat, as the user goes: in Plan mode it's Rovik's agent (Rovik's prompt
/// and tools), which changes nothing and saves its plan as Rovik's are;
/// implementing the plan in Agent mode, it works with its own tools, Rovik
/// answering what they ask, and goes on in the same session of its own.
///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored --nocapture
#[tokio::test]
#[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
async fn real_claude_code_plans_as_rovik_s_agent_then_implements_with_its_own_tools() {
    if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
        return;
    }
    const SOURCE: &str = "import sys\n\n\ndef main():\n    print('hello', sys.argv[1:])\n";
    let h = harness(&[("cli.py", SOURCE)]).await;
    let start = |content: String, meta: Value| {
        let send = StartKind::Send {
            message: UserMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content,
                meta: Some(meta),
                attachments: Vec::new(),
            },
        };
        let mut request = h.request("connector://claude-code", send, quiet_permissions());
        request.profile.id = "cc::haiku".into();
        request.profile.label = "Claude Code/haiku".into();
        request.profile.model = "haiku".into();
        request.profile.connector = Some(json!({}));
        request
    };
    let last_reply = |messages: Vec<MessageRow>| {
        messages
            .into_iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .unwrap()
    };

    let ask = "Add a --version flag to cli.py that prints 1.0.";
    h.start(start(ask.into(), json!({ "mode": "plan" }))).await;
    let end = h.run_end_within(Duration::from_secs(600)).await;
    let planned = last_reply(h.messages().await);
    eprintln!("--- Plan mode: {end}\n{}\n", planned.content);
    assert_eq!(
        h.file("cli.py").as_deref(),
        Some(SOURCE),
        "Plan mode changed the project"
    );
    let saved: Vec<PathBuf> = std::fs::read_dir(h.project.join(super::plan::PLANS_DIR))
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default();
    assert_eq!(saved.len(), 1, "saved as Rovik's plans are: {saved:?}");
    let plan = saved[0].to_string_lossy().into_owned();
    let meta = planned.meta.unwrap_or_default();
    assert_eq!(meta["planFile"], plan.as_str(), "{meta:#}");
    assert_eq!(meta["connector"]["rovikTools"], true, "{meta:#}");
    let session = meta["connector"]["session"].clone();

    // "Implement this plan", as the chat sends it (src/lib/plans.ts).
    h.events.lock().unwrap().clear();
    let relative = saved[0].strip_prefix(&h.project).unwrap().display();
    let implement = format!("Implement the plan in `{relative}`.");
    let meta = json!({ "mode": "agent", "implementsPlan": true, "planPath": plan });
    h.start(start(implement, meta)).await;
    let end = h.run_end_within(Duration::from_secs(600)).await;
    let done = last_reply(h.messages().await);
    eprintln!("--- Agent mode: {end}\n{}\n", done.content);
    let now = h.file("cli.py").unwrap();
    assert!(now.contains("--version"), "{now}");
    let meta = done.meta.unwrap_or_default();
    assert_ne!(
        meta["connector"]["rovikTools"], true,
        "its own tools: {meta:#}"
    );
    assert_eq!(meta["connector"]["session"], session, "{meta:#}");
    // Told its tools changed, it calls none of Rovik's it planned with.
    let steps: Vec<String> = h
        .messages()
        .await
        .iter()
        .skip_while(|m| m.id != planned.id)
        .skip(1)
        .filter_map(|m| m.meta.as_ref()?["connector"]["steps"].as_array().cloned())
        .flatten()
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect();
    eprintln!("its steps: {steps:?}");
    assert!(
        !steps.iter().any(|s| s.starts_with("mcp__rovik__")),
        "{steps:?}"
    );
}
