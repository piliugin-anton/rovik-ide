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

/// A send on Claude Code in Agent mode, as `claude_code_send`'s, with the
/// project's checks on.
#[cfg(unix)]
fn claude_code_checked(h: &Harness, program: &str, text: &str) -> StartRequest {
    let mut request = claude_code_send(h, program, "agent", text);
    let permissions = &mut request.config.as_mut().unwrap().permissions;
    permissions.verify_changes = true;
    permissions.checklist = ChecklistMode::Off;
    request
}

/// A step of `fake_claude_turns`: Claude Code's `Write` (`use_id`) of
/// `text` to `path` in `h`'s project (or outside it, `path` absolute),
/// which it makes when it runs.
#[cfg(unix)]
fn claude_writes(h: &Harness, use_id: &str, path: &str, text: &str) -> Value {
    let mut files = serde_json::Map::new();
    files.insert(path.to_string(), json!(text));
    json!({
        "tool": "Write",
        "input": { "file_path": h.project.join(path), "content": text },
        "use": use_id,
        "files": files,
    })
}

/// What the stand-in for Claude Code kept in `name` beside it.
#[cfg(unix)]
fn kept(h: &Harness, name: &str) -> String {
    std::fs::read_to_string(h._dir.0.join(name)).unwrap()
}

/// In Agent mode, what Claude Code changes with its own tools is the run's
/// change: the project's checks run on it when Claude Code finishes, what
/// fails goes back to its session as its next message, and its fix is
/// checked again.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_s_own_changes_are_checked_and_a_failure_goes_back_to_it() {
    let h = harness(&[("src/a.txt", "bad\n")]).await;
    h.set_checks(json!([{ "kind": "test", "command": "grep -q good src/a.txt", "enabled": true }]));
    let turns = json!([
        [claude_writes(&h, "toolu_1", "src/a.txt", "still bad\n")],
        [claude_writes(&h, "toolu_2", "src/a.txt", "good\n")],
    ]);
    let Some(program) = crate::connectors::fake_claude_turns(&h._dir.0, &turns) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_checked(&h, &program, "Make it good"))
        .await
        .unwrap();
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");

    let records = super::repair_tests::records(&h.messages().await);
    let statuses: Vec<&Value> = records.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("passed")],
        "{records:#?}"
    );
    assert_eq!(records[0]["changedFiles"], json!(["src/a.txt"]));
    // The failure went back to the same session.
    let report = kept(&h, "prompt-1.txt");
    assert!(
        report.contains("[Automatic check by the IDE, not written by the user]")
            && report.contains("Files you changed in this task: src/a.txt")
            && report.contains("## Tests failed: `grep -q good src/a.txt` (exit code 1)"),
        "{report}"
    );
    assert!(kept(&h, "args.txt").contains("--resume s-8"));
    assert_eq!(h.file("src/a.txt").as_deref(), Some("good\n"));
}

/// A turn of Claude Code's that changes none of the project's files runs
/// no checks: it read a file, ran a command that changes nothing, its edit
/// that failed changed nothing, Rovik refused its edit of a restricted
/// file, and it wrote a scratch file outside the project.
#[cfg(unix)]
#[tokio::test]
async fn a_claude_code_turn_that_changes_no_project_file_runs_no_checks() {
    let env = "API_KEY=sk-live-1\n";
    let h = harness(&[("src/a.txt", "one\n"), (".env", env)]).await;
    h.set_checks(json!([{ "kind": "test", "command": "grep -q one src/a.txt", "enabled": true }]));
    let at = |path: &str| h.project.join(path);
    let scratch = h._dir.0.join("scratch.txt");
    let outside = claude_writes(&h, "toolu_5", &scratch.to_string_lossy(), "notes\n");
    let turns = json!([[
        { "tool": "Read", "input": { "file_path": at("src/a.txt") }, "use": "toolu_1" },
        ["ls src", null, "toolu_2"],
        {
            "tool": "Edit",
            "input": { "file_path": at("src/a.txt"), "old_string": "gone", "new_string": "x" },
            "use": "toolu_3",
            "error": "String to replace not found in file.",
        },
        {
            "tool": "Edit",
            "input": { "file_path": at(".env"), "old_string": "sk", "new_string": "pk" },
            "use": "toolu_4",
            "files": { ".env": "API_KEY=pk-live-1\n" },
        },
        outside,
    ]]);
    let Some(program) = crate::connectors::fake_claude_turns(&h._dir.0, &turns) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_checked(&h, &program, "Look around"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");
    let records = super::repair_tests::records(&h.messages().await);
    assert!(records.is_empty(), "{records:#?}");
    assert_eq!(h.file(".env").as_deref(), Some(env), "refused");
    assert_eq!(h.file("src/a.txt").as_deref(), Some("one\n"));
    assert_eq!(std::fs::read_to_string(scratch).unwrap(), "notes\n");
}

/// A checklist an earlier run left unfinished is kept with Rovik's `todo`,
/// which Claude Code with its own tools hasn't: its run that goes on with
/// the task, changing a file, isn't held to that checklist, and its change
/// is checked.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_with_its_own_tools_is_not_held_to_rovik_s_checklist() {
    let h = harness(&[("src/a.txt", "bad\n")]).await;
    h.set_checks(json!([{ "kind": "test", "command": "grep -q good src/a.txt", "enabled": true }]));
    let (url, _) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "Read it", "status": "done" }, { "title": "Fix it", "status": "pending" }] }),
        },
        Reply::Text("Done."),
        Reply::Text("Really done."),
        Reply::Text("Truly done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix src/a.txt"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "unfinished-checklist");

    h.events.lock().unwrap().clear();
    let turns = json!([[claude_writes(&h, "toolu_1", "src/a.txt", "good\n")]]);
    let Some(program) = crate::connectors::fake_claude_turns(&h._dir.0, &turns) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_checked(&h, &program, "continue"))
        .await
        .unwrap();
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    assert_eq!(
        kept(&h, "runs.txt"),
        "1",
        "one turn: no reminder of the checklist"
    );
}

/// Working in the project itself, a fix of Claude Code's that breaks a
/// check that passed is rolled back as one of Rovik's own would be: each
/// file it changed has its text from before the fix again, and the file it
/// created goes. Claude Code hears what was undone, and its next fix is
/// checked.
#[cfg(unix)]
#[tokio::test]
// Its checks run npm: the lock keeps out other tests' cloud hosts, whose
// Node would come first on their PATH (`toolchains::env_for`). This test's
// one thread holds it across its awaits.
#[allow(clippy::await_holding_lock)]
async fn a_claude_code_fix_that_breaks_a_passing_check_is_rolled_back() {
    use super::repair_tests::{has_node, records, PACKAGE, RUNNER, TESTS};
    let _no_host = crate::toolchains::test_lock();
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let turns = json!([
        // `one` still fails.
        [claude_writes(&h, "toolu_1", "notes.txt", "first try\n")],
        // The fix breaks `two`, and creates a file.
        [
            claude_writes(&h, "toolu_2", "b.txt", "broken\n"),
            claude_writes(&h, "toolu_3", "c.txt", "new file\n"),
            claude_writes(&h, "toolu_4", "notes.txt", "second try\n"),
        ],
        // Rolled back, it fixes `one`.
        [claude_writes(&h, "toolu_5", "a.txt", "good\n")],
    ]);
    let Some(program) = crate::connectors::fake_claude_turns(&h._dir.0, &turns) else {
        eprintln!("no python3: skipped");
        return;
    };
    h.start(claude_code_checked(&h, &program, "Fix the tests"))
        .await
        .unwrap();
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");

    let rounds = records(&h.messages().await);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("rolled-back"), &json!("passed")],
        "{rounds:#?}"
    );
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(
        rb["restored"],
        json!(["b.txt", "c.txt", "notes.txt"]),
        "{rb}"
    );
    assert_eq!(rb["skipped"], json!([]), "{rb}");
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
    assert_eq!(h.file("c.txt"), None);
    assert_eq!(h.file("notes.txt").as_deref(), Some("first try\n"));
    assert_eq!(h.file("a.txt").as_deref(), Some("good\n"));
    let told = kept(&h, "prompt-2.txt");
    assert!(
        told.contains("Your last fix broke something that didn't fail before")
            && told
                .contains("So it was rolled back: b.txt, c.txt, notes.txt are as before that fix."),
        "{told}"
    );
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

/// The patches of the chat's reply, in the order they came.
#[cfg(unix)]
fn reply_patches(h: &Harness) -> Vec<Value> {
    let reply = h
        .events_of("messagesAdded")
        .iter()
        .flat_map(|e| e["messages"].as_array().cloned().unwrap_or_default())
        .find(|m| m["role"] == "assistant")
        .expect("a reply");
    h.events_of("messagePatched")
        .into_iter()
        .filter(|e| e["messageId"] == reply["id"])
        .map(|e| e["patch"].clone())
        .collect()
}

/// A fake `claude` printing each of `parts` (lines of its output), silent
/// for a second between one part and the next: far longer than the run
/// waits before it says a reply is late or stalled (`run::STILL_WAITING`,
/// `run::REPLY_IDLE`).
#[cfg(unix)]
fn claude_in_parts(h: &Harness, parts: &[&[Value]]) -> String {
    let printed: Vec<String> = parts
        .iter()
        .map(|lines| {
            let lines: Vec<String> = lines.iter().map(Value::to_string).collect();
            format!("cat <<'EOF'\n{}\nEOF\n", lines.join("\n"))
        })
        .collect();
    let script = format!("cat > /dev/null\n{}", printed.join("sleep 1\n"));
    crate::connectors::fake_claude(&h._dir.0, &script)
}

/// A skill the user runs that works in a forked context (`/name` of one
/// with `context: fork`): Claude Code prints nothing of its work until it's
/// done. Its step shows from the start, at work, and no line says the
/// reply is late or stalled meanwhile; its reply follows once it's done.
#[cfg(unix)]
#[tokio::test]
async fn a_forked_skill_the_user_runs_shows_as_its_step_while_it_works() {
    let h = harness(&[("a.txt", "alpha\n")]).await;
    let fork = "forked-command-census";
    let started = json!({"type":"system","subtype":"task_started","task_id":"a1","description":"/census","task_type":"local_agent"});
    let done = [
        json!({"type":"system","subtype":"task_notification","task_id":"a1","status":"completed","summary":"/census"}),
        json!({"type":"system","subtype":"init","session_id":"s-6","model":"claude-haiku-5-5"}),
        json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.txt"}}]},"parent_tool_use_id":fork}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"alpha"}]},"parent_tool_use_id":fork}),
        json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"a.txt says alpha."}]},"parent_tool_use_id":null}),
        json!({"type":"result","subtype":"success","is_error":false,"result":"a.txt says alpha.","num_turns":0,"usage":{}}),
    ];
    // The skill works for a second.
    let program = claude_in_parts(&h, &[&[started], &done]);
    h.start(claude_code_send(&h, &program, "agent", "/census"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");

    let mut running = false;
    let mut shown_running = false;
    for patch in reply_patches(&h) {
        if let Some(steps) = patch["meta"]["connector"]["steps"].as_array() {
            running = steps.first().is_some_and(|s| s["ok"].is_null());
            shown_running |= running;
            // Named before Claude Code says anything of itself, for the
            // chat to show the step as Claude Code's ("Using skill census").
            assert_eq!(patch["meta"]["connector"]["kind"], "claude-code");
        }
        if running {
            assert!(
                patch.get("status").is_none_or(Value::is_null),
                "a status while the skill works: {patch:#}"
            );
        }
    }
    assert!(shown_running, "{:#?}", reply_patches(&h));

    let messages = h.messages().await;
    let reply = messages.last().unwrap();
    assert_eq!(reply.content, "a.txt says alpha.");
    let meta = reply.meta.clone().unwrap();
    let steps = meta["connector"]["steps"].as_array().unwrap();
    assert_eq!(steps[0]["name"], "Skill");
    assert_eq!(steps[0]["input"], json!({ "skill": "census" }));
    assert_eq!(steps[0]["ok"], true);
    // What it did, as its subagent's steps.
    assert_eq!(steps[1]["name"], "Read");
    assert_eq!(steps[1]["parent"], fork);
}

/// Claude Code writes, runs a tool, writes again: its reply's text is one,
/// so each step says where in it the step came (the text's length then, as
/// the chat's strings count it), for the chat to show the text and the
/// steps in the order they went.
#[cfg(unix)]
#[tokio::test]
async fn each_step_says_where_in_the_reply_s_text_it_came() {
    let h = harness(&[("a.txt", "alpha\n"), ("b.txt", "beta\n")]).await;
    let event = |message: &str, e: Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null,"api_message_id":message});
    let start = |message: &str| {
        event(
            message,
            json!({"type":"message_start","message":{"id":message}}),
        )
    };
    let text = |message: &str, text: &str| {
        event(
            message,
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        )
    };
    let read = |message: &str, id: &str, path: &str| json!({"type":"assistant","message":{"id":message,"content":[{"type":"tool_use","id":id,"name":"Read","input":{"file_path":path}}]},"parent_tool_use_id":null});
    let result = |id: &str, content: &str| json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":content}]},"parent_tool_use_id":null});
    let lines = [
        json!({"type":"system","subtype":"init","session_id":"s-3","model":"claude-haiku-5-5"}),
        start("m1"),
        text("m1", "Let’s read 📄 a.txt…"),
        read("m1", "t1", "a.txt"),
        result("t1", "alpha"),
        start("m2"),
        text("m2", "Now b."),
        read("m2", "t2", "b.txt"),
        result("t2", "beta"),
        start("m3"),
        text("m3", "Alpha, beta."),
        json!({"type":"result","subtype":"success","is_error":false,"result":"Alpha, beta.","num_turns":3,"usage":{}}),
    ];
    let program = claude_in_parts(&h, &[&lines]);
    h.start(claude_code_send(&h, &program, "agent", "Read a and b"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");

    let reply = h.messages().await.last().unwrap().clone();
    assert_eq!(
        reply.content,
        "Let’s read 📄 a.txt…\n\nNow b.\n\nAlpha, beta."
    );
    let meta = reply.meta.unwrap();
    let steps = meta["connector"]["steps"].as_array().unwrap();
    // In UTF-16 code units, as JavaScript slices strings: the emoji is two.
    let at: Vec<&Value> = steps.iter().map(|s| &s["at"]).collect();
    assert_eq!(at, [&json!(20), &json!(28)]);
    // Kept once each had its result.
    assert_eq!(steps[1]["ok"], true);
}

/// While Claude Code runs a tool of its own it prints nothing: that's the
/// tool's time, which its step shows at work, not the reply stalling. Once
/// the tool is done, a silence is the reply's again: "Still generating".
#[cfg(unix)]
#[tokio::test]
async fn while_claude_code_runs_a_tool_its_reply_is_not_said_to_be_generating() {
    let h = harness(&[]).await;
    let event = |e: Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null});
    let call = [
        json!({"type":"system","subtype":"init","session_id":"s-8","model":"claude-haiku-5-5"}),
        json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 1"}}]},"parent_tool_use_id":null}),
    ];
    let result = json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":""}]},"parent_tool_use_id":null});
    let reply = [
        event(json!({"type":"message_start","message":{"id":"m2"}})),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Slept."}}),
        ),
        json!({"type":"result","subtype":"success","is_error":false,"result":"Slept.","num_turns":2,"usage":{}}),
    ];
    // The command runs for a second; then the model takes a second to answer.
    let program = claude_in_parts(&h, &[&call, &[result], &reply]);
    h.start(claude_code_send(&h, &program, "agent", "Sleep a second"))
        .await
        .unwrap();
    assert_eq!(h.run_end().await, "done");

    // The step's state as the patches went: none, running, done.
    let mut step = "none";
    let mut generating_after_it = false;
    for patch in reply_patches(&h) {
        if let Some(steps) = patch["meta"]["connector"]["steps"].as_array() {
            if let Some(first) = steps.first() {
                step = if first["ok"].is_null() {
                    "running"
                } else {
                    "done"
                };
            }
        }
        let generating = patch["status"]["text"] == "Still generating";
        assert!(
            !(generating && step == "running"),
            "said to be generating while the command runs: {patch:#}"
        );
        generating_after_it |= generating && step == "done";
    }
    assert!(generating_after_it, "{:#?}", reply_patches(&h));
    assert_eq!(h.messages().await.last().unwrap().content, "Slept.");
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
