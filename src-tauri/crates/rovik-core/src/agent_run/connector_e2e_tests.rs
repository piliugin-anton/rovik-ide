//! Whole runs on a connector's model. In Rovik's tools mode, Claude Code
//! (a stand-in, connectors::fake_claude_with_mcp) is the model behind
//! Rovik's agent loop, and its calls are Rovik's tool calls, run by Rovik.

#[cfg(unix)]
use super::permissions::{ApprovalRule, Approver};
use super::*;

/// A start on Claude Code's model, with Rovik's tools.
#[cfg(unix)]
fn connector_start(h: &Harness, program: &str, text: &str) -> StartRequest {
    let mut request = h.request(
        "connector://claude-code",
        Harness::send(text),
        quiet_permissions(),
    );
    request.profile.id = "cc::sonnet".into();
    request.profile.label = "Claude Code/sonnet".into();
    request.profile.model = "sonnet".into();
    request.profile.connector = Some(json!({ "program": program, "tools": "rovik" }));
    request
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_with_rovik_tools_runs_its_calls_through_rovik() {
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let Some(program) = crate::connectors::fake_claude_with_mcp(&h._dir.0, "src/alpha.rs") else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(connector_start(&h, &program, "What does alpha return?"))
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

    // The system prompt was Rovik's, its tools named; the user's request
    // was the message.
    let system = std::fs::read_to_string(h._dir.0.join("system.txt")).unwrap();
    assert!(system.contains("mcp__rovik__read"), "{system}");
    assert!(
        system.contains("## Tool use rules"),
        "the coding agent's tool rules: {system}"
    );
    let prompt = std::fs::read_to_string(h._dir.0.join("prompt.txt")).unwrap();
    assert_eq!(prompt, "What does alpha return?");
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
async fn claude_codes_own_commands_have_what_they_need_made_ready() {
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
    let request = own_tools_send(&h, &program, "agent", "Run the tests");
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
    let request = own_tools_send(&h, &program, "agent", "Which Node?");
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
    h.start(own_tools_send(&h, &program, "agent", "Switch branches"))
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

/// A send in `mode` on Claude Code's model with its own tools.
#[cfg(unix)]
fn own_tools_send(h: &Harness, program: &str, mode: &str, text: &str) -> StartRequest {
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
    request.profile.model = "sonnet".into();
    // As the app resolves a connector running its own tools.
    request.profile.no_tools = true;
    // A mode of Claude Code's own that would let anything run.
    request.profile.connector =
        Some(json!({ "program": program, "permissionMode": "bypassPermissions" }));
    request
}

/// Plan mode with Claude Code's own tools: Claude Code runs in its own plan
/// mode, whatever its permission mode, and Rovik answers what it asks:
/// its edit and its command refused as Rovik's own would be in Plan mode,
/// its way out of plan mode refused with its plan as the reply. Nothing of
/// Rovik's prompt goes (its plan instructions describe Rovik's tools), and
/// the run doesn't press it to submit a plan with a tool it doesn't have.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_in_plan_mode_plans_in_its_own_and_changes_nothing() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    let asks = json!([
        ["Edit", { "file_path": "src/a.txt", "old_string": "one", "new_string": "ONE" }, "toolu_1"],
        ["Bash", { "command": "rm -rf build" }, "toolu_2"],
        ["ExitPlanMode", { "plan": "# Plan" }, "toolu_3"],
    ]);
    let Some(program) = crate::connectors::fake_claude_asking(&h._dir.0, &asks) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(own_tools_send(&h, &program, "plan", "Plan the change"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");

    let decisions: Value =
        serde_json::from_str(&std::fs::read_to_string(h._dir.0.join("decisions.json")).unwrap())
            .unwrap();
    for (i, tool) in [(0, "write"), (1, "bash")] {
        assert_eq!(decisions[i]["behavior"], "deny", "{decisions:#?}");
        let why = decisions[i]["message"].as_str().unwrap();
        assert!(
            why.contains(&format!("{tool} isn't available in Plan mode")),
            "{why}"
        );
    }
    assert_eq!(decisions[2]["behavior"], "deny");
    assert!(decisions[2]["message"]
        .as_str()
        .unwrap()
        .contains("your plan is your reply"));
    assert!(h.events_of("approvalRequested").is_empty());

    let args = std::fs::read_to_string(h._dir.0.join("args.txt")).unwrap();
    assert!(
        args.contains("--permission-mode plan")
            && args.contains("--permission-prompt-tool mcp__rovik__approve"),
        "{args}"
    );
    assert!(!args.contains("bypassPermissions"), "{args}");
    assert!(!args.contains("system-prompt-file"), "{args}");
    assert_eq!(h.file("src/a.txt").as_deref(), Some("one\n"));
    // One request, one reply: no note sent it back to submit a plan.
    let messages = h.messages().await;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant"], "{messages:#?}");
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
    h.start(own_tools_send(
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

/// Autopilot needs Rovik's tools: a send on a connector running its own
/// is refused with why, before anything is stored.
#[cfg(unix)]
#[tokio::test]
async fn autopilot_is_refused_on_a_connector_running_its_own_tools() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    let refused = agent_start(
        &h.core,
        own_tools_send(&h, "claude", "autopilot", "Ship it"),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("Autopilot works with Rovik's tools"),
        "{refused}"
    );
    assert!(h.messages().await.is_empty());
}

/// Plan mode refuses a connector running its own tools without a plan mode
/// of its own: they would carry the plan out. Claude Code has one.
#[test]
fn plan_mode_needs_a_connector_s_own_plan_mode() {
    let request = |mode: &str, no_tools: bool| StartRequest {
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
            "id": "cc::sonnet", "label": "Claude Code/sonnet", "model": "sonnet",
            "baseUrl": "connector://claude-code", "noTools": no_tools,
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
    };
    let without = |r: &StartRequest| super::super::mode_refusal_with(r, |_| false);
    let refused = without(&request("plan", true)).unwrap();
    assert!(
        refused.starts_with("Claude Code has no plan mode of its own"),
        "{refused}"
    );
    // Its plan mode, Rovik's tools, or another mode: nothing to refuse.
    assert_eq!(super::super::mode_refusal(&request("plan", true)), None);
    assert_eq!(without(&request("plan", false)), None);
    assert_eq!(without(&request("agent", true)), None);
}

/// The real Claude Code through whole runs, in both of its setups, in Agent
/// and in Plan mode: with its own tools (its own prompt; Rovik answers what
/// it asks) and with Rovik's (Rovik's prompt, its tools over MCP). Plan mode
/// changes nothing (with Rovik's tools, the plan is saved as Rovik's are);
/// Agent mode makes the change.
///   ROVIK_REAL_CLAUDE=1 cargo test -p rovik-core real_claude -- --ignored --nocapture
#[tokio::test]
#[ignore = "runs the real claude: set ROVIK_REAL_CLAUDE=1 and pass --ignored"]
async fn real_claude_code_plans_and_works_with_its_own_tools_and_with_rovik_s() {
    if std::env::var("ROVIK_REAL_CLAUDE").is_err() {
        return;
    }
    const SOURCE: &str = "import sys\n\n\ndef main():\n    print('hello', sys.argv[1:])\n";
    for tools in ["own", "rovik"] {
        for mode in ["plan", "agent"] {
            let h = harness(&[("cli.py", SOURCE)]).await;
            let send = StartKind::Send {
                message: UserMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    content: "Add a --version flag to cli.py that prints 1.0.".into(),
                    meta: Some(json!({ "mode": mode })),
                    attachments: Vec::new(),
                },
            };
            let mut request = h.request("connector://claude-code", send, quiet_permissions());
            request.profile.id = "cc::haiku".into();
            request.profile.label = "Claude Code/haiku".into();
            request.profile.model = "haiku".into();
            // As the app resolves the connector's model in each setup.
            request.profile.no_tools = tools == "own";
            request.profile.connector = Some(json!({ "tools": tools }));
            h.start(request).await;
            let end = h.run_end_within(Duration::from_secs(600)).await;
            let messages = h.messages().await;
            let reply = messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
                .map(|m| m.content.clone())
                .unwrap_or_default();
            eprintln!("--- {tools} tools, {mode} mode: {end}\n{reply}\n");
            let now = h.file("cli.py").unwrap();
            if mode == "plan" {
                assert_eq!(now, SOURCE, "{tools} tools: Plan mode changed the project");
                let plans = h.project.join(super::plan::PLANS_DIR);
                let saved = std::fs::read_dir(&plans).map(|d| d.count()).unwrap_or(0);
                if tools == "rovik" {
                    assert!(saved > 0, "Rovik's tools: the plan is saved as Rovik's are");
                } else {
                    assert_eq!(saved, 0, "its own tools: its plan is the reply");
                    assert!(reply.contains("--version"), "{reply}");
                }
            } else {
                assert!(
                    now.contains("--version"),
                    "{tools} tools, Agent mode: {now}"
                );
            }
        }
    }
}
