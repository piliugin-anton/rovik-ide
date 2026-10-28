//! The `mcp` tool in whole runs against the scripted model
//! (docs/design/mcp-plan.md, Q05-Q08), and served to Claude Code and Codex
//! running their own tools, as their stand-ins call it (Q09). The MCP
//! server is Rovik's own (connectors/mcp.rs), a legacy Streamable HTTP one,
//! here as the user's `docs` server: `read` marked read-only, `bash` not.

use super::*;
use crate::connectors::mcp::{McpReply, McpServer as RovikServer};
use rovik_protocol::mcp::{McpServer, McpTransport, McpValue};

/// The `docs` server's tools, as OpenAI function schemas (what Rovik's own
/// server lists).
fn docs_tools() -> Vec<Value> {
    vec![
        json!({ "type": "function", "function": { "name": "read", "description": "Read a page of the docs. Any page.", "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } } }),
        json!({ "type": "function", "function": { "name": "bash", "description": "Run a command on the docs host.", "parameters": { "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] } } }),
    ]
}

/// Rovik's server as the user's `docs` server, its calls answered with
/// `"<tool>: <arguments>"`; and the calls it got.
async fn docs_server() -> (RovikServer, McpServer, Arc<Mutex<Vec<String>>>) {
    let (rovik, mut calls) = RovikServer::start(&docs_tools(), "The project's docs.", None)
        .await
        .unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let kept = got.clone();
    tokio::spawn(async move {
        while let Some(call) = calls.recv().await {
            let text = format!("{}: {}", call.name, call.arguments);
            kept.lock().unwrap().push(text.clone());
            let _ = call.reply.send(McpReply::text(text));
        }
    });
    let server = McpServer {
        id: "docs-id".into(),
        name: "docs".into(),
        enabled: true,
        transport: McpTransport::Url {
            url: rovik.url(),
            headers: vec![McpValue {
                name: "Authorization".into(),
                value: rovik.authorization(),
                secret: false,
            }],
        },
        disabled_tools: Vec::new(),
    };
    (rovik, server, got)
}

/// The run's start, with the `docs` server.
fn with_docs(mut request: StartRequest, server: &McpServer) -> StartRequest {
    request.config.as_mut().unwrap().mcp_servers = vec![server.clone()];
    request
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t.pointer("/function/name")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Q05, Q06, Q07: the agent gets one `mcp` tool, the list in its prompt; a
/// description comes without asking; a call waits for the user's approval,
/// which names the server's tool and its arguments, and its result goes
/// back to the model.
#[tokio::test]
async fn an_mcp_tool_is_called_once_the_user_approves_it() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "docs.bash", "describe": true }),
        },
        Reply::Call {
            id: "m2",
            name: "mcp",
            args: json!({ "tool": "docs.bash", "arguments": { "command": "ls" } }),
        },
        Reply::Text("Listed."),
    ])
    .await;
    let request = h.request(
        &url,
        Harness::send("List the docs host"),
        quiet_permissions(),
    );
    h.start(with_docs(request, &docs)).await;
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert_eq!(
        asked["request"]["toolCallId"], "m2",
        "the description didn't ask"
    );
    assert_eq!(
        asked["request"]["summary"],
        "Call docs.bash with {\"command\":\"ls\"}"
    );
    assert!(
        got.lock().unwrap().is_empty(),
        "nothing ran before the approval"
    );
    assert!(
        agent_resolve_approval(
            &h.core,
            h.dialog_id.clone(),
            "m2".into(),
            ApprovalDecision::Allow
        )
        .await
    );
    assert_eq!(h.run_end().await, "done");

    let requests = seen.lock().unwrap().clone();
    let first = &requests[0];
    assert_eq!(tool_names(first).iter().filter(|n| *n == "mcp").count(), 1);
    let prompt = system_prompt(first);
    assert!(prompt.contains("## MCP tools"), "{prompt}");
    assert!(
        prompt.contains("### docs\nThe project's docs.\n- bash(command): Run a command on the docs host.\n- read(path): Read a page of the docs."),
        "{prompt}"
    );
    for (i, request) in requests.iter().enumerate() {
        assert_eq!(system_prompt(request), prompt, "request {i}: the same list");
        assert_eq!(
            tool_names(request),
            tool_names(first),
            "request {i}: the same tools"
        );
    }
    let results: Vec<String> = h
        .messages()
        .await
        .into_iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content)
        .collect();
    assert!(
        results[0].starts_with("docs.bash: Run a command"),
        "{}",
        results[0]
    );
    assert!(results[0].contains("Input schema"), "{}", results[0]);
    assert_eq!(results[1], "bash: {\"command\":\"ls\"}");
    assert_eq!(*got.lock().unwrap(), ["bash: {\"command\":\"ls\"}"]);
}

/// Q11 (mcp-plan-2.md): a daemon's runs get the `mcp` tool too.
#[tokio::test]
async fn a_run_on_a_daemon_calls_mcp_tools() {
    let h = harness(&[]).await;
    h.core.agents.set_on_server();
    let (_rovik, docs, got) = docs_server().await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "docs.bash", "arguments": { "command": "ls" } }),
        },
        Reply::Text("Listed."),
    ])
    .await;
    let request = h.request(
        &url,
        Harness::send("List the docs host"),
        quiet_permissions(),
    );
    h.start(with_docs(request, &docs)).await;
    h.wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert!(
        agent_resolve_approval(
            &h.core,
            h.dialog_id.clone(),
            "m1".into(),
            ApprovalDecision::Allow
        )
        .await
    );
    assert_eq!(h.run_end().await, "done");
    let first = seen.lock().unwrap()[0].clone();
    assert_eq!(tool_names(&first).iter().filter(|n| *n == "mcp").count(), 1);
    assert_eq!(*got.lock().unwrap(), ["bash: {\"command\":\"ls\"}"]);
}

/// Q07: the user can let MCP calls run without asking.
#[tokio::test]
async fn mcp_calls_run_without_asking_when_the_rule_says_so() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "docs.read", "arguments": { "path": "intro" } }),
        },
        Reply::Text("Read."),
    ])
    .await;
    let permissions = Permissions {
        mcp_approval: permissions::ApprovalRule {
            required: false,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    let request = h.request(&url, Harness::send("Read the intro"), permissions);
    h.start(with_docs(request, &docs)).await;
    assert_eq!(h.run_end().await, "done");
    assert!(h.events_of("approvalRequested").is_empty());
    assert_eq!(*got.lock().unwrap(), ["read: {\"path\":\"intro\"}"]);
}

/// Q08: Plan mode lists only the read-only tools, and a call of another is
/// refused without asking or reaching the server.
#[tokio::test]
async fn plan_mode_lists_and_calls_only_read_only_mcp_tools() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "m1",
            name: "mcp",
            args: json!({ "tool": "docs.bash", "arguments": { "command": "rm -rf /" } }),
        },
        Reply::Text("Can't."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Plan it"), quiet_permissions());
    request.mode = Some("plan".into());
    h.start(with_docs(request, &docs)).await;
    h.run_end().await;
    let requests = seen.lock().unwrap().clone();
    let prompt = system_prompt(&requests[0]);
    assert!(prompt.contains("- read(path)"), "{prompt}");
    assert!(!prompt.contains("- bash("), "{prompt}");
    let refused = h
        .messages()
        .await
        .into_iter()
        .find(|m| m.role == "tool")
        .unwrap();
    assert!(
        refused.content.contains("isn't marked read-only"),
        "{}",
        refused.content
    );
    assert!(h.events_of("approvalRequested").is_empty());
    assert!(got.lock().unwrap().is_empty());
}

/// Q01: a run with no MCP servers gets no `mcp` tool and no list.
#[tokio::test]
async fn without_servers_there_is_no_mcp_tool() {
    let h = harness(&[]).await;
    let (url, seen) = model(vec![Reply::Text("Hi.")]).await;
    h.start(h.request(&url, Harness::send("Hello"), quiet_permissions()))
        .await;
    h.run_end().await;
    let requests = seen.lock().unwrap().clone();
    assert!(!tool_names(&requests[0]).contains(&"mcp".to_string()));
    assert!(!system_prompt(&requests[0]).contains("## MCP tools"));
}

/// Q08: a scout gets `mcp` too, with the read-only tools alone in its list,
/// and calls them as the lead would.
#[tokio::test]
async fn a_scout_calls_only_read_only_mcp_tools() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let report = "## Answer\nIn the intro.\n\n## Evidence\n- the docs' intro\n\n## Open questions\nNone\n\n## Web sources\nNone";
    let (url, seen) = model_routed(
        vec![(
            "MARK-7",
            vec![
                Reply::Call {
                    id: "s1",
                    name: "mcp",
                    args: json!({ "tool": "docs.read", "arguments": { "path": "intro" } }),
                },
                Reply::Text(report),
            ],
        )],
        vec![
            Reply::Call {
                id: "t1",
                name: "task",
                args: json!({ "kind": "scout", "goal": "Find MARK-7 in the docs" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    let permissions = Permissions {
        mcp_approval: permissions::ApprovalRule {
            required: false,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    let mut request = h.request(&url, Harness::send("Find it"), permissions);
    request.config.as_mut().unwrap().subagents = serde_json::from_value(
        json!({ "enabled": true, "kinds": { "scout": { "enabled": true } } }),
    )
    .unwrap();
    h.start(with_docs(request, &docs)).await;
    assert_eq!(h.run_end().await, "done");
    let scout: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|b| crate::testing::route_of(b, &["MARK-7"]).is_some())
        .cloned()
        .collect();
    assert!(tool_names(&scout[0]).contains(&"mcp".to_string()));
    let prompt = system_prompt(&scout[0]);
    assert!(prompt.starts_with("You are a scout"), "{prompt}");
    assert!(prompt.contains("- read(path)"), "{prompt}");
    assert!(!prompt.contains("- bash("), "{prompt}");
    assert_eq!(*got.lock().unwrap(), ["read: {\"path\":\"intro\"}"]);
}

/// The calls the user's `docs` server got, one of `read` with `path`.
fn read_of(path: &str) -> Vec<String> {
    vec![format!("read: {{\"path\":\"{path}\"}}")]
}

/// Q09: Claude Code with its own tools (Agent mode) is served the `mcp`
/// tool beside them, under Rovik's permissions here, the list after its own
/// prompt; a call of it waits for the user's approval under the MCP rule,
/// then runs, and its result goes back to Claude Code.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_with_its_own_tools_calls_mcp_tools_through_rovik() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let call = json!({ "tool": "docs.read", "arguments": { "path": "intro" } });
    let asks = json!([["mcp__rovik__mcp", call, "toolu_9"]]);
    let Some(program) = crate::connectors::fake_claude_asking(&h._dir.0, &asks) else {
        eprintln!("no python3: skipped");
        return;
    };
    let mut request = h.request(
        "connector://claude-code",
        Harness::send("Read the intro"),
        quiet_permissions(),
    );
    request.profile.id = "cc::sonnet".into();
    request.profile.model = "sonnet".into();
    request.profile.connector = Some(json!({ "program": program }));
    h.start(with_docs(request, &docs)).await;
    let asked = h
        .wait_for("approvalRequested", |e| e["type"] == "approvalRequested")
        .await;
    assert_eq!(asked["request"]["toolCallId"], "toolu_9");
    assert_eq!(
        asked["request"]["summary"],
        "Call docs.read with {\"path\":\"intro\"}"
    );
    assert!(
        got.lock().unwrap().is_empty(),
        "nothing ran before the approval"
    );
    assert!(
        agent_resolve_approval(
            &h.core,
            h.dialog_id.clone(),
            "toolu_9".into(),
            ApprovalDecision::Allow
        )
        .await
    );
    assert_eq!(h.run_end().await, "done");
    assert_eq!(*got.lock().unwrap(), read_of("intro"));

    let kept = |name: &str| std::fs::read_to_string(h._dir.0.join(name)).unwrap();
    let decisions: Value = serde_json::from_str(&kept("decisions.json")).unwrap();
    assert_eq!(decisions[0]["text"], read_of("intro")[0]);
    assert_eq!(decisions[0]["isError"], false);
    let tools: Vec<String> = serde_json::from_str(&kept("tools.json")).unwrap();
    assert_eq!(tools, ["approve", "mcp"]);
    let appended = kept("appended.txt");
    assert!(appended.starts_with("## MCP tools"), "{appended}");
    assert!(
        appended.contains("Call one with the `mcp__rovik__mcp` tool"),
        "{appended}"
    );
    assert!(
        appended.contains("### docs\nThe project's docs.\n- bash(command)"),
        "{appended}"
    );
    let args = kept("args.txt");
    assert!(
        args.contains(
            "--permission-prompt-tool mcp__rovik__approve --allowedTools mcp__rovik__mcp"
        ),
        "Claude Code doesn't ask before it: Rovik does, inside the call: {args}"
    );
    assert!(
        !args.contains("--strict-mcp-config"),
        "the user's own servers stay: {args}"
    );
}

/// Q09: Codex with its own tools is served the `mcp` tool on a `rovik`
/// server of its own, called directly, the list in the thread's developer
/// instructions; a call goes by the MCP rule, here one that doesn't ask.
#[cfg(unix)]
#[tokio::test]
async fn codex_with_its_own_tools_calls_mcp_tools_through_rovik() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let call = json!({ "tool": "docs.read", "arguments": { "path": "intro" } });
    let scenario = json!({ "steps": [
        { "rovik": "mcp", "id": "c1", "arguments": call },
        { "text": "Read: {result:c1}" },
    ] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 that reads TOML: skipped");
        return;
    };
    let permissions = Permissions {
        mcp_approval: permissions::ApprovalRule {
            required: false,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    let request = super::codex_tests::codex_send(
        &h,
        &program,
        "agent",
        "Read the intro",
        json!({}),
        permissions,
    );
    h.start(with_docs(request, &docs)).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(*got.lock().unwrap(), read_of("intro"));
    assert!(h.events_of("approvalRequested").is_empty());

    let kept = std::fs::read_to_string(h._dir.0.join("codex.json")).unwrap();
    let record: Value = serde_json::from_str(&kept).unwrap();
    assert_eq!(record["listed"]["rovik"], json!(["mcp"]));
    assert_eq!(record["results"]["c1"], read_of("intro")[0]);
    let started = record["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["method"] == "thread/start")
        .unwrap();
    let developer = started["params"]["developerInstructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        developer.contains("Call one with the `mcp__rovik__mcp` tool"),
        "{developer}"
    );
    let args: Vec<String> = serde_json::from_value(record["args"].clone()).unwrap();
    let rovik = args
        .iter()
        .find(|a| a.starts_with("mcp_servers.rovik="))
        .unwrap();
    assert!(
        rovik.contains("omit_tools_from=[\"code_mode\",\"deferred\"]"),
        "{rovik}"
    );
    let reply = h.messages().await.pop().unwrap();
    assert_eq!(reply.content, format!("Read: {}", read_of("intro")[0]));
}

/// Q09: in Plan mode Claude Code is Rovik's agent: `mcp` is one of the
/// tools Rovik serves it, the list in Rovik's prompt, and its call is the
/// reply's, run by Rovik as its own.
#[cfg(unix)]
#[tokio::test]
async fn in_plan_mode_claude_code_calls_mcp_tools_as_rovik_s_agent() {
    let h = harness(&[]).await;
    let (_rovik, docs, got) = docs_server().await;
    let call = json!({ "tool": "docs.read", "arguments": { "path": "intro" } });
    let Some(program) = crate::connectors::fake_claude_with_mcp(&h._dir.0, "mcp", &call) else {
        eprintln!("no python3: skipped");
        return;
    };
    let permissions = Permissions {
        mcp_approval: permissions::ApprovalRule {
            required: false,
            by: permissions::Approver::User,
        },
        ..quiet_permissions()
    };
    let mut request = h.request(
        "connector://claude-code",
        Harness::send("Plan it"),
        permissions,
    );
    request.mode = Some("plan".into());
    request.profile.id = "cc::sonnet".into();
    request.profile.model = "sonnet".into();
    request.profile.connector = Some(json!({ "program": program }));
    h.start(with_docs(request, &docs)).await;
    assert_eq!(h.run_end().await, "done");
    assert_eq!(*got.lock().unwrap(), read_of("intro"));
    let system = std::fs::read_to_string(h._dir.0.join("system.txt")).unwrap();
    assert!(system.contains("## MCP tools"), "{system}");
    assert!(system.contains("- read(path)"), "{system}");
    assert!(
        !system.contains("- bash("),
        "Plan mode: read-only tools: {system}"
    );
    let reply = h.messages().await.pop().unwrap();
    assert_eq!(reply.content, format!("Got: {}", read_of("intro")[0]));
}
