//! The Codex connector's own tests: its settings, the app server's
//! arguments, its policies, its sign-in, its models and its commands. Its
//! parser, hooks, patches and wire are tested beside them; whole runs, with
//! a stand-in for `codex`, in agent_run's connector tests.

use super::*;

fn turn(prompt: &str) -> Turn {
    Turn {
        settings: json!({}),
        api_key: String::new(),
        model: "gpt-6.1-sol".into(),
        system_prompt: "You are Rovik.".into(),
        system_context: Some("## Project\n\nRust.".into()),
        prompt: prompt.into(),
        cwd: None,
        isolated: false,
        plan_mode: false,
        session: None,
        recap: None,
        one_shot: false,
        effort: Some(ReasoningEffort::High),
        first_byte_timeout: Duration::from_secs(60),
        dialog_id: Some("d1".into()),
        tools: vec![
            json!({ "type": "function", "function": { "name": "read", "parameters": {} } }),
        ],
        answers: None,
        continue_reply: false,
        asks: None,
        commands: None,
        denied: Default::default(),
    }
}

fn settings(value: Value) -> Settings {
    Settings::from_value(&value)
}

/// The value of a `-c key=…` argument, as written.
fn setting<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    args.windows(2)
        .find(|w| w[0] == "-c" && w[1].starts_with(&prefix))
        .map(|w| &w[1][prefix.len()..])
}

#[test]
fn settings_default_to_rovik_s_permissions_and_codex_s_own_tools() {
    let s = settings(json!({}));
    assert_eq!(s.permission_mode, PermissionMode::Rovik);
    assert_eq!(s.tools, ToolsMode::Own);
    assert_eq!(s.auth, Auth::Login);
    let s = settings(json!({ "permissionMode": "fullAccess", "auth": "apiKey", "tools": "rovik" }));
    assert_eq!(
        (s.permission_mode, s.auth, s.tools),
        (PermissionMode::FullAccess, Auth::ApiKey, ToolsMode::Rovik)
    );
}

#[test]
fn each_permission_mode_is_codex_s_policy_and_plan_mode_is_read_only() {
    let t = turn("hi");
    let of = |mode: &str| policy(&settings(json!({ "permissionMode": mode })), &t);
    assert_eq!(
        of("rovik"),
        Policy {
            approval: "untrusted",
            sandbox: "danger-full-access",
            reviewer: None
        }
    );
    assert_eq!(of("auto").reviewer, Some("auto_review"));
    assert_eq!(
        (of("auto").approval, of("auto").sandbox),
        ("on-request", "workspace-write")
    );
    assert_eq!(
        (of("workspace").approval, of("workspace").sandbox),
        ("never", "workspace-write")
    );
    assert_eq!(of("readOnly").sandbox, "read-only");
    assert_eq!(of("fullAccess").sandbox, "danger-full-access");
    // Plan mode, whatever the mode: read-only, never asking.
    let plan = Turn {
        plan_mode: true,
        ..turn("plan it")
    };
    let p = policy(&settings(json!({ "permissionMode": "fullAccess" })), &plan);
    assert_eq!((p.approval, p.sandbox), ("never", "read-only"));
    // A one-off request and Rovik's tools: Codex's own tools idle.
    let one_off = Turn {
        one_shot: true,
        ..turn("title")
    };
    assert_eq!(policy(&settings(json!({})), &one_off).sandbox, "read-only");
    assert_eq!(
        policy(&settings(json!({ "tools": "rovik" })), &t).sandbox,
        "read-only"
    );
}

#[test]
fn hooks_follow_the_run_codex_s_asks_only_under_rovik_s_permissions() {
    let (checks, _c) = mpsc::unbounded_channel();
    let (asks, _a) = mpsc::unbounded_channel();
    let chat = Turn {
        commands: Some(checks),
        asks: Some(asks),
        ..turn("go")
    };
    assert_eq!(
        hook_events(&settings(json!({})), &chat),
        [
            Event::PreToolUse,
            Event::PermissionRequest,
            Event::PostToolUse
        ]
    );
    assert_eq!(
        hook_events(&settings(json!({ "permissionMode": "workspace" })), &chat),
        [Event::PreToolUse, Event::PostToolUse]
    );
    // Plan mode never asks: nothing for Codex to ask about.
    let plan = Turn {
        plan_mode: true,
        ..chat
    };
    assert_eq!(
        hook_events(&settings(json!({})), &plan),
        [Event::PreToolUse, Event::PostToolUse]
    );
    assert_eq!(
        hook_events(&settings(json!({ "tools": "rovik" })), &plan),
        [Event::PreToolUse]
    );
    let one_off = Turn {
        one_shot: true,
        ..plan
    };
    assert!(hook_events(&settings(json!({})), &one_off).is_empty());
    // Outside a run (nothing to check with): none.
    assert!(hook_events(&settings(json!({})), &turn("go")).is_empty());
}

#[tokio::test]
async fn the_app_server_s_arguments_name_rovik_s_servers_and_the_settings_own_last() {
    let server = McpServer::start(&[], "", None).await.unwrap().0;
    let s = settings(json!({ "extraArgs": "-c 'model_reasoning_summary=\"detailed\"'" }));
    let served = [Served {
        name: hooks::SERVER,
        server: &server,
        token_var: HOOKS_TOKEN,
        hidden: true,
    }];
    let args = args_for(&s, &turn("go"), &served, &[Event::PreToolUse], "");
    assert_eq!(args[0], "app-server");
    let hooks_server = setting(&args, "mcp_servers.rovik_hooks").unwrap();
    assert!(
        hooks_server.contains(&format!("url=\"{}\"", server.url())),
        "{hooks_server}"
    );
    assert!(hooks_server.contains("bearer_token_env_var=\"ROVIK_HOOKS_TOKEN\""));
    assert!(hooks_server.contains("omit_tools_from=[\"code_mode\",\"deferred\",\"direct\"]"));
    // The token is never on the command line.
    assert!(!args.iter().any(|a| a.contains(server.token())));
    assert_eq!(setting(&args, "features.hooks"), Some("true"));
    assert!(setting(&args, "hooks.PreToolUse").is_some());
    // Codex's own tools stay on with its own tools.
    assert_eq!(setting(&args, "features.shell_tool"), None);
    assert_eq!(args.last().unwrap(), "model_reasoning_summary=\"detailed\"");
    // An API key goes over the input, kept in memory: never stored.
    let keyed = settings(json!({ "auth": "apiKey" }));
    let args = args_for(&keyed, &turn("go"), &[], &[], "sk-test");
    assert_eq!(
        setting(&args, "cli_auth_credentials_store"),
        Some("\"ephemeral\"")
    );
    assert!(!args.iter().any(|a| a.contains("sk-test")));
    assert_eq!(
        setting(
            &args_for(&keyed, &turn("go"), &[], &[], ""),
            "cli_auth_credentials_store"
        ),
        None
    );
}

#[test]
fn with_rovik_s_tools_codex_s_own_are_off_and_rovik_s_are_functions_of_their_own() {
    let args = args_for(
        &settings(json!({ "tools": "rovik" })),
        &turn("go"),
        &[],
        &[],
        "",
    );
    for feature in ["shell_tool", "unified_exec", "view_image", "multi_agent"] {
        assert_eq!(
            setting(&args, &format!("features.{feature}")),
            Some("false"),
            "{feature}"
        );
    }
    assert_eq!(setting(&args, "web_search"), Some("\"disabled\""));
    assert_eq!(setting(&args, "project_doc_max_bytes"), Some("0"));
    let code_mode = setting(&args, "features.code_mode").unwrap();
    assert!(code_mode.contains("enabled=false"), "{code_mode}");
    assert!(
        code_mode.contains(r#"direct_only_tool_namespaces=["mcp__rovik"]"#),
        "{code_mode}"
    );
    // A one-off request: no tools either, but no Rovik tools to call.
    let one_off = Turn {
        one_shot: true,
        ..turn("title")
    };
    let args = args_for(&settings(json!({})), &one_off, &[], &[], "");
    assert_eq!(setting(&args, "features.shell_tool"), Some("false"));
    assert_eq!(setting(&args, "features.code_mode"), None);
}

#[test]
fn the_environment_carries_the_token_and_the_settings_folder() {
    let s = settings(json!({ "auth": "token", "configDir": "/srv/codex" }));
    let env = env_for(&s, " at-123 ");
    assert!(env.contains(&("CODEX_ACCESS_TOKEN".into(), "at-123".into())));
    assert!(env.contains(&("CODEX_HOME".into(), "/srv/codex".into())));
    // An API key never goes in the environment: Codex's app server would
    // ignore it, and every program it starts would see it.
    let keyed = env_for(&settings(json!({ "auth": "apiKey" })), "sk-test");
    assert!(!keyed.iter().any(|(_, v)| v.contains("sk-test")));
}

#[test]
fn a_session_is_kept_for_the_tools_its_thread_was_started_with() {
    assert_eq!(session_of("t1", false), "t1");
    assert_eq!(session_of("t1", true), "rovik:t1");
    assert_eq!(thread_of(Some("t1"), false).as_deref(), Some("t1"));
    assert_eq!(thread_of(Some("rovik:t1"), true).as_deref(), Some("t1"));
    // The other tools' thread isn't continued.
    assert_eq!(thread_of(Some("rovik:t1"), false), None);
    assert_eq!(thread_of(Some("t1"), true), None);
    assert_eq!(thread_of(None, false), None);
    assert!(is_gone("no rollout found for thread id 0199"));
    assert!(is_gone("invalid session id: x"));
    assert!(!is_gone("model not available"));
}

#[test]
fn a_picked_command_is_codex_s_and_a_typed_one_is_text() {
    let skills = skills_of(&json!({ "data": [{ "cwd": "/p", "skills": [
        { "name": "pdf", "description": "Read PDFs", "path": "/home/u/.codex/skills/pdf/SKILL.md", "enabled": true },
        { "name": "off", "description": "Off", "path": "/x", "enabled": false },
    ] }] }));
    assert_eq!(ask_of("/compact", &skills), Ask::Compact);
    assert_eq!(ask_of("/review", &skills), Ask::Review(String::new()));
    assert_eq!(
        ask_of("/review the API", &skills),
        Ask::Review("the API".into())
    );
    assert_eq!(
        ask_of("/pdf summarize it", &skills),
        Ask::Skill {
            name: "pdf".into(),
            path: "/home/u/.codex/skills/pdf/SKILL.md".into(),
            args: "summarize it".into()
        }
    );
    // Typed by hand (after a space), unknown, or off: a message.
    assert_eq!(ask_of(" /compact", &skills), Ask::Text(" /compact".into()));
    assert_eq!(
        ask_of("/etc/passwd has a typo", &skills),
        Ask::Text("/etc/passwd has a typo".into())
    );
    assert_eq!(ask_of("/off", &skills), Ask::Text("/off".into()));
}

#[test]
fn each_request_is_the_app_server_s_method_for_it() {
    let t = turn("go");
    let fix = Ask::Text("Fix it".into());
    let (method, params) = request_for(&fix, "t1", &t, Some("gpt-6.1-sol"), false);
    assert_eq!(method, "turn/start");
    assert_eq!(
        params["input"],
        json!([{ "type": "text", "text": "Fix it", "text_elements": [] }])
    );
    assert_eq!(params["effort"], "high");
    assert_eq!(params["summary"], "auto");
    assert!(params.get("collaborationMode").is_none());
    let (method, params) = request_for(&Ask::Compact, "t1", &t, None, false);
    assert_eq!(
        (method, params),
        ("thread/compact/start", json!({ "threadId": "t1" }))
    );
    let (method, params) = request_for(&Ask::Review(String::new()), "t1", &t, None, false);
    assert_eq!(method, "review/start");
    assert_eq!(params["target"], json!({ "type": "uncommittedChanges" }));
    let (_, params) = request_for(&Ask::Review("the docs".into()), "t1", &t, None, false);
    assert_eq!(
        params["target"],
        json!({ "type": "custom", "instructions": "the docs" })
    );
    let skill = Ask::Skill {
        name: "pdf".into(),
        path: "/s/SKILL.md".into(),
        args: "this one".into(),
    };
    let (_, params) = request_for(&skill, "t1", &t, None, false);
    assert_eq!(
        params["input"][0],
        json!({ "type": "skill", "name": "pdf", "path": "/s/SKILL.md" })
    );
    assert_eq!(params["input"][1]["text"], "$pdf this one");
    // Its own plan mode, with the thread's model.
    let plan = Turn {
        plan_mode: true,
        ..turn("plan")
    };
    let (_, params) = request_for(
        &Ask::Text("Plan".into()),
        "t1",
        &plan,
        Some("gpt-6.1-sol"),
        true,
    );
    assert_eq!(params["collaborationMode"]["mode"], "plan");
    assert_eq!(
        params["collaborationMode"]["settings"]["model"],
        "gpt-6.1-sol"
    );
    // A one-off request's reasoning isn't shown.
    let one_off = Turn {
        one_shot: true,
        effort: None,
        ..turn("title")
    };
    let (_, params) = request_for(&Ask::Text("Title".into()), "t1", &one_off, None, false);
    assert_eq!(params["summary"], "none");
    assert!(params.get("effort").is_none());
}

/// In a chat's Plan mode Codex plans in its own plan mode only with its
/// own tools: with Rovik's, the plan mode is Rovik's (an Autopilot task's
/// plan stage too), and a one-off request has none.
#[test]
fn codex_plans_on_its_own_only_with_its_own_tools() {
    let plan = Turn {
        plan_mode: true,
        ..turn("plan")
    };
    let own = Settings::default();
    let rovik = Settings {
        tools: ToolsMode::Rovik,
        ..Settings::default()
    };
    assert!(plans_on_its_own(&own, &plan));
    assert!(!plans_on_its_own(&rovik, &plan));
    assert!(!plans_on_its_own(&own, &turn("go")));
    let one_off = Turn {
        one_shot: true,
        ..plan
    };
    assert!(!plans_on_its_own(&own, &one_off));
}

#[test]
fn the_commands_are_codex_s_own_then_its_skills() {
    let listed = json!({ "data": [{ "skills": [
        { "name": "pdf", "description": "Long", "shortDescription": "Read PDFs", "path": "/a", "enabled": true },
        { "name": "compact", "description": "A skill named like a command", "path": "/b", "enabled": true },
    ] }] });
    let commands = commands_from(&listed);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["compact", "review", "pdf"]);
    assert!(commands[0].builtin && commands[1].builtin && !commands[2].builtin);
    assert_eq!(commands[2].description, "Read PDFs");
    assert_eq!(
        commands[1].argument_hint.as_deref(),
        Some("[what to review]")
    );
}

#[test]
fn the_models_are_those_codex_shows_its_default_first() {
    let listed = json!({ "data": [
        { "id": "gpt-6-astra", "displayName": "GPT-6-Astra", "description": "Frontier", "hidden": false, "isDefault": false },
        { "id": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "description": "Workhorse", "hidden": false, "isDefault": true },
        { "id": "codex-auto-review", "hidden": true },
    ] });
    let models = models_of(&listed);
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6-astra"]);
    assert_eq!(
        models[0].description.as_deref(),
        Some("GPT-6.1-Sol: Workhorse")
    );
    assert_eq!(models[0].owned_by.as_deref(), Some("openai"));
}

#[test]
fn the_sign_in_is_codex_s_answer_as_the_settings_see_it() {
    let login = settings(json!({}));
    let auth = parse_auth_status("", "Logged in using ChatGPT\n", &login, "").unwrap();
    assert!(auth.logged_in);
    assert_eq!(auth.method.as_deref(), Some("chatgpt"));
    let out = parse_auth_status("", "Not logged in\n", &login, "").unwrap();
    assert!(!out.logged_in);
    assert!(out.detail.unwrap().contains("codex login"));
    let key = parse_auth_status(
        "",
        "API key configured - sk-proj-***abc\nNote: API key authentication…",
        &login,
        "",
    )
    .unwrap();
    assert_eq!(key.method.as_deref(), Some("api_key"));
    assert!(
        parse_auth_status("", "Error checking login status: bad file", &login, "")
            .unwrap_err()
            .contains("bad file")
    );
    assert!(parse_auth_status("", "", &login, "").is_err());
    // An API key of the settings is the run's sign-in, whatever is stored.
    let keyed = settings(json!({ "auth": "apiKey" }));
    let with_key = parse_auth_status("", "Not logged in", &keyed, "sk-x").unwrap();
    assert!(with_key.logged_in);
    assert_eq!(with_key.method.as_deref(), Some("api_key"));
    let without = parse_auth_status("", "Logged in using ChatGPT", &keyed, "").unwrap();
    assert!(without.detail.unwrap().contains("this computer's login"));
    // A token Codex didn't take.
    let token = settings(json!({ "auth": "token" }));
    let refused = parse_auth_status("", "Not logged in", &token, "at-1").unwrap();
    assert!(!refused.logged_in);
    assert!(
        refused.detail.unwrap().contains("doesn't take"),
        "a token was set"
    );
    let custom = settings(json!({ "configDir": "/srv/codex" }));
    let dir = parse_auth_status("", "Logged in using ChatGPT", &custom, "").unwrap();
    assert_eq!(dir.config_dir.as_deref(), Some("/srv/codex"));
}

#[test]
fn the_newest_version_is_the_registry_s_answer() {
    assert_eq!(
        latest_of(r#"{"name":"@openai/codex","version":"0.165.0"}"#).as_deref(),
        Some("0.165.0")
    );
    assert_eq!(latest_of("<html>error</html>"), None);
    assert_eq!(latest_of(r#"{"version":"latest; rm -rf"}"#), None);
}

#[test]
fn set_up_installs_with_openai_s_installer_and_signs_in_as_the_settings_say() {
    let codex = Codex;
    let args = |settings: Value| ProbeRequestArgs {
        kind: ConnectorKind::Codex,
        settings,
        api_key: String::new(),
        request: false,
    };
    let install = codex
        .setup_command(SetupAction::Install, &args(json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(install.shown, INSTALL);
    // A token or a key isn't signed in with: it's pasted in the settings.
    let keyed = codex.setup_command(SetupAction::Login, &args(json!({ "auth": "apiKey" })));
    assert!(keyed.unwrap_err().contains("API key"));
    let token = codex.setup_command(SetupAction::Login, &args(json!({ "auth": "token" })));
    assert!(token.unwrap_err().contains("access token"));
}

/// `codex update` finds its installation by the settings folder OpenAI's
/// installer ran with: the update runs with that one, whatever the
/// connector's own, and without its sign-in. A package manager's
/// installation needs none.
#[cfg(unix)]
#[test]
fn an_update_runs_with_the_settings_folder_codex_was_installed_with() {
    use std::os::unix::fs::symlink;
    let dir = crate::testing::temp_dir();
    // The installer's layout: the program a link into the current release.
    let home = dir.0.join("home");
    let release = home.join(RELEASES).join("0.1.0-x86_64-unknown-linux-musl");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::write(release.join("bin/codex"), "").unwrap();
    let standalone = home.join("packages/standalone");
    symlink(&release, standalone.join("current")).unwrap();
    std::fs::create_dir_all(dir.0.join("bin")).unwrap();
    let program = dir.0.join("bin/codex");
    symlink(standalone.join("current/bin/codex"), &program).unwrap();
    let home = std::fs::canonicalize(&home).unwrap();
    assert_eq!(install_home(&program.to_string_lossy()), Some(home.clone()));

    let args = ProbeRequestArgs {
        kind: ConnectorKind::Codex,
        settings: json!({
            "program": program,
            "auth": "token",
            "configDir": dir.0.join("work"),
        }),
        api_key: "the-token".into(),
        request: false,
    };
    let update = Codex
        .setup_command(SetupAction::Update, &args)
        .unwrap()
        .unwrap();
    assert_eq!(update.args, ["update"]);
    let home = home.to_string_lossy().into_owned();
    assert_eq!(update.env, [("CODEX_HOME".to_string(), home)]);

    // npm's, or one of the user's own making: no folder of the installer's.
    let npm = dir.0.join("npm/codex");
    std::fs::create_dir_all(npm.parent().unwrap()).unwrap();
    std::fs::write(&npm, "").unwrap();
    assert_eq!(install_home(&npm.to_string_lossy()), None);
}

/// A user's update of Codex installed for everyone (`admin codex`) isn't
/// run: it's an administrator's, and the user is told how.
#[test]
fn an_installation_for_everyone_is_an_administrators_to_update() {
    let shared = Path::new("/opt/codex");
    let refused = update_env(Some(shared), false).unwrap_err();
    assert!(refused.contains("/opt/codex"), "{refused}");
    assert!(refused.contains("rovik-agentd admin codex"), "{refused}");
    assert_eq!(
        update_env(Some(shared), true).unwrap(),
        [("CODEX_HOME".to_string(), "/opt/codex".to_string())]
    );
    assert_eq!(update_env(None, false).unwrap(), []);
}

/// What a user may change: a folder of their own; not one that isn't there.
#[cfg(unix)]
#[test]
fn a_users_own_folder_is_writable_and_a_missing_one_is_not() {
    let dir = crate::testing::temp_dir();
    assert!(writable(&dir.0));
    assert!(!writable(&dir.0.join("missing")));
}

#[test]
fn a_missing_program_says_how_to_install_it() {
    let error = resolve_program("/nowhere/codex").unwrap_err();
    assert!(error.contains("no such program"), "{error}");
    let error = resolve_program("codex-that-isnt-there").unwrap_err();
    assert!(error.contains("Install Codex"), "{error}");
}
