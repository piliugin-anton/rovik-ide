//! A project's own MCP servers (mcp-plan-2.md, Q12): read from its three
//! files, duplicates merged, the user's own winning, and filled in as
//! they run.

use super::*;
use crate::testing::temp_dir;
use serde_json::json;
use std::path::PathBuf;

fn write(root: &Path, file: &str, json: Value) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, json.to_string()).unwrap();
}

fn value(name: &str, value: &str, secret: bool) -> McpValue {
    McpValue {
        name: name.into(),
        value: value.into(),
        secret,
    }
}

fn variable(name: &str) -> ProjectVariable {
    ProjectVariable {
        name: name.into(),
        description: String::new(),
        default: None,
        password: false,
    }
}

/// The user's own server, a command.
fn users(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        id: format!("id-{name}"),
        name: name.into(),
        enabled: true,
        transport: McpTransport::Command {
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Vec::new(),
        },
        disabled_tools: Vec::new(),
    }
}

/// A project with a server in each of its three files.
fn project() -> (crate::testing::TempDir, PathBuf) {
    let dir = temp_dir();
    let root = dir.0.clone();
    write(
        &root,
        ".mcp.json",
        json!({ "mcpServers": {
            "GitHub": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github", "${workspaceFolder}"],
                "env": { "GITHUB_TOKEN": "${GITHUB_TOKEN}", "LOG_LEVEL": "${LEVEL:-info}" },
            },
            "off": { "command": "uvx", "disabled": true },
        } }),
    );
    write(
        &root,
        ".cursor/mcp.json",
        json!({ "mcpServers": {
            "sentry": { "url": "https://mcp.sentry.dev/mcp", "headers": { "Authorization": "Bearer ${env:SENTRY_TOKEN}" } },
        } }),
    );
    write(
        &root,
        ".vscode/mcp.json",
        json!({
            "inputs": [{ "type": "promptString", "id": "db", "description": "The database's password", "password": true }],
            "servers": { "db": { "type": "stdio", "command": "db-mcp", "env": { "DB_PASSWORD": "${input:db}" } } },
        }),
    );
    (dir, root)
}

/// Q12: the servers of `.mcp.json`, `.cursor/mcp.json` and
/// `.vscode/mcp.json`, in that order, their values as written and the
/// variables they name for the user to fill (an input as VS Code describes
/// it, a default as written); one turned off in its file is left out.
#[test]
fn a_project_s_three_files_give_its_servers_and_their_variables() {
    let (_dir, root) = project();
    let found = read(&root, &[]);
    let names: Vec<(&str, &str)> = (found.servers.iter())
        .map(|s| (s.name.as_str(), s.file.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("github", ".mcp.json"),
            ("sentry", ".cursor/mcp.json"),
            ("db", ".vscode/mcp.json"),
        ]
    );
    let github = &found.servers[0];
    assert_eq!(
        github.transport,
        McpTransport::Command {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-github".into(),
                "${workspaceFolder}".into(),
            ],
            env: vec![
                value("GITHUB_TOKEN", "${GITHUB_TOKEN}", true),
                value("LOG_LEVEL", "${LEVEL:-info}", true),
            ],
        }
    );
    let level = ProjectVariable {
        default: Some("info".into()),
        ..variable("LEVEL")
    };
    assert_eq!(github.variables, [variable("GITHUB_TOKEN"), level]);
    assert_eq!(found.servers[1].variables, [variable("SENTRY_TOKEN")]);
    let db = ProjectVariable {
        description: "The database's password".into(),
        password: true,
        ..variable("input:db")
    };
    assert_eq!(found.servers[2].variables, [db]);
    assert!(found.left_out.is_empty(), "{:?}", found.left_out);
}

/// Duplicates are one server, the first file's name kept: the same URL
/// and the same auth, or the same command, arguments and environment. One
/// the user's own servers already are is dropped, and one named as one of
/// theirs is left out, saying the user's own wins.
#[test]
fn duplicates_are_one_and_the_user_s_own_win() {
    let dir = temp_dir();
    let sentry = json!({ "url": "https://mcp.sentry.dev/mcp", "headers": { "Authorization": "Bearer ${env:SENTRY_TOKEN}" } });
    let other_token = json!({ "url": "https://mcp.sentry.dev/mcp", "headers": { "Authorization": "Bearer ${env:OTHER}" } });
    write(
        &dir.0,
        ".mcp.json",
        json!({ "mcpServers": { "sentry": sentry, "fetch": { "command": "uvx", "args": ["mcp-server-fetch"] } } }),
    );
    write(
        &dir.0,
        ".cursor/mcp.json",
        json!({ "mcpServers": { "sentry-again": sentry, "sentry-other": other_token } }),
    );
    write(
        &dir.0,
        ".vscode/mcp.json",
        json!({ "servers": { "time": { "command": "uvx", "args": ["mcp-server-time"] } } }),
    );
    let user = [
        users("my-fetch", "uvx", &["mcp-server-fetch"]),
        users("time", "other-time-server", &[]),
    ];
    let found = read(&dir.0, &user);
    let names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["sentry", "sentry-other"],
        "fetch is the user's my-fetch"
    );
    assert_eq!(found.left_out.len(), 1, "{:?}", found.left_out);
    assert!(found.left_out[0].starts_with("time (.vscode/mcp.json) isn't used"));
    assert!(found.left_out[0].contains("the user's own wins"));
}

/// A file that isn't there, or isn't JSON, is passed over.
#[test]
fn a_missing_or_broken_file_is_passed_over() {
    let dir = temp_dir();
    assert_eq!(read(&dir.0, &[]), Found::default());
    std::fs::write(dir.0.join(".mcp.json"), "{ not json").unwrap();
    write(
        &dir.0,
        ".vscode/mcp.json",
        json!({ "servers": { "time": { "command": "uvx" } } }),
    );
    let found = read(&dir.0, &[]);
    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "time");
}

/// An approval is of a definition: the same one has the same hash, and
/// any change to it (an edit, a pull, the agent's) another.
#[test]
fn a_changed_definition_has_another_hash() {
    let (_dir, root) = project();
    let first = read(&root, &[]).servers[0].hash.clone();
    assert_eq!(read(&root, &[]).servers[0].hash, first, "the same");
    write(
        &root,
        ".mcp.json",
        json!({ "mcpServers": { "GitHub": { "command": "npx", "args": ["-y", "@evil/server-github"] } } }),
    );
    assert_ne!(read(&root, &[]).servers[0].hash, first);
}

/// A server runs with its variables filled: a value the user gave, else
/// the default; the folders as they are where it runs. The values of the
/// user's variables are kept out of its settings, as secrets, by key. One
/// with a variable that has neither a value nor a default doesn't run.
#[test]
fn a_project_server_runs_with_its_variables_filled_its_secrets_apart() {
    let (_dir, root) = project();
    let found = read(&root, &[]);
    let given = |name: &str| (name == "GITHUB_TOKEN").then(|| "ghp_1".to_string());
    let folder = Path::new("/work/app");
    let (server, secrets) = resolved(&found.servers[0], "p1", folder, given).unwrap();
    assert_eq!(server.id, format!("project/p1/{}", found.servers[0].hash));
    assert_eq!(server.name, "github");
    assert_eq!(
        server.transport,
        McpTransport::Command {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-github".into(),
                "/work/app".into(),
            ],
            env: vec![
                value("GITHUB_TOKEN", "", true),
                value("LOG_LEVEL", "", true)
            ],
        }
    );
    assert_eq!(
        secrets,
        BTreeMap::from([
            ("env/GITHUB_TOKEN".to_string(), "ghp_1".to_string()),
            ("env/LOG_LEVEL".to_string(), "info".to_string()),
        ])
    );
    assert!(
        !format!("{server:?}").contains("ghp_1"),
        "never in its settings"
    );

    assert_eq!(resolved(&found.servers[1], "p1", folder, |_| None), None);
}

/// The project's settings keep the approved servers' hashes, and the
/// variables they take, which a start brings the values of to a host.
#[test]
fn approvals_are_read_from_the_project_s_settings() {
    let settings = json!({ "mcpApprovals": [
        { "hash": "a1", "name": "github", "file": ".mcp.json", "variables": ["GITHUB_TOKEN"] },
        { "hash": "b2", "name": "time", "file": ".vscode/mcp.json" },
    ] });
    assert_eq!(
        approved(&settings),
        BTreeSet::from(["a1".into(), "b2".into()])
    );
    assert_eq!(
        approved_variables(&settings),
        BTreeSet::from(["GITHUB_TOKEN".to_string()])
    );
    assert!(approved(&Value::Null).is_empty());
    assert_eq!(values_id("p1"), "project/p1");
}

/// Only `${VAR}` and `${VAR:-default}` (in `.mcp.json`), `${env:VAR}`
/// and `${input:id}` are the user's to give; the folders are filled in;
/// anything else in braces stays as written: in Cursor's and VS Code's
/// files a bare name is one of theirs.
#[test]
fn what_braces_name() {
    let text = "a ${workspaceFolder} ${userHome} ${pathSeparator} ${env:X} $HOME ${";
    let filled = |bare| {
        substitute(text, bare, |v| match v {
            Named::Folder => "F".into(),
            Named::Home => "H".into(),
            Named::Given(name, _) => format!("<{name}>"),
        })
    };
    assert_eq!(filled(false), "a F H ${pathSeparator} <X> $HOME ${");
    assert_eq!(
        filled(true),
        "a F H <pathSeparator> <X> $HOME ${",
        "Claude Code's"
    );
    assert!(names_a_value("Bearer ${env:T}", false) && names_a_value("${input:x}", false));
    assert!(names_a_value("${T}", true) && !names_a_value("${T}", false));
    assert!(!names_a_value("${workspaceFolder}/data", true) && !names_a_value("plain", true));
}

/// A variable's name is an env var's or an input's, and never leaves the
/// project's place in the keychain; nor does the project's id.
#[test]
fn a_variable_s_name_stays_in_the_project_s_place() {
    for name in ["GITHUB_TOKEN", "_x1", "input:db", "input:db password"] {
        assert!(is_variable_name(name), "{name}");
    }
    for name in [
        "",
        "1X",
        "A/B",
        "input:",
        "input:../x",
        "input:a\nb",
        "env/X",
    ] {
        assert!(!is_variable_name(name), "{name}");
    }
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|n| n.to_string()).collect() };
    let token = names(&["GITHUB_TOKEN", "input:db"]);
    assert_eq!(values_place("p1", &token), Ok("project/p1".to_string()));
    assert_eq!(
        values_place("p1/../p2", &token),
        Err("p1/../p2 isn't a project's id.".to_string())
    );
    assert_eq!(
        values_place("", &token),
        Err(" isn't a project's id.".to_string())
    );
    assert_eq!(
        values_place("p1", &names(&["GITHUB_TOKEN", "env/X"])),
        Err("env/X isn't a variable's name.".to_string())
    );
}

/// The fields are filled from the environment a server started here gets:
/// an env var that's set; never an input, or a name that isn't a variable's.
#[test]
fn the_environment_fills_the_env_vars_only() {
    let path = crate::process::child_var("PATH").expect("tests run with a PATH");
    let names: Vec<String> = ["PATH", "input:PATH", "ROVIK_NOT_SET_ANYWHERE", "PATH/x"]
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(
        from_environment(&names),
        BTreeMap::from([("PATH".to_string(), path.to_string_lossy().into_owned())])
    );
}
