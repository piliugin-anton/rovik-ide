//! Whole runs on a connector's model in Rovik's tools mode: Claude Code
//! (a stand-in, connectors::fake_claude_with_mcp) is the model behind
//! Rovik's agent loop, and its calls are Rovik's tool calls, run by Rovik.

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
