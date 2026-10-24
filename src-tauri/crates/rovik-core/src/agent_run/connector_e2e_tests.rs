//! Whole runs on a connector's model. In Rovik's tools mode, Claude Code
//! (a stand-in, connectors::fake_claude_with_mcp) is the model behind
//! Rovik's agent loop, and its calls are Rovik's tool calls, run by Rovik.

use super::permissions::{ApprovalRule, Approver};
use super::*;

/// A start on Claude Code's model, with Rovik's tools.
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
}
