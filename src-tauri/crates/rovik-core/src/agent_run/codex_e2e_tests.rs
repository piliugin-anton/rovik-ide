//! Whole runs on Codex's models, with a stand-in for `codex` (a Python app
//! server, connectors::fake_codex) that plays a scripted turn and calls
//! Rovik's hooks and tools as Codex does: under Rovik's permissions, its
//! commands and patches are put to them; whatever its mode, they're
//! checked against the project's restricted files; in Plan mode and
//! Autopilot, with Rovik's tools, it's the model behind Rovik's agent.
//! Tests that run the real `codex` are `#[ignore]`d, and run with
//! `ROVIK_REAL_CODEX=1`.

#[cfg(unix)]
use super::permissions::{ApprovalRule, Approver};
use super::*;

/// A send in `mode` on Codex's model, its connector's `settings` with the
/// stand-in as its program. Whose tools it runs is the run's to decide, by
/// the mode (`run::lead_profile`).
#[cfg(unix)]
pub(super) fn codex_send(
    h: &Harness,
    program: &str,
    mode: &str,
    text: &str,
    settings: Value,
    permissions: Permissions,
) -> StartRequest {
    let send = StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: text.into(),
            meta: Some(json!({ "mode": mode })),
            attachments: Vec::new(),
        },
    };
    let mut request = h.request("connector://codex", send, permissions);
    request.profile.id = "cx::gpt-6.1-sol".into();
    request.profile.label = "Codex/gpt-6.1-sol".into();
    request.profile.model = "gpt-6.1-sol".into();
    let mut connector = settings;
    connector["program"] = json!(program);
    request.profile.connector = Some(connector);
    request
}

/// What the stand-in kept of its last run.
#[cfg(unix)]
fn record(h: &Harness) -> Value {
    let text = std::fs::read_to_string(h._dir.0.join("codex.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Every request every run of the stand-in was sent, in order.
#[cfg(unix)]
fn all_requests(h: &Harness) -> Vec<Value> {
    let text = std::fs::read_to_string(h._dir.0.join("requests.jsonl")).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The answer Rovik's hook gave to `event` for the tool use `id`.
#[cfg(unix)]
fn hook_answer(record: &Value, event: &str, id: &str) -> Value {
    record["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["event"] == event && h["id"] == id)
        .map(|h| h["answer"].clone())
        .unwrap_or_else(|| panic!("no {event} for {id}: {:#}", record["hooks"]))
}

/// The params of the first request of `method` the stand-in was sent.
#[cfg(unix)]
fn request_of(record: &Value, method: &str) -> Value {
    record["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["method"] == method)
        .map(|r| r["params"].clone())
        .unwrap_or_else(|| panic!("no {method}: {:#}", record["requests"]))
}

/// The steps the reply recorded, by id.
#[cfg(unix)]
fn steps_of(reply: &MessageRow) -> HashMap<String, Value> {
    let meta = reply.meta.clone().unwrap_or_default();
    meta["connector"]["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s["id"].as_str().unwrap_or("").to_string(), s))
        .collect()
}

/// Codex with its own tools under Rovik's permissions: each command and
/// each patch is put to them as Rovik's own call of the same kind would
/// be, before Codex would ask: a command outside the allowed list is
/// refused with why, one in it runs, and a patch is reviewed as Rovik's
/// edit of the file, of which the user accepts part: Codex applies what
/// was accepted. What Codex asks before running a call passes only what
/// Rovik allowed.
#[cfg(unix)]
#[tokio::test]
async fn codex_asks_rovik_before_its_own_commands_and_patches() {
    let h = harness(&[("src/a.txt", "one\ntwo\n")]).await;
    let patch =
        "*** Begin Patch\n*** Update File: src/a.txt\n@@\n-one\n+uno\n-two\n+dos\n*** End Patch";
    let scenario = json!({ "steps": [
        { "reasoning": "Clean up, then edit." },
        { "command": "rm -rf build", "id": "exec-1" },
        { "command": "git status", "id": "exec-2", "output": "clean\n" },
        { "patch": patch, "id": "exec-3", "paths": ["src/a.txt"] },
        { "text": "Done." },
        { "usage": [1200, 1000, 30] },
    ] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
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
    h.start(codex_send(
        &h,
        &program,
        "agent",
        "Tidy up",
        json!({}),
        permissions,
    ))
    .await
    .unwrap();

    // The patch is reviewed as Rovik's own edit: the file, and the change.
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    let request = &asked["request"];
    assert_eq!(request["toolCallId"], "exec-3");
    assert_eq!(request["toolName"], "write");
    assert_eq!(request["edit"]["current"], "one\ntwo\n");
    assert_eq!(request["edit"]["proposed"], "uno\ndos\n");
    // Of its changes, the user takes the second.
    let partly = ApprovalDecision::Partial {
        content: "one\ndos\n".into(),
        total: 2,
        rejected: Vec::new(),
    };
    let core = h.core.clone();
    assert!(agent_resolve_approval(&core, h.dialog_id.clone(), "exec-3".into(), partly).await);
    assert_eq!(h.run_end().await, "done");

    let record = record(&h);
    assert_eq!(
        record["trusted"],
        json!(["PermissionRequest", "PostToolUse", "PreToolUse"])
    );
    // A command outside the allowed list is refused, as Rovik's would be.
    let refused = &hook_answer(&record, "PreToolUse", "exec-1")["hookSpecificOutput"];
    assert_eq!(refused["permissionDecision"], "deny");
    let why = refused["permissionDecisionReason"].as_str().unwrap();
    assert!(why.contains("permission settings"), "{why}");
    // One in it runs; Codex's ask before it does passes.
    assert_eq!(hook_answer(&record, "PreToolUse", "exec-2"), json!({}));
    let asks: Vec<&Value> = record["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["event"] == "PermissionRequest")
        .collect();
    assert_eq!(asks.len(), 2, "the command and the patch: {asks:#?}");
    for ask in asks {
        assert_eq!(
            ask["answer"]["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
    }
    // The patch Codex applies makes what the user accepted.
    let edited = &hook_answer(&record, "PreToolUse", "exec-3")["hookSpecificOutput"];
    assert_eq!(edited["permissionDecision"], "allow");
    let applied = record["applied"][0].as_str().unwrap();
    assert_eq!(edited["updatedInput"]["command"], applied);
    assert_eq!(
        applied,
        "*** Begin Patch\n*** Update File: src/a.txt\n@@\n-one\n-two\n+one\n+dos\n*** End Patch"
    );
    // Codex asked before every call, its sandbox none: Rovik's commands'.
    let thread = request_of(&record, "thread/start");
    assert_eq!(thread["approvalPolicy"], "untrusted");
    assert_eq!(thread["sandbox"], "danger-full-access");
    assert!(
        thread.get("baseInstructions").is_none(),
        "Codex's own prompt"
    );
    let turn = request_of(&record, "turn/start");
    assert_eq!(turn["input"][0]["text"], "Tidy up");
    assert_eq!(turn["summary"], "auto");

    let messages = h.messages().await;
    let reply = messages.last().unwrap();
    assert_eq!(reply.content, "Done.");
    let steps = steps_of(reply);
    // The refused command shows as a step, with why.
    assert_eq!(steps["exec-1"]["name"], "shell");
    assert_eq!(steps["exec-1"]["ok"], false);
    assert!(steps["exec-1"]["result"]
        .as_str()
        .unwrap()
        .contains("permission settings"));
    assert_eq!(steps["exec-2"]["input"]["command"], "git status");
    assert_eq!(steps["exec-2"]["ok"], true);
    assert_eq!(steps["exec-3"]["name"], "edit");
    assert_eq!(steps["exec-3"]["input"]["path"], "src/a.txt");
    assert_eq!(steps["thinking-1"]["thinking"], "Clean up, then edit.");
    let meta = reply.meta.clone().unwrap();
    assert_eq!(meta["connector"]["kind"], "codex");
    assert_eq!(meta["connector"]["session"], "th-1");
    assert_eq!(meta["connector"]["usage"]["prompt_tokens"], 1200);
    assert_eq!(
        meta["connector"]["usage"]["prompt_tokens_details"]["cached_tokens"],
        1000
    );
    assert_eq!(
        meta["connector"]["usage"]["connector"]["context"],
        json!({ "used": 1230, "window": 258400 })
    );
    // The hooks' token went in the environment, never on the command line.
    assert_eq!(record["env"]["ROVIK_HOOKS_TOKEN"], true);
}

/// Whatever Codex's own mode, its command that would read a restricted
/// file is refused before it runs, and a command that failed goes to the
/// run, which tells the model what it found. Nothing is put to Rovik's
/// permissions.
#[cfg(unix)]
#[tokio::test]
async fn codex_s_own_commands_are_checked_whatever_its_mode() {
    let h = harness(&[(".env", "API_KEY=sk-live-1\n"), ("src/app.ts", "x\n")]).await;
    let missing = "/bin/bash: line 1: rovik-no-such-tool: command not found\n";
    let scenario = json!({ "steps": [
        { "command": "cat .env", "id": "exec-1" },
        { "command": "ls src", "id": "exec-3", "output": "app.ts\n" },
        { "command": "rovik-no-such-tool --help", "id": "exec-4", "output": missing, "exit": 127 },
        { "text": "Looked." },
    ] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let permissions = Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    h.start(codex_send(
        &h,
        &program,
        "agent",
        "Look",
        settings,
        permissions,
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "done");

    let record = record(&h);
    let reason = |id: &str| {
        let out = &hook_answer(&record, "PreToolUse", id)["hookSpecificOutput"];
        assert_eq!(
            out["permissionDecision"], "deny",
            "{id}: {:#}",
            record["hooks"]
        );
        out["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(reason("exec-1").contains("would read .env, a restricted file"));
    assert_eq!(hook_answer(&record, "PreToolUse", "exec-3"), json!({}));
    let told = hook_answer(&record, "PostToolUse", "exec-4");
    let said = told["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        said.contains("`rovik-no-such-tool` isn't installed on this computer"),
        "{said}"
    );
    // Codex's own mode decides the rest: nothing asked of Rovik.
    assert_eq!(record["trusted"], json!(["PostToolUse", "PreToolUse"]));
    assert!(h.events_of("approvalRequested").is_empty());
    let thread = request_of(&record, "thread/start");
    assert_eq!(
        (
            thread["approvalPolicy"].as_str(),
            thread["sandbox"].as_str()
        ),
        (Some("never"), Some("danger-full-access"))
    );
    let messages = h.messages().await;
    let steps = steps_of(messages.last().unwrap());
    assert_eq!(steps["exec-1"]["ok"], false, "refused, and shown so");
    assert_eq!(steps["exec-4"]["ok"], false);
}

/// In Agent mode, the files Codex's own patch changes are the run's
/// change: the project's checks run on them when Codex finishes. Its
/// command that changes nothing doesn't count.
#[cfg(unix)]
#[tokio::test]
async fn codex_s_own_patch_is_checked() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    h.set_checks(json!([{ "kind": "test", "command": "ls src", "enabled": true }]));
    let patch = "*** Begin Patch\n*** Update File: src/a.txt\n@@\n-one\n+uno\n*** End Patch";
    let scenario = json!({ "steps": [
        { "command": "ls src", "id": "exec-1", "output": "a.txt\n" },
        { "patch": patch, "id": "exec-2", "paths": ["src/a.txt"] },
        { "text": "Done." },
    ] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    h.start(codex_send(
        &h,
        &program,
        "agent",
        "Make it uno",
        settings,
        permissions,
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "verified");
    let records = super::repair_tests::records(&h.messages().await);
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0]["status"], "passed");
    assert_eq!(records[0]["changedFiles"], json!(["src/a.txt"]));
}

/// Plan mode on Codex, whatever its permission mode: it's the model behind
/// Rovik's agent. Its call is the reply's, Rovik runs it and stores its
/// result, and Codex, waiting on it over MCP, gets it and writes its reply.
/// Rovik's prompt replaces Codex's, the project's part apart, and Codex's
/// own tools are off.
#[cfg(unix)]
#[tokio::test]
async fn in_plan_mode_codex_is_rovik_s_agent_and_rovik_runs_its_calls() {
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let scenario = json!({ "steps": [
        { "text": "Reading it." },
        { "rovik": "read", "arguments": { "path": "src/alpha.rs" }, "id": "call_1" },
        { "usage": [900, 0, 20] },
        { "text": "Got: {result:call_1}" },
    ] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    let request = codex_send(
        &h,
        &program,
        "plan",
        "What does alpha return?",
        settings,
        quiet_permissions(),
    );
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");

    let messages = h.messages().await;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(
        roles,
        ["user", "assistant", "tool", "assistant"],
        "{messages:#?}"
    );
    let calls = messages[1].tool_calls.clone().unwrap();
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        ("call_1", "read")
    );
    assert_eq!(calls[0].arguments, r#"{"path":"src/alpha.rs"}"#);
    assert_eq!(messages[1].content, "Reading it.");
    assert!(
        messages[2].content.contains("pub fn alpha()"),
        "{}",
        messages[2].content
    );
    let reply = &messages[3].content;
    assert!(
        reply.starts_with("Got: ") && reply.contains("pub fn alpha()"),
        "{reply}"
    );
    let meta = messages[3].meta.clone().unwrap();
    assert_eq!(meta["connector"]["rovikTools"], true);
    assert_eq!(meta["connector"]["session"], "rovik:th-1");

    let record = record(&h);
    // Rovik's prompt, its tools named; the project's part apart.
    let thread = request_of(&record, "thread/start");
    let base = thread["baseInstructions"].as_str().unwrap();
    assert!(base.contains("mcp__rovik"), "{base}");
    assert!(
        base.contains("## Tool use rules"),
        "the coding agent's rules: {base}"
    );
    assert_eq!(
        (
            thread["approvalPolicy"].as_str(),
            thread["sandbox"].as_str()
        ),
        (Some("never"), Some("read-only"))
    );
    let listed = &record["listed"];
    assert!(listed["rovik"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "read"));
    assert_eq!(listed["rovik_hooks"], json!(["hook"]));
    let args = record["args"].as_array().unwrap();
    assert!(
        args.iter().any(|a| a == "features.shell_tool=false"),
        "{args:?}"
    );
    assert_eq!(record["env"]["ROVIK_MCP_TOKEN"], true);
}

/// Codex in Plan mode (as an Autopilot task's plan stage runs too): the
/// plan mode is Rovik's, in its prompt, and none of Codex's own, which
/// Codex refuses from a client without its experimental API.
#[cfg(unix)]
#[tokio::test]
async fn codex_plans_in_rovik_s_plan_mode() {
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let scenario = json!({ "steps": [{ "text": "# Plan\n1. Make alpha return 43." }] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let settings = json!({});
    let request = codex_send(
        &h,
        &program,
        "plan",
        "Plan a change to alpha",
        settings,
        quiet_permissions(),
    );
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    let reply = h.messages().await.last().unwrap().content.clone();
    assert!(reply.contains("Make alpha return 43."), "{reply}");
    let record = record(&h);
    assert!(request_of(&record, "turn/start")
        .get("collaborationMode")
        .is_none());
    let initialize = request_of(&record, "initialize");
    assert_eq!(initialize["capabilities"]["experimentalApi"], false);
    // Rovik's plan mode is told with the project's part of its prompt.
    let developer = request_of(&record, "thread/start")["developerInstructions"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        developer.contains("The user chose Plan mode"),
        "{developer}"
    );
}

/// A chat whose Codex thread is gone (another computer, a cleared folder)
/// goes on in a new one, told what the chat was about.
#[cfg(unix)]
#[tokio::test]
async fn a_lost_codex_thread_starts_a_new_one_with_the_recap() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    // It knows no thread: each one to continue is gone.
    let scenario = json!({ "threads": [], "steps": [{ "text": "Sure." }] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    let first = codex_send(
        &h,
        &program,
        "agent",
        "First ask",
        settings.clone(),
        quiet_permissions(),
    );
    h.start(first).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    h.events.lock().unwrap().clear();
    let next = codex_send(
        &h,
        &program,
        "agent",
        "Next ask",
        settings,
        quiet_permissions(),
    );
    h.start(next).await.unwrap();
    assert_eq!(h.run_end().await, "done");

    let requests = all_requests(&h);
    let of = |method: &str| -> Vec<Value> {
        requests
            .iter()
            .filter(|r| r["method"] == method)
            .map(|r| r["params"].clone())
            .collect()
    };
    assert_eq!(of("thread/resume")[0]["threadId"], "th-1");
    assert_eq!(
        of("thread/start").len(),
        2,
        "the first turn's, and the new one"
    );
    let turns = of("turn/start");
    assert_eq!(turns[0]["input"][0]["text"], "First ask");
    let text = turns[1]["input"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("Earlier in this conversation"), "{text}");
    assert!(
        text.contains("First ask") && text.ends_with("Next ask"),
        "{text}"
    );
}

/// A command of Codex's that starts without Rovik's hook having seen it
/// (its hook failed, a version that skips it) stops the turn: Codex is
/// told to interrupt it, and the run ends in an error.
#[cfg(unix)]
#[tokio::test]
async fn a_codex_command_rovik_didn_t_check_stops_the_turn() {
    let h = harness(&[("src/a.txt", "one\n")]).await;
    let scenario = json!({ "steps": [{ "unchecked": "rm -rf src", "id": "exec-9" }] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    h.start(codex_send(
        &h,
        &program,
        "agent",
        "Go",
        settings,
        quiet_permissions(),
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "error");
    let record = record(&h);
    assert!(
        record["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "turn/interrupt"),
        "{:#}",
        record["requests"]
    );
    let reply = h.messages().await.last().unwrap().clone();
    let failure = reply.meta.unwrap_or_default()["failure"].clone();
    let message = failure["message"].as_str().unwrap_or_default();
    assert!(message.contains("without Rovik IDE's hook"), "{failure:#}");
}

/// On a cloud host, Codex's shell gets the host's toolchains from a file
/// it reads before each command (`BASH_ENV`), as Rovik's own commands get
/// them.
#[cfg(unix)]
#[tokio::test]
// The lock keeps other tests' hosts out; this test's one thread holds it
// across its awaits.
#[allow(clippy::await_holding_lock)]
async fn on_a_host_codex_s_shell_gets_the_host_s_toolchains() {
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
    let scenario = json!({ "steps": [{ "command": "node --version", "id": "exec-1" }] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let settings = json!({ "permissionMode": "fullAccess" });
    h.start(codex_send(
        &h,
        &program,
        "agent",
        "Which Node?",
        settings,
        quiet_permissions(),
    ))
    .await
    .unwrap();
    assert_eq!(h.run_end().await, "done");
    let record = record(&h);
    let script = record["bash_env"].as_str().unwrap();
    let node = root.join("node/22.3.0/bin");
    assert!(
        script.starts_with(&format!("export PATH='{}':\"$PATH\"", node.display())),
        "{script}"
    );
}

/*
 * The real `codex`, on a scripted model: OpenAI's Responses API stood in
 * for (`responses_api`), so no account is needed. Its program is the one
 * on the PATH.
 */

/// A scripted stand-in for OpenAI's Responses API: each request takes the
/// next of `script`'s responses (a list of events, sent as a stream), the
/// last again once they're used up; the requests are kept, in order.
#[cfg(unix)]
async fn responses_api(script: Vec<Vec<Value>>) -> (String, Arc<Mutex<Vec<Value>>>) {
    responses_routed(Vec::new(), script).await
}

/// `responses_api` with a script for each run of a task: a request takes
/// the next response of the first route whose marker it holds (`last:`, in
/// its last input item), else of `default`.
#[cfg(unix)]
pub(super) async fn responses_routed(
    routes: Vec<(&'static str, Vec<Vec<Value>>)>,
    default: Vec<Vec<Value>>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let (markers, mut scripts): (Vec<&str>, Vec<Vec<Vec<Value>>>) = routes.into_iter().unzip();
    scripts.push(default);
    let scripts = Arc::new(scripts);
    // How far each script has gone.
    let taken = Arc::new(Mutex::new(vec![0usize; scripts.len()]));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (kept, markers) = (kept.clone(), markers.clone());
            let (scripts, taken) = (scripts.clone(), taken.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let (mut head, mut length) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                if reader.read_exact(&mut body).await.is_err() {
                    return;
                }
                let (kind, text) = if head.starts_with("POST") {
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let text = request.to_string();
                    let last = request["input"]
                        .as_array()
                        .and_then(|input| input.last())
                        .map(Value::to_string)
                        .unwrap_or_default();
                    let route = markers
                        .iter()
                        .position(|marker| match marker.strip_prefix("last:") {
                            Some(marker) => last.contains(marker),
                            None => text.contains(marker),
                        })
                        .unwrap_or(markers.len());
                    kept.lock().unwrap().push(request);
                    let n = {
                        let mut taken = taken.lock().unwrap();
                        taken[route] += 1;
                        taken[route] - 1
                    };
                    let script = &scripts[route];
                    let events = &script[n.min(script.len() - 1)];
                    let stream: String = events
                        .iter()
                        .map(|e| {
                            format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or(""))
                        })
                        .collect();
                    ("text/event-stream", stream)
                } else {
                    ("application/json", r#"{"models":[]}"#.to_string())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = write.write_all(response.as_bytes()).await;
                let _ = write.shutdown().await;
            });
        }
    });
    (url, seen)
}

/// One response of the scripted model: `output`, then its usage.
#[cfg(unix)]
pub(super) fn response(id: &str, output: Value) -> Vec<Value> {
    vec![
        json!({ "type": "response.created", "response": { "id": id } }),
        json!({ "type": "response.output_item.done", "item": output }),
        json!({ "type": "response.completed", "response": { "id": id,
            "usage": { "input_tokens": 100, "output_tokens": 10, "total_tokens": 110 } } }),
    ]
}

/// A reply of the scripted model, in words.
#[cfg(unix)]
pub(super) fn says(id: &str, text: &str) -> Vec<Value> {
    response(
        id,
        json!({ "type": "message", "role": "assistant", "id": format!("m-{id}"),
                "content": [{ "type": "output_text", "text": text }] }),
    )
}

/// A call of Codex's code tool, which runs `code` (JavaScript, its own
/// tools in `tools`), each call's result or error added to its output.
#[cfg(unix)]
fn runs(id: &str, calls: &[(&str, &str)]) -> Vec<Value> {
    let code: Vec<String> = calls
        .iter()
        .map(|(label, call)| {
            format!("try {{ text(\"{label} \" + JSON.stringify(await {call})); }} catch (e) {{ text(\"{label} ERROR \" + e); }}")
        })
        .collect();
    let item = json!({ "type": "custom_tool_call", "name": "exec", "input": code.join("\n"), "call_id": format!("call-{id}") });
    response(id, item)
}

/// The real `codex`'s settings: on the scripted model at `url`, its own
/// settings folder `home`.
#[cfg(unix)]
pub(super) fn real_codex(url: &str, home: &std::path::Path, mut settings: Value) -> Value {
    let provider = format!(
        r#"model_providers.mock={{name="mock",base_url="{url}",wire_api="responses",env_key="HOME"}}"#
    );
    settings["configDir"] = json!(home.to_string_lossy());
    settings["extraArgs"] = json!(format!("-c '{provider}' -c 'model_provider=\"mock\"'"));
    settings
}

/// What the scripted model was sent, as one text.
#[cfg(unix)]
fn sent(seen: &Arc<Mutex<Vec<Value>>>) -> String {
    seen.lock().unwrap().iter().map(Value::to_string).collect()
}

/// The real `codex` under Rovik's permissions: its command outside the
/// allowed list never runs, the model told why; its patch is reviewed as
/// Rovik's edit, and of it Codex applies what the user accepted.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[cfg(unix)]
#[tokio::test]
#[ignore = "runs the real codex: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_asks_rovik_before_its_own_commands_and_patches() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    let h = harness(&[("src/a.txt", "one\ntwo\n"), ("build/out.txt", "x\n")]).await;
    let patch = r#"tools.apply_patch("*** Begin Patch\n*** Update File: src/a.txt\n@@\n-one\n+uno\n-two\n+dos\n*** End Patch")"#;
    let (url, seen) = responses_api(vec![
        runs(
            "r1",
            &[
                ("A", r#"tools.exec_command({cmd: "rm -rf build"})"#),
                ("B", r#"tools.exec_command({cmd: "ls src"})"#),
                ("C", patch),
            ],
        ),
        says("r2", "Done."),
    ])
    .await;
    let home = crate::testing::temp_dir();
    let permissions = Permissions {
        edit_approval: ApprovalRule {
            required: true,
            by: Approver::User,
        },
        ..quiet_permissions()
    };
    let settings = real_codex(&url, &home.0, json!({}));
    let mut request = codex_send(&h, "codex", "agent", "Tidy up", settings, permissions);
    request.profile.connector.as_mut().unwrap()["program"] = json!("");
    h.start(request).await.unwrap();
    let asked = h
        .wait_for_within("approvalRequested", Duration::from_secs(120), |e| {
            e["type"] == "approvalRequested"
        })
        .await;
    assert_eq!(asked["request"]["edit"]["proposed"], "uno\ndos\n");
    let call = asked["request"]["toolCallId"].as_str().unwrap().to_string();
    let partly = ApprovalDecision::Partial {
        content: "one\ndos\n".into(),
        total: 2,
        rejected: Vec::new(),
    };
    assert!(agent_resolve_approval(&h.core, h.dialog_id.clone(), call, partly).await);
    assert_eq!(h.run_end_within(Duration::from_secs(120)).await, "done");
    assert!(
        h.project.join("build/out.txt").exists(),
        "the refused command didn't run"
    );
    assert_eq!(h.file("src/a.txt").as_deref(), Some("one\ndos\n"));
    let told = sent(&seen);
    assert!(
        told.contains("permission settings"),
        "the model is told why: {told}"
    );
    assert_eq!(h.messages().await.last().unwrap().content, "Done.");
}

/// The real `codex` as the model behind Rovik's agent, in Plan mode: its
/// call of Rovik's `read` is the reply's, Rovik runs it, and Codex sends
/// the model its result.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[cfg(unix)]
#[tokio::test]
#[ignore = "runs the real codex: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_runs_rovik_tools() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let call = json!({ "type": "function_call", "namespace": "mcp__rovik", "name": "read",
                       "arguments": "{\"path\":\"src/alpha.rs\"}", "call_id": "call_read_1" });
    let (url, seen) = responses_api(vec![response("r1", call), says("r2", "It returns 42.")]).await;
    let home = crate::testing::temp_dir();
    let settings = real_codex(&url, &home.0, json!({}));
    let mut request = codex_send(
        &h,
        "codex",
        "plan",
        "What does alpha return?",
        settings,
        quiet_permissions(),
    );
    request.profile.connector.as_mut().unwrap()["program"] = json!("");
    h.start(request).await.unwrap();
    assert_eq!(h.run_end_within(Duration::from_secs(120)).await, "done");
    let messages = h.messages().await;
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(
        roles,
        ["user", "assistant", "tool", "assistant"],
        "{messages:#?}"
    );
    assert_eq!(messages[1].tool_calls.clone().unwrap()[0].id, "call_read_1");
    assert!(messages[2].content.contains("pub fn alpha()"));
    assert_eq!(messages[3].content, "It returns 42.");
    let first = seen.lock().unwrap()[0].clone();
    assert!(
        first.to_string().contains("## Tool use rules"),
        "Rovik's prompt: {first}"
    );
    assert!(
        sent(&seen).contains("pub fn alpha()"),
        "Codex got Rovik's result"
    );
}

/// The real `codex` in Plan mode (as an Autopilot task's plan stage runs
/// too): the plan mode is Rovik's, its prompt and its tools, not Codex's
/// own, and the plan is the reply.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[cfg(unix)]
#[tokio::test]
#[ignore = "runs the real codex: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_plans_in_rovik_s_plan_mode() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    let h = harness(&[("src/alpha.rs", "pub fn alpha() -> u32 {\n    42\n}\n")]).await;
    let (url, seen) = responses_api(vec![says("r1", "# Plan\n1. Make alpha return 43.")]).await;
    let home = crate::testing::temp_dir();
    let settings = real_codex(&url, &home.0, json!({}));
    let mut request = codex_send(
        &h,
        "codex",
        "plan",
        "Plan a change to alpha",
        settings,
        quiet_permissions(),
    );
    request.profile.connector.as_mut().unwrap()["program"] = json!("");
    h.start(request).await.unwrap();
    assert_eq!(h.run_end_within(Duration::from_secs(120)).await, "done");
    let reply = h.messages().await.last().unwrap().content.clone();
    assert!(reply.contains("Make alpha return 43."), "{reply}");
    let first = seen.lock().unwrap()[0].to_string();
    assert!(
        first.contains("The user chose Plan mode"),
        "Rovik's plan mode: {first}"
    );
}

/// The real `codex`'s probe and commands: its version, its models, a test
/// request answered, and its own commands with its skills.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[cfg(unix)]
#[tokio::test]
#[ignore = "runs the real codex: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_is_probed_and_lists_its_commands() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    let (url, _) = responses_api(vec![says("r1", "pong")]).await;
    let home = crate::testing::temp_dir();
    let args = crate::connectors::ProbeRequestArgs {
        kind: crate::connectors::ConnectorKind::Codex,
        settings: real_codex(&url, &home.0, json!({})),
        api_key: String::new(),
        request: true,
    };
    let probe = crate::connectors::probe(args.clone()).await.unwrap();
    assert!(probe.version.starts_with("codex-cli "), "{}", probe.version);
    assert!(!probe.models.is_empty(), "{probe:#?}");
    let answered = probe.request.unwrap();
    assert!(answered.input_tokens > 0 && answered.output_tokens > 0);
    let cwd = crate::testing::temp_dir();
    let commands = crate::connectors::commands(args, Some(cwd.0.to_string_lossy().into_owned()))
        .await
        .unwrap();
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names[..2], ["compact", "review"], "{names:?}");
}

/// npm's registry answers with Codex's newest version.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[tokio::test]
#[ignore = "asks registry.npmjs.org: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_latest_version_is_published() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    let codex = crate::connectors::connector(crate::connectors::ConnectorKind::Codex);
    let latest = codex.latest_version().await.expect("a version");
    assert!(latest.starts_with(|c: char| c.is_ascii_digit()), "{latest}");
}

/// An older release of Codex's, for an update to have something to do.
#[cfg(unix)]
const OLDER_CODEX: &str = "0.163.0";

/// Runs a Set up step's command as Set up does, with `home` as the user's
/// home and `more` in its environment; fails with what it printed.
#[cfg(unix)]
async fn run_step(
    command: &crate::connectors::SetupCommand,
    home: &std::path::Path,
    more: &[(&str, &str)],
) {
    let out = tokio::process::Command::new(&command.program)
        .args(&command.args)
        .envs(command.env.iter().map(|(key, value)| (key, value)))
        .env("HOME", home)
        .envs(more.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}:\n{}{}",
        command.shown,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// What `program --version` says.
#[cfg(unix)]
fn version_of(program: &std::path::Path) -> String {
    let out = std::process::Command::new(program)
        .arg("--version")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// OpenAI's installer, as Set up runs it, installs Codex for the user, and
/// Update brings it to the newest version, which Rovik then no longer
/// offers, though the connector keeps its login in a settings folder of
/// its own.
///   ROVIK_REAL_CODEX=1 cargo test -p rovik-core real_codex -- --ignored
#[cfg(unix)]
#[tokio::test]
#[ignore = "downloads Codex twice: set ROVIK_REAL_CODEX=1 and pass --ignored"]
async fn real_codex_is_installed_by_set_up_and_updated_to_the_newest() {
    if std::env::var("ROVIK_REAL_CODEX").is_err() {
        return;
    }
    use crate::connectors::{connector, is_newer, ConnectorKind, ProbeRequestArgs, SetupAction};
    let codex = connector(ConnectorKind::Codex);
    let home = crate::testing::temp_dir();
    let program = home.0.join(".local/bin/codex");
    let args = ProbeRequestArgs {
        kind: ConnectorKind::Codex,
        settings: json!({ "program": program, "configDir": home.0.join("work") }),
        api_key: String::new(),
        request: false,
    };
    let install = codex
        .setup_command(SetupAction::Install, &args)
        .unwrap()
        .unwrap();
    run_step(&install, &home.0, &[("CODEX_RELEASE", OLDER_CODEX)]).await;
    assert_eq!(version_of(&program), format!("codex-cli {OLDER_CODEX}"));

    let update = codex
        .setup_command(SetupAction::Update, &args)
        .unwrap()
        .unwrap();
    run_step(&update, &home.0, &[]).await;
    let updated = version_of(&program);
    assert!(is_newer(&updated, OLDER_CODEX), "{updated}");
    let latest = codex.latest_version().await.expect("a version");
    assert!(
        !is_newer(&latest, &updated),
        "{updated}; {latest} is published"
    );
}
