//! Workspace chats' runs (docs/design/workspaces-plan.md, 4.3): a chat of a
//! workspace works in one view of its folders, by their names, every run,
//! and its changes land in each folder. They need FUSE (skipped without it).

use super::*;

/// A workspace `Shop` of `folders` (each a name and its files) under the
/// test's folder, which is the harness's `project`, so `file("api/x")` reads
/// api's `x`; and a chat of it working in `checked` (`"all"`, or names as
/// JSON), isolated.
async fn workspace_harness(folders: &[(&str, &[(&str, &str)])], checked: &str) -> Harness {
    let TestCore { core, events, dir } = TestCore::new();
    let shop = dir.0.join("shop");
    let mut paths = Vec::new();
    for (name, files) in folders {
        let folder = shop.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for (path, content) in *files {
            let file = folder.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
        paths.push(folder.to_string_lossy().into_owned());
    }
    let workspace = crate::chat_db::api::workspace_create(&core.db, "Shop".into(), paths)
        .await
        .unwrap();
    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: dialog_id.clone(),
        title: "Test".into(),
        created_at: 0,
        updated_at: 0,
        project_id: Some(workspace.id),
        folders: Some(checked.into()),
    };
    core.db
        .write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    let mut h = Harness {
        core,
        project: shop,
        events,
        dialog_id,
        _dir: dir,
        _isolated: None,
    };
    h.isolate().await;
    h
}

/// `api` with `src/a.ts`, `web` with `src/b.ts`.
const SHOP: &[(&str, &[(&str, &str)])] = &[
    ("api", &[("src/a.ts", "export const a = 1;\n")]),
    ("web", &[("src/b.ts", "export const b = 1;\n")]),
];

impl Harness {
    /// A workspace chat's request, as the app sends it: no folder, the
    /// app's isolation.
    fn workspace_request(
        &self,
        url: &str,
        start: StartKind,
        permissions: Permissions,
    ) -> StartRequest {
        StartRequest {
            root: None,
            isolate: None,
            ..self.request(url, start, permissions)
        }
    }
}

fn any_command() -> Permissions {
    Permissions {
        allow_any_command: true,
        ..quiet_permissions()
    }
}

#[tokio::test]
async fn a_workspace_chat_reads_and_edits_two_folders_by_their_names_and_lands_in_each() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "api/src/a.ts" }),
        },
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "web/src/b.ts", "old_text": "b = 1", "content": "b = 2" }),
        },
        Reply::Call {
            id: "w2",
            name: "write",
            args: json!({ "path": "api/src/new.ts", "content": "export const n = 0;\n" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change both"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let read = &result_of(&messages, "r1").content;
    assert!(read.contains("export const a = 1;"), "{read}");
    // Each change landed in its folder, and nothing anywhere else.
    assert_eq!(
        h.file("web/src/b.ts").as_deref(),
        Some("export const b = 2;\n")
    );
    assert_eq!(
        h.file("api/src/new.ts").as_deref(),
        Some("export const n = 0;\n")
    );
    let mut top: Vec<String> = std::fs::read_dir(&h.project)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, ["api", "web"]);
    let mut landed = crate::overlay::chats::last_landing(&h.dialog_id)
        .unwrap()
        .paths;
    landed.sort();
    assert_eq!(landed, ["api/src/new.ts", "web/src/b.ts"]);
}

#[tokio::test]
async fn a_folder_not_checked_for_the_chat_isnt_in_its_view() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, r#"["api"]"#).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "ls" }),
        },
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "web/src/b.ts" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Look"), any_command()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let listed = &result_of(&messages, "b1").content;
    assert!(
        listed.contains("api") && !listed.contains("web"),
        "{listed}"
    );
    let read = &result_of(&messages, "r1").content;
    assert!(read.starts_with("Error"), "{read}");
}

#[tokio::test]
async fn plan_mode_works_in_the_view_and_changes_nothing() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "web/src/b.ts" }),
        },
        Reply::Text("It's one constant."),
    ])
    .await;
    h.start(h.workspace_request(&url, plan_send("What's in web?"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let read = &result_of(&messages, "r1").content;
    assert!(read.contains("export const b = 1;"), "{read}");
    let view = crate::overlay::chats::get(&h.dialog_id).expect("its view is open");
    assert!(view.view.join("api").is_dir() && view.view.join("web").is_dir());
    assert!(crate::overlay::chats::changes(&h.dialog_id).is_empty());
    assert!(crate::overlay::chats::last_landing(&h.dialog_id).is_none());
    assert_eq!(
        h.file("web/src/b.ts").as_deref(),
        Some("export const b = 1;\n")
    );
    crate::overlay::chats::discard(&h.dialog_id).unwrap();
}

#[tokio::test]
async fn a_workspace_chat_is_refused_without_the_driver_or_with_isolation_off() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model(vec![Reply::Text("Done.")]).await;
    // Its view can't be mounted, as without the driver: something else
    // is where it goes.
    let at = crate::overlay::chats::mountpoint(&h.dialog_id);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(&at, "").unwrap();
    let request = h.workspace_request(&url, Harness::send("Go"), quiet_permissions());
    let refused = agent_start(&h.core, request).await.unwrap_err();
    std::fs::remove_file(&at).unwrap();
    assert!(refused.starts_with(super::run::NEEDS_VIEW), "{refused}");
    assert!(crate::overlay::chats::get(&h.dialog_id).is_none());
    assert!(!crate::overlay::chats::exists(&h.dialog_id));

    let mut request = h.workspace_request(&url, Harness::send("Go"), quiet_permissions());
    request.isolate = Some(false);
    let refused = agent_start(&h.core, request).await.unwrap_err();
    assert!(refused.contains("isolation is off"), "{refused}");
    // Neither worked in a folder instead.
    assert!(crate::overlay::chats::get(&h.dialog_id).is_none());
    assert_eq!(
        h.file("api/src/a.ts").as_deref(),
        Some("export const a = 1;\n")
    );
}

#[tokio::test]
async fn land_and_undo_are_refused_while_a_folder_is_missing() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "web/src/b.ts", "old_text": "b = 1", "content": "b = 2" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change web"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let landing = crate::overlay::chats::last_landing(&h.dialog_id).unwrap();

    // More changes, in both folders, as a run would leave them waiting.
    let folders = check_project(&h.core.db, &h.dialog_id, None, &[])
        .await
        .unwrap()
        .unwrap();
    let view = match crate::overlay::chats::open(&h.dialog_id, &folders, Box::new(|_, _| {})) {
        Ok(view) => view,
        Err(e) => panic!("{e}"),
    };
    std::fs::write(view.view.join("api/src/a.ts"), "export const a = 2;\n").unwrap();
    std::fs::write(view.view.join("web/src/c.ts"), "export const c = 3;\n").unwrap();
    view.fs.flush();
    crate::overlay::chats::close(&h.dialog_id);

    // web is away: a drive that isn't mounted.
    std::fs::rename(h.project.join("web"), h.project.join("web-away")).unwrap();
    let agent = &h.core.agents;
    let land = crate::overlay::chats::overlay_land(agent, h.dialog_id.clone(), false)
        .await
        .unwrap();
    let refused = land.error.unwrap_or_default();
    assert!(refused.starts_with("web isn't there"), "{refused}");
    let refused =
        crate::overlay::chats::overlay_undo(agent, h.dialog_id.clone(), landing.id.clone())
            .await
            .unwrap_err();
    assert!(refused.starts_with("web isn't there"), "{refused}");
    // Nothing was written in api either, and the changes still wait.
    assert_eq!(
        h.file("api/src/a.ts").as_deref(),
        Some("export const a = 1;\n")
    );
    assert_eq!(crate::overlay::chats::changes(&h.dialog_id).len(), 2);
    crate::overlay::chats::discard(&h.dialog_id).unwrap();
}

/// The system prompt of the `n`th request the model was sent.
fn system_of(seen: &Arc<Mutex<Vec<Value>>>, n: usize) -> String {
    seen.lock().unwrap()[n]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A folder's instructions as the prompt has them, up to their end or
/// where they were cut.
fn instructions_of<'a>(system: &'a str, name: &str) -> &'a str {
    let label =
        format!("{name}'s instructions for agents ({name}/AGENTS.md); follow them in {name}:\n\n");
    let start = system.find(&label).expect(&label) + label.len();
    let rest = &system[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    rest[..end].trim_end_matches("\n[…truncated]")
}

#[tokio::test]
async fn each_folders_instructions_checks_and_git_are_in_the_prompt_under_its_name() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    ("AGENTS.md", "Use tabs in api.\n"),
                    ("package.json", r#"{"scripts":{"test":"node t.js"}}"#),
                ],
            ),
            (
                "web",
                &[
                    ("AGENTS.md", "Use spaces in web.\n"),
                    ("package.json", r#"{"scripts":{"lint":"eslint ."}}"#),
                ],
            ),
        ],
        "all",
    )
    .await;
    for name in ["api", "web"] {
        turn_on_detected_checks(&h.core.paths.config_dir, &h.project.join(name));
    }
    let (url, seen) = model(vec![Reply::Text("Hello.")]).await;
    h.start(h.workspace_request(&url, Harness::send("Hi"), Permissions::default()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let system = system_of(&seen, 0);
    assert_eq!(instructions_of(&system, "api"), "Use tabs in api.");
    assert_eq!(instructions_of(&system, "web"), "Use spaces in web.");
    assert!(system.contains("`cd api && npm test`"), "{system}");
    assert!(system.contains("`cd web && npm run lint`"), "{system}");
    assert!(
        system.contains("Git: `api` isn't in a repository; `web` isn't in a repository."),
        "{system}"
    );
    assert!(!system.contains("The project's instructions"), "{system}");
}

#[tokio::test]
async fn the_folders_instructions_share_12000_characters_and_a_short_ones_rest_goes_to_the_others()
{
    if !crate::vfs::mount_works() {
        return;
    }
    let line = format!("{}\n", "x".repeat(99));
    let (api, web) = (
        format!("API\n{}", line.repeat(100)),
        format!("WEB\n{}", line.repeat(100)),
    );
    let docs = format!("Short.\n{}", line.repeat(9));
    let h = workspace_harness(
        &[
            ("api", &[("AGENTS.md", api.as_str())]),
            ("web", &[("AGENTS.md", web.as_str())]),
            ("docs", &[("AGENTS.md", docs.as_str())]),
        ],
        "all",
    )
    .await;
    let (url, seen) = model(vec![Reply::Text("Hello.")]).await;
    h.start(h.workspace_request(&url, Harness::send("Hi"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let system = system_of(&seen, 0);
    let len = |name: &str| instructions_of(&system, name).chars().count();
    assert_eq!(instructions_of(&system, "docs"), docs.trim(), "whole");
    // What docs doesn't use goes to the others, evenly.
    let room = (12_000 - docs.trim().chars().count()) / 2;
    for name in ["api", "web"] {
        assert!(
            len(name) > 6_000 - 1_000 && len(name) <= room,
            "{name}: {}",
            len(name)
        );
    }
    assert!(len("api") + len("web") + len("docs") <= 12_000);
    assert_eq!(system.matches("[…truncated]").count(), 2);
}

#[tokio::test]
async fn a_folders_restricted_files_are_refused_by_their_paths_in_the_view_and_only_its_own() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(
        &[
            (
                "api",
                &[(".env", "KEY=1\n"), ("config.json", "{\"api\": 1}\n")],
            ),
            ("web", &[("config.json", "{\"web\": 1}\n")]),
            ("examples", &[("secrets.yaml", "token: x\n")]),
        ],
        "all",
    )
    .await;
    // web's own list keeps its config.json from the agent; api's says nothing.
    crate::project_profile::save_project_settings(
        &h.core.paths.config_dir,
        h.project.join("web").to_string_lossy().into_owned(),
        json!({ "restrictedFiles": [{ "path": "config.json" }] }),
    )
    .unwrap();
    let read = |id: &'static str, path: &str| Reply::Call {
        id,
        name: "read",
        args: json!({ "path": path }),
    };
    let (url, _) = model(vec![
        read("r1", "api/.env"),
        read("r2", "web/config.json"),
        read("r3", "api/config.json"),
        read("r4", "examples/secrets.yaml"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Read"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let result = |id: &str| result_of(&messages, id).content.clone();
    assert!(
        result("r1").starts_with("Error: api/.env is a restricted file"),
        "{}",
        result("r1")
    );
    assert!(
        result("r2").starts_with("Error: web/config.json is a restricted file"),
        "{}",
        result("r2")
    );
    assert!(result("r3").contains("\"api\": 1"), "{}", result("r3"));
    // A folder named `examples` keeps its secrets as it does alone.
    assert!(
        result("r4").starts_with("Error: examples/secrets.yaml is a restricted file"),
        "{}",
        result("r4")
    );
}

#[tokio::test]
async fn after_the_chats_folders_change_the_next_prompt_says_which_joined_and_which_left() {
    if !crate::vfs::mount_works() {
        return;
    }
    let shop: &[(&str, &[(&str, &str)])] = &[
        ("api", &[("a.txt", "a\n")]),
        ("web", &[("b.txt", "b\n")]),
        ("docs", &[("c.txt", "c\n")]),
    ];
    let h = workspace_harness(shop, r#"["api","web"]"#).await;
    let (url, seen) = model(vec![Reply::Text("One."), Reply::Text("Two.")]).await;
    h.start(h.workspace_request(&url, Harness::send("First"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    assert!(!system_of(&seen, 0).contains("folders changed"));
    let messages = h.messages().await;
    let first = messages.iter().find(|m| m.role == "user").unwrap();
    assert_eq!(
        first.meta.as_ref().unwrap()["folders"],
        json!(["api", "web"])
    );

    let id = h.dialog_id.clone();
    h.core
        .db
        .write(move |c| store::set_dialog_folders(c, &id, Some(r#"["api","docs"]"#)))
        .await
        .unwrap();
    h.events.lock().unwrap().clear();
    h.start(h.workspace_request(&url, Harness::send("Second"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let system = system_of(&seen, 1);
    assert!(
        system.contains("its folders changed: `docs` joined; `web` left"),
        "{system}"
    );
}

#[tokio::test]
async fn a_command_in_each_folder_runs_there_and_sees_the_chats_changes() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(
        &[("api", &[("a.txt", "a\n")]), ("web", &[("b.txt", "b\n")])],
        "all",
    )
    .await;
    let api = h.project.join("api").canonicalize().unwrap();
    let bash = |id: &'static str, command: String| Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": command }),
    };
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "api/a.txt", "content": "A\n" }),
        },
        bash("b1", "cd api && cat a.txt".into()),
        bash("b2", "cd web && pwd && cat b.txt".into()),
        bash("b3", format!("cat {}/a.txt", api.display())),
        Reply::Text("Done."),
    ])
    .await;
    // The default allowed commands: `cd` into a folder needs none.
    h.start(h.workspace_request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let result = |id: &str| result_of(&messages, id).content.clone();
    let printed = |out: &str, line: &str| out.lines().any(|l| l == line);
    let b1 = result("b1");
    assert!(printed(&b1, "A"), "the chat's change: {b1}");
    let b2 = result("b2");
    assert!(
        b2.lines().any(|l| l.ends_with("/web")) && printed(&b2, "b"),
        "{b2}"
    );
    // A folder's own path: the chat's view where the sandbox shows it
    // there, refused where it can't.
    let b3 = result("b3");
    assert!(
        printed(&b3, "A")
            || b3.starts_with("Error: this chat works in its own view of its folders"),
        "{b3}"
    );
    assert_eq!(h.file("api/a.txt").as_deref(), Some("A\n"), "landed");
}

/*
 * F10: the checks of the folders a workspace chat changed, each in its
 * folder (workspaces-plan.md, 4.7).
 */

/// Each of `checks` (kind, command) on for the folder at `path` (under the
/// harness's `project`), as the user turns them on in its own settings.
fn set_checks(h: &Harness, path: &str, checks: &[(&str, &str)]) {
    let checks: Vec<Value> = checks
        .iter()
        .map(|(kind, command)| json!({ "kind": kind, "command": command, "enabled": true }))
        .collect();
    let folder = h.project.join(path).to_string_lossy().into_owned();
    std::fs::create_dir_all(&h.core.paths.config_dir).unwrap();
    crate::project_profile::save_project_settings(
        &h.core.paths.config_dir,
        folder,
        json!({ "checks": checks }),
    )
    .unwrap();
}

/// The checks on, any command allowed, no checklist.
fn checked() -> Permissions {
    Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    }
}

/// The run's verification records, oldest first.
fn verifications(messages: &[MessageRow]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect()
}

/// A record's check run as `command`.
fn check_in<'a>(record: &'a Value, command: &str) -> &'a Value {
    record["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|c| c["command"] == command))
        .unwrap_or_else(|| panic!("no `{command}` in {record:#}"))
}

/// A type check as tsc prints one: a TODO or a FIXME left in `src/*.ts`,
/// each by its path as `print` writes it (`$f`, relative to where it runs;
/// `$PWD/$f`, absolute).
fn todo_check(print: &str) -> String {
    format!(
        "status=0\nfor f in src/*.ts; do\n  if grep -q TODO \"$f\"; then echo \"{print}(1,1): error TS1000: A TODO is left.\"; status=1; fi\n  if grep -q FIXME \"$f\"; then echo \"{print}(1,1): error TS2000: A FIXME is left.\"; status=1; fi\ndone\nexit $status\n"
    )
}

fn write(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

/// D27: the checks of the folder the chat changed run, in it (`sh
/// check.sh` is found there alone); another folder's don't.
#[tokio::test]
async fn only_the_changed_folders_checks_run_in_it() {
    if !crate::vfs::mount_works() {
        return;
    }
    let check = todo_check("$f");
    let h = workspace_harness(
        &[
            ("api", &[("check.sh", &check), ("src/a.ts", "a\n")]),
            ("web", &[("check.sh", &check), ("src/b.ts", "b\n")]),
        ],
        "all",
    )
    .await;
    for name in ["api", "web"] {
        set_checks(&h, name, &[("typecheck", "sh check.sh")]);
    }
    let (url, _) = model(vec![
        write("w1", "web/src/b.ts", "b2\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change web"), checked()))
        .await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let records = verifications(&h.messages().await);
    let commands: Vec<&Value> = records[0]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["command"])
        .collect();
    assert_eq!(commands, [&json!("cd web && sh check.sh")]);
    assert_eq!(
        check_in(&records[0], "cd web && sh check.sh")["status"],
        "passed"
    );
}

/// D27: the folder the chat changed has no checks (another has): none
/// runs, and the record says why.
#[tokio::test]
async fn with_no_checks_in_the_changed_folders_none_runs() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(
        &[
            ("api", &[("check.sh", "exit 0\n")]),
            ("web", &[("b.txt", "b\n")]),
        ],
        "all",
    )
    .await;
    set_checks(&h, "api", &[("typecheck", "sh check.sh")]);
    let (url, _) = model(vec![write("w1", "web/b.txt", "b2\n"), Reply::Text("Done.")]).await;
    h.start(h.workspace_request(&url, Harness::send("Edit web"), checked()))
        .await;
    assert_eq!(h.run_end().await, "unverified");
    let records = verifications(&h.messages().await);
    assert_eq!(
        records[0]["reason"],
        "no checks are set up for the folders it changed"
    );
    assert_eq!(h.file("web/b.txt").as_deref(), Some("b2\n"), "landed");
}

/// A folder's failed type check skips that folder's tests, not another's;
/// its failure names the folder's file as the chat does (tsc printed it
/// relative to the folder).
#[tokio::test]
async fn a_failed_type_check_in_one_folder_doesnt_skip_the_others_tests() {
    if !crate::vfs::mount_works() {
        return;
    }
    let check = todo_check("$f");
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    ("check.sh", &check),
                    ("test.sh", "exit 0\n"),
                    ("src/a.ts", "a\n"),
                ],
            ),
            ("web", &[("test.sh", "exit 0\n"), ("src/b.ts", "b\n")]),
        ],
        "all",
    )
    .await;
    set_checks(
        &h,
        "api",
        &[("typecheck", "sh check.sh"), ("test", "sh test.sh")],
    );
    set_checks(&h, "web", &[("test", "sh test.sh")]);
    let (url, _) = model(vec![
        write("w1", "api/src/a.ts", "a // TODO\n"),
        write("w2", "web/src/b.ts", "b2\n"),
        Reply::Text("Done."),
        write("w3", "api/src/a.ts", "a2\n"),
        Reply::Text("Fixed."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change both"), checked()))
        .await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let records = verifications(&h.messages().await);
    let typecheck = check_in(&records[0], "cd api && sh check.sh");
    assert_eq!(typecheck["status"], "failed");
    assert_eq!(
        typecheck["failures"],
        json!(["typecheck|api/src/a.ts|TS1000|A TODO is left."])
    );
    let api_tests = check_in(&records[0], "cd api && sh test.sh");
    assert_eq!(api_tests["status"], "skipped", "{api_tests:#}");
    assert_eq!(
        check_in(&records[0], "cd web && sh test.sh")["status"],
        "passed"
    );
}

/// Each folder's check fails only as it did before the change: it runs on
/// a copy of the chat's folders under the same names, and its failures are
/// set apart by the chat's paths, whether the check printed them relative
/// to its folder (web) or absolute (api: the view's, then the copy's).
#[tokio::test]
async fn a_check_failing_as_before_the_change_is_set_apart_per_folder() {
    if !crate::vfs::mount_works() {
        return;
    }
    let (absolute, relative) = (todo_check("$PWD/$f"), todo_check("$f"));
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    ("check.sh", &absolute),
                    ("src/old.ts", "// TODO\n"),
                    ("src/a.ts", "a\n"),
                ],
            ),
            (
                "web",
                &[
                    ("check.sh", &relative),
                    ("src/old.ts", "// TODO\n"),
                    ("src/b.ts", "b\n"),
                ],
            ),
        ],
        "all",
    )
    .await;
    for name in ["api", "web"] {
        set_checks(&h, name, &[("typecheck", "sh check.sh")]);
    }
    let (url, _) = model(vec![
        write("w1", "api/src/a.ts", "a2\n"),
        write("w2", "web/src/b.ts", "b2\n"),
        Reply::Text("Done."),
        Reply::Text("The TODOs were there before: I left them."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change both"), checked()))
        .await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let records = verifications(&h.messages().await);
    for name in ["api", "web"] {
        let check = check_in(&records[0], &format!("cd {name} && sh check.sh"));
        assert_eq!(check["status"], "preexisting", "{check:#}");
        assert_eq!(
            check["preexisting"],
            json!([format!(
                "typecheck|{name}/src/old.ts|TS1000|A TODO is left."
            )])
        );
    }
}

/// P3.2: a new error in the file the fix wrote is a slip in it, not a
/// regression to undo, though the check prints it by its folder's real
/// path, whose last part isn't the folder's name (`y/app` is `y-app`).
#[tokio::test]
async fn a_slip_printed_with_a_folders_real_path_is_the_rounds_own() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(
        &[
            ("x/app", &[("src/a.ts", "a\n")]),
            ("y/app", &[("src/b.ts", "b\n")]),
        ],
        "all",
    )
    .await;
    let real = h.project.join("y/app");
    let check = todo_check(&format!("{}/$f", real.display()));
    std::fs::write(real.join("check.sh"), check).unwrap();
    set_checks(&h, "y/app", &[("typecheck", "sh check.sh")]);
    let (url, _) = model(vec![
        write("w1", "y-app/src/b.ts", "b // TODO\n"),
        Reply::Text("Done."),
        write("w2", "y-app/src/b.ts", "b // FIXME\n"),
        Reply::Text("Fixed."),
        write("w3", "y-app/src/b.ts", "b2\n"),
        Reply::Text("Fixed again."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Change b"), checked()))
        .await;
    assert_eq!(h.run_end_within(Duration::from_secs(60)).await, "verified");
    let records = verifications(&h.messages().await);
    let statuses: Vec<&Value> = records.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("failed"), &json!("passed")],
        "{records:#?}"
    );
    assert_eq!(records[1]["rolledBack"], Value::Null, "{:#}", records[1]);
    assert_eq!(
        records[1]["checks"][0]["failures"],
        json!(["typecheck|y-app/src/b.ts|TS2000|A FIXME is left."])
    );
    assert_eq!(h.file("y/app/src/b.ts").as_deref(), Some("b2\n"));
}

/// E04: the test writer's new tests run on a copy of the chat's folders as
/// they were, each in its folder (`cd api && go test ./a`): one that passes
/// there already is flagged, one that fails there isn't.
#[tokio::test]
async fn the_red_check_runs_a_new_test_in_its_own_folder() {
    if !crate::vfs::mount_works() {
        return;
    }
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_err()
    {
        return;
    }
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    ("go.mod", "module api\n\ngo 1.21\n"),
                    ("a/a.go", "package a\n\nfunc A() int { return 1 }\n"),
                ],
            ),
            (
                "web",
                &[
                    ("go.mod", "module web\n\ngo 1.21\n"),
                    ("b/b.go", "package b\n\nfunc B() int { return 1 }\n"),
                ],
            ),
        ],
        "all",
    )
    .await;
    let (url, _) = model_routed(
        vec![(
            "# Write tests for this change",
            vec![
                write("t1", "api/a/a_test.go", "package a\n\nimport \"testing\"\n\nfunc TestA(t *testing.T) { if A() != 1 { t.Fatal(A()) } }\n"),
                write("t2", "web/b/b_test.go", "package b\n\nimport \"testing\"\n\nfunc TestB(t *testing.T) { if B() != 2 { t.Fatal(B()) } }\n"),
                Reply::Text("## Summary\nTests.\n\n## Test files\n- `api/a/a_test.go`\n- `web/b/b_test.go`\n\n## Assumptions\nNone"),
            ],
        )],
        vec![
            write("w1", "web/b/b.go", "package b\n\nfunc B() int { return 2 }\n"),
            Reply::Call {
                id: "tw",
                name: "task",
                args: json!({ "kind": "testWriter" }),
            },
            Reply::Text("Done."),
        ],
    )
    .await;
    let config: subagent::SubagentConfig = serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "testWriter": { "enabled": true } },
        "redCheck": true,
    }))
    .unwrap();
    let mut request = h.workspace_request(
        &url,
        Harness::send("Make B return 2"),
        Permissions {
            allow_any_command: true,
            checklist: ChecklistMode::Off,
            ..quiet_permissions()
        },
    );
    request.config.as_mut().unwrap().subagents = config;
    h.start(request).await;
    // `go test` builds Go's standard library first on a fresh machine.
    h.run_end_within(Duration::from_secs(120)).await;
    let r = result_of(&h.messages().await, "tw").content.clone();
    assert!(
        r.contains("Red check: `api/a/a_test.go` already passes on the code before the change"),
        "{r}"
    );
    assert!(!r.contains("`web/b/b_test.go` already"), "{r}");
}

/*
 * Plans (4.8, D28)
 */

/// A plan of SHOP's two folders, its files named by their folders.
const SHOP_PLAN: &str = "# Change both\n\nBoth constants become 2.\n\n## Steps\n\
    1. **API**\n   - Files: `api/src/a.ts`\n   - Change `a` in `api/src/a.ts` to 2.\n\
    2. **Web**\n   - Files: `web/src/b.ts`\n   - Change `b` in `web/src/b.ts` to 2.\n\n\
    ## Validation\n- `npm test`\n";

/// The plan file a `submit_plan` call saved.
fn plan_file(messages: &[MessageRow], call: &str) -> String {
    let meta = result_of(messages, call).meta.as_ref().expect("its meta");
    meta["planFile"].as_str().expect("a plan file").to_string()
}

/// A plan run of SHOP's chat that saves `SHOP_PLAN`: its file.
async fn planned(h: &Harness) -> String {
    let (url, _) = model(vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": SHOP_PLAN }),
        },
        Reply::Text("Saved."),
    ])
    .await;
    h.start(h.workspace_request(&url, plan_send("Plan both"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    plan_file(&h.messages().await, "p1")
}

/// Its chat path: `api/.rovik/plans/<its name>`.
fn in_api(file: &str) -> String {
    let name = std::path::Path::new(file).file_name().unwrap();
    format!("api/.rovik/plans/{}", name.to_string_lossy())
}

#[tokio::test]
async fn a_workspace_chats_plan_is_saved_in_its_first_folder_under_a_line_naming_its_folders() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model(vec![
        // A file named as a folder chat names it is in none of the folders.
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": SHOP_PLAN.replace("`api/src/a.ts`", "`src/a.ts`") }),
        },
        Reply::Call {
            id: "p2",
            name: "submit_plan",
            args: json!({ "plan": SHOP_PLAN }),
        },
        Reply::Text("Saved."),
    ])
    .await;
    h.start(h.workspace_request(&url, plan_send("Plan both"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let refused = &result_of(&messages, "p1").content;
    assert!(
        refused.starts_with("Error: the plan wasn't saved.") && refused.contains("`src/a.ts`"),
        "{refused}"
    );
    assert!(!refused.contains("`web/src/b.ts`"), "{refused}");
    let file = plan_file(&messages, "p2");
    let saved = &result_of(&messages, "p2").content;
    assert!(
        saved.starts_with(&format!("Saved the plan to {}.", in_api(&file))),
        "{saved}"
    );
    assert_eq!(
        std::path::Path::new(&file).parent(),
        Some(h.project.join("api/.rovik/plans").as_path())
    );
    assert!(!h.project.join("web/.rovik").exists());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("# Change both\n\nFolders: api, web\n\nBoth constants become 2.\n"),
        "{text}"
    );
}

#[tokio::test]
async fn the_plan_mode_line_and_the_implementation_name_a_workspace_plan_by_its_chat_path() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let file = planned(&h).await;
    let shown = in_api(&file);

    // Planning again: the current plan, by its chat path.
    let (url, seen) = model(vec![Reply::Text("It holds.")]).await;
    h.events.lock().unwrap().clear();
    h.start(h.workspace_request(&url, plan_send("Look again"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let sent = seen.lock().unwrap()[0].to_string();
    assert!(sent.contains(&format!("Saved in {shown};")), "{sent}");
    assert!(!sent.contains(&file), "{sent}");

    // Implemented as the app sends it: the request names it so, and its
    // files are checked in the folders by their names.
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("`web/src/b.ts`", "`web/src/gone.ts`")).unwrap();
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({ "items": [{ "title": "API", "status": "done" }] }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: format!("Implement the plan in `{shown}`."),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true, "planPath": file })),
        attachments: Vec::new(),
    };
    h.events.lock().unwrap().clear();
    h.start(h.workspace_request(&url, StartKind::Send { message }, quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let first = seen.lock().unwrap()[0]["messages"].to_string();
    assert!(
        first.contains(&format!("It's saved in {shown};")),
        "{first}"
    );
    let messages = h.messages().await;
    let note = messages
        .iter()
        .find_map(|m| m.meta.as_ref().filter(|m| m["harness"] == "planCheck"))
        .expect("a plan check note");
    assert_eq!(note["planCheck"]["file"], json!(shown));
    let problems = note["planCheck"]["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0]
            .as_str()
            .unwrap()
            .starts_with("Step 2 names `web/src/gone.ts`"),
        "{problems:?}"
    );
}

/// A plan of web alone.
const WEB_PLAN: &str = "# Web only\n\nThe constant becomes 2.\n\n## Steps\n\
    1. **Web**\n   - Files: `web/src/b.ts`\n   - Change `b` in `web/src/b.ts` to 2.\n\
    2. **Check**\n   - Files: `web/src/b.ts` (read)\n   - Read `b` in `web/src/b.ts` to confirm it is 2.\n\n\
    ## Validation\n- `npm test`\n";

#[tokio::test]
async fn after_its_first_folder_is_unchecked_a_chats_next_plan_goes_to_its_new_first_folder() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let first = planned(&h).await;
    let id = h.dialog_id.clone();
    h.core
        .db
        .write(move |c| store::set_dialog_folders(c, &id, Some(r#"["web"]"#)))
        .await
        .unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "p2",
            name: "submit_plan",
            args: json!({ "plan": WEB_PLAN }),
        },
        Reply::Text("Saved."),
    ])
    .await;
    h.events.lock().unwrap().clear();
    h.start(h.workspace_request(&url, plan_send("Only web now"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let file = plan_file(&messages, "p2");
    assert_eq!(
        std::path::Path::new(&file).parent(),
        Some(h.project.join("web/.rovik/plans").as_path())
    );
    let saved = &result_of(&messages, "p2").content;
    assert!(
        saved.starts_with("Saved the plan to web/.rovik/plans/"),
        "{saved}"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# Web only\n\nFolders: web\n\n"), "{text}");
    // The earlier plan stays where it was written.
    let earlier = std::fs::read_to_string(&first).unwrap();
    assert!(
        earlier.starts_with("# Change both\n\nFolders: api, web\n\n"),
        "{earlier}"
    );
}

/// What an Autopilot task's plan says done means (S01), for SHOP.
const SHOP_TASK_PLAN: &str = "# Change both\n\nBoth constants become 2.\n\n## Criteria\n\n\
    1. `api/src/a.ts` exports `a` as 2, and `web/src/b.ts` exports `b` as 2.\n\n\
    **Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
    1. **API**\n   - Files: `api/src/a.ts`\n   - Change `a` in `api/src/a.ts` to 2.\n\
    2. **Web**\n   - Files: `web/src/b.ts`\n   - Change `b` in `web/src/b.ts` to 2.\n\n\
    ## Validation\n- `npm test`\n";

#[tokio::test]
async fn autopilots_report_and_implementation_name_a_workspace_plan_by_its_chat_path() {
    if !crate::vfs::mount_works() {
        return;
    }
    use super::autopilot::Stage;
    use super::autopilot_tests::{
        autopilot_send, implement_messages, model_routed, quiet, stages_until, PLAN_REVIEW,
    };
    let h = workspace_harness(SHOP, "all").await;
    let (url, _) = model_routed(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        vec![
            Reply::Call {
                id: "p1",
                name: "submit_plan",
                args: json!({ "plan": SHOP_TASK_PLAN }),
            },
            Reply::Text("Saved the plan."),
            // The implementation: stopped once it has its request.
            Reply::Hang(None),
        ],
    )
    .await;
    let mut request =
        h.workspace_request(&url, autopilot_send("Change both"), Permissions::default());
    request.mode = Some("autopilot".into());
    h.start(request).await;
    let records = stages_until(&h, "the implementation", |records| {
        records.iter().any(|r| r.name == Stage::Implement)
    })
    .await;
    let messages = within("the implementation's request", async {
        loop {
            let messages = h.messages().await;
            if !implement_messages(&messages).is_empty() {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    agent_stop(&h.core, h.dialog_id.clone()).await;
    quiet(&h).await;
    let file = plan_file(&messages, "p1");
    let shown = in_api(&file);
    assert_eq!(
        records[0].summary.as_deref(),
        Some(format!("Saved the plan to {shown}").as_str())
    );
    let request = &implement_messages(&messages)[0].content;
    assert!(
        request.contains(&format!("It's saved in {shown};")),
        "{request}"
    );
}

/*
 * Subagents, Thorough, Autopilot, diagnostics (4.9, 4.10)
 */

fn read(id: &'static str, path: &str) -> Reply {
    Reply::Call {
        id,
        name: "read",
        args: json!({ "path": path }),
    }
}

/// A plan of a step in each of SHOP's folders, for workers.
const SHOP_STEPS: &str = "# Change both\n\nBoth constants become 2.\n\n## Steps\n\n\
    1. **API**\n   - Files: `api/src/a.ts`\n   - Change `a` in `api/src/a.ts` to 2.\n\
    2. **Web**\n   - Files: `web/src/b.ts`\n   - Change `b` in `web/src/b.ts` to 2.\n\n\
    ## Validation\n- read both files\n";

/// 4.9: a workspace chat's subagents work in its view: a scout reads both
/// folders by their names, and a step's worker writes its step's file, in
/// its folder, and none of the other folder's.
#[tokio::test]
async fn a_scout_reads_both_folders_and_a_worker_writes_only_its_steps_folder() {
    if !crate::vfs::mount_works() {
        return;
    }
    use super::subagent_tests::{step, task, worker_report, REPORT};
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    ("src/a.ts", "export const a = 1;\n"),
                    (".rovik/plans/shop.md", SHOP_STEPS),
                ],
            ),
            ("web", &[("src/b.ts", "export const b = 1;\n")]),
        ],
        "all",
    )
    .await;
    let checklist = |api: &str, web: &str| Reply::Call {
        id: "t0",
        name: "todo",
        args: json!({ "items": [
            { "title": "API", "status": api },
            { "title": "Web", "status": web },
        ] }),
    };
    let (url, _) = model_routed(
        vec![
            (
                "MARK-SHOP",
                vec![
                    read("r1", "api/src/a.ts"),
                    read("r2", "web/src/b.ts"),
                    Reply::Text(REPORT),
                ],
            ),
            (
                "# Your step: 1.",
                vec![
                    write("w1", "api/src/a.ts", "export const a = 2;\n"),
                    write("w2", "web/src/b.ts", "export const b = 9;\n"),
                    worker_report("Changed a.", &["api/src/a.ts"]),
                ],
            ),
        ],
        vec![
            checklist("pending", "pending"),
            task("t1", "MARK-SHOP: where are a and b?"),
            step("t2", 1),
            checklist("done", "skipped"),
            Reply::Text("Done."),
        ],
    )
    .await;
    let file = h.project.join("api/.rovik/plans/shop.md");
    let message = UserMessage {
        id: uuid::Uuid::new_v4().to_string(),
        content: "Implement the plan in `api/.rovik/plans/shop.md`.".into(),
        meta: Some(json!({ "mode": "agent", "implementsPlan": true, "planPath": file })),
        attachments: Vec::new(),
    };
    let mut request = h.workspace_request(&url, StartKind::Send { message }, any_command());
    request.config.as_mut().unwrap().subagents = serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "scout": { "enabled": true }, "worker": { "enabled": true } },
        "workerMinSteps": 2,
    }))
    .unwrap();
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let scout = h.child_of("t1").await;
    let seen = h.messages_of(&scout.id).await;
    for (call, text) in [("r1", "export const a = 1;"), ("r2", "export const b = 1;")] {
        let read = &result_of(&seen, call).content;
        assert!(read.contains(text), "{read}");
    }
    let worker = h.child_of("t2").await;
    let wrote = h.messages_of(&worker.id).await;
    let own = &result_of(&wrote, "w1").content;
    assert!(!own.starts_with("Error"), "{own}");
    let other = &result_of(&wrote, "w2").content;
    assert!(
        other.contains("`web/src/b.ts` is outside your write scope"),
        "{other}"
    );
    assert_eq!(
        h.file("api/src/a.ts").as_deref(),
        Some("export const a = 2;\n")
    );
    assert_eq!(
        h.file("web/src/b.ts").as_deref(),
        Some("export const b = 1;\n")
    );
}

/// What api's check needs: its ok.txt saying ok.
const OK_CHECK: &str =
    "if [ \"$(cat ok.txt)\" = ok ]; then exit 0; fi\necho 'ok.txt:1: error: not ok'\nexit 1\n";

/// An Autopilot task's plan for api's check, its files named by their
/// folders, and what done means (S01).
const API_TASK_PLAN: &str = "# Make the check pass\n\napi's check passes once api/ok.txt says ok.\n\n## Criteria\n\n\
    1. `cd api && sh check.sh` passes: `api/check.sh` finds `api/ok.txt` saying ok.\n\n\
    **Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `api/ok.txt`\n   - Replace the text of `api/ok.txt` with `ok`, which `api/check.sh` checks.\n\
    2. **Check**\n   - Files: `api/check.sh` (read)\n   - Read `api/check.sh` to confirm it checks `api/ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `cd api && sh check.sh`\n";

/// A workspace for a task: api's check (`sh check.sh`, on) passes once
/// its ok.txt says ok; web has instructions of its own.
async fn task_shop() -> Harness {
    let h = workspace_harness(
        &[
            ("api", &[("check.sh", OK_CHECK), ("ok.txt", "no\n")]),
            (
                "web",
                &[
                    ("AGENTS.md", "MARK-WEB: web keeps its own rules.\n"),
                    ("src/b.ts", "export const b = 1;\n"),
                ],
            ),
        ],
        "all",
    )
    .await;
    set_checks(&h, "api", &[("test", "sh check.sh")]);
    h
}

/// A task's start in the workspace chat, in Autopilot, the plan reviewer on.
fn shop_task(h: &Harness, url: &str) -> StartRequest {
    let send = super::autopilot_tests::autopilot_send("Make the check pass");
    let mut request = h.workspace_request(url, send, Permissions::default());
    request.mode = Some("autopilot".into());
    request.config.as_mut().unwrap().subagents = serde_json::from_value(json!({
        "enabled": true,
        "kinds": { "planReviewer": { "enabled": true } },
        "reviewMinLines": 1,
    }))
    .unwrap();
    request
}

fn submit_task_plan() -> Reply {
    Reply::Call {
        id: "p1",
        name: "submit_plan",
        args: json!({ "plan": API_TASK_PLAN }),
    }
}

/// 4.9: an Autopilot task in a workspace chat runs to its delivery, its
/// check run in its folder and its change landed there. Cut between two
/// stages and its view closed, as a restart leaves it, it gets the chat's
/// folders again from the chat when it goes on: nothing else keeps them.
#[tokio::test]
async fn an_autopilot_task_in_a_workspace_chat_runs_to_its_delivery_and_gets_its_folders_again_after_a_restart(
) {
    if !crate::vfs::mount_works() {
        return;
    }
    use super::autopilot::{crash_at, Crash, Status};
    use super::autopilot_tests::{
        candidate_script, delivered, model_routed as task_model, names, stages_until, BALANCED_OK,
        PLAN_REVIEW,
    };
    let h = task_shop().await;
    let mut script = vec![submit_task_plan(), Reply::Text("Saved the plan.")];
    script.extend(candidate_script(&[("api/ok.txt", "ok\n")]));
    let (url, _) = task_model(
        vec![(PLAN_REVIEW, vec![Reply::Text("## Findings\n\nNone.")])],
        script,
    )
    .await;
    crash_at(&h.dialog_id, Crash::AfterEnd);
    h.start(shop_task(&h, &url)).await;
    stages_until(&h, "the plan's end", |r| {
        r.first().is_some_and(|p| p.status == Status::Done)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.core.agents.is_running(&h.dialog_id));
    // The restart: the view is unmounted, its changes kept on disk.
    let id = h.dialog_id.clone();
    crate::rt::spawn_blocking(move || crate::overlay::chats::close(&id))
        .await
        .unwrap();
    assert!(crate::overlay::chats::get(&h.dialog_id).is_none());
    let mut resume = h.workspace_request(&url, StartKind::Resume, Permissions::default());
    resume.mode = Some("autopilot".into());
    resume.config = None;
    h.start(resume).await;
    let records = stages_until(&h, "the delivery", delivered).await;
    assert_eq!(names(&records), BALANCED_OK, "{records:#?}");
    assert_eq!(
        records.last().unwrap().summary.as_deref(),
        Some("Delivered: the changes are in the project")
    );
    assert_eq!(h.file("api/ok.txt").as_deref(), Some("ok\n"));
    let record = verifications(&h.messages().await)
        .pop()
        .expect("the checks' record");
    assert_eq!(
        check_in(&record, "cd api && sh check.sh")["status"],
        "passed"
    );
}

/// T05 (4.9): a Thorough task in a workspace chat has two candidates,
/// each in a copy of the whole view, told every folder's instructions and
/// checked by api's own check; the smaller change lands in its folder, and
/// the other's file in web never does.
#[tokio::test]
async fn a_thorough_task_makes_candidates_over_the_whole_view_each_following_every_folders_settings(
) {
    if !crate::vfs::mount_works() {
        return;
    }
    use super::autopilot::{Effort, Stage};
    use super::autopilot_tests::{
        candidate_script, candidates_row, chat_messages, delivered_slowly,
        model_routed as task_model, CHANGE_REVIEW, PLAN_REVIEW,
    };
    let h = task_shop().await;
    let mut script = vec![submit_task_plan(), Reply::Text("Saved the plan.")];
    script.extend(candidate_script(&[("api/ok.txt", "ok\n")]));
    script.extend(candidate_script(&[
        ("api/ok.txt", "ok\n"),
        ("web/notes.txt", "Wrote ok.\n"),
    ]));
    let clean = || vec![Reply::Text("## Findings\n\nNone.")];
    let (url, seen) = task_model(
        vec![(PLAN_REVIEW, clean()), (CHANGE_REVIEW, clean())],
        script,
    )
    .await;
    let mut request = shop_task(&h, &url);
    request.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    h.start(request).await;
    let records = delivered_slowly(&h).await;
    let implement = records.iter().find(|r| r.name == Stage::Implement).unwrap();
    assert_eq!(implement.candidates, ["a", "b"], "{records:#?}");
    assert_eq!(implement.summary.as_deref(), Some("Candidate a's, picked"));
    assert_eq!(h.file("api/ok.txt").as_deref(), Some("ok\n"));
    assert_eq!(h.file("web/notes.txt"), None);
    // Each candidate's check was api's, run in it, and passed.
    let messages = h.messages().await;
    let row = candidates_row(&messages);
    for candidate in row["candidates"].as_array().unwrap() {
        let chat = candidate["chat"].as_str().unwrap();
        let record = verifications(&chat_messages(&h, chat).await)
            .pop()
            .expect("its checks' record");
        assert_eq!(
            check_in(&record, "cd api && sh check.sh")["status"],
            "passed"
        );
    }
    // Each implementer was told web's instructions too.
    let requests = seen.lock().unwrap().clone();
    let implementing: Vec<&Value> = requests
        .iter()
        .filter(|r| {
            r["messages"]
                .to_string()
                .contains("Implement this plan. It's saved in")
        })
        .filter(|r| r["tools"].to_string().contains("\"write\""))
        .collect();
    assert!(implementing.len() >= 2, "{}", implementing.len());
    for request in implementing {
        let system = request["messages"][0]["content"].as_str().unwrap_or("");
        assert!(system.contains("MARK-WEB"), "{system}");
        // Its plan named as the chat names it, which its copy has the same.
        let asked = request["messages"].to_string();
        assert!(asked.contains("It's saved in api/.rovik/plans/"), "{asked}");
    }
}

/// An editor marker on line 1, an error saying `message`.
fn marker(message: &str) -> crate::agent_tools::EditorMarker {
    crate::agent_tools::EditorMarker {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 2,
        severity: "error".into(),
        message: message.into(),
        source: None,
        code: None,
    }
}

/// 4.10: the editor's markers, reported by each folder, are found through
/// the view by the folder of the file, for the project's version only: a
/// file the chat changed gets none, in its diagnostics and the summary.
#[tokio::test]
async fn the_diagnostics_tool_finds_the_editors_markers_through_the_view_and_none_for_a_changed_file(
) {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    for (folder, file, message) in [
        ("api", "src/a.ts", "MARK-A is wrong"),
        ("web", "src/b.ts", "MARK-B is wrong"),
    ] {
        let root = h.project.join(folder).to_string_lossy().into_owned();
        crate::agent_tools::report_editor_diagnostics(root, file.into(), vec![marker(message)])
            .unwrap();
    }
    let diagnostics = |id: &'static str, args: Value| Reply::Call {
        id,
        name: "diagnostics",
        args,
    };
    let (url, _) = model(vec![
        diagnostics("d1", json!({ "path": "api/src/a.ts" })),
        write("w1", "web/src/b.ts", "export const b = 2;\n"),
        diagnostics("d2", json!({ "path": "web/src/b.ts" })),
        diagnostics("d3", json!({})),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.workspace_request(&url, Harness::send("Look"), quiet_permissions()))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let result = |id: &str| result_of(&messages, id).content.clone();
    assert!(
        result("d1").contains("error: MARK-A is wrong"),
        "{}",
        result("d1")
    );
    let changed = result("d2");
    assert!(
        !changed.contains("MARK-B") && changed.contains("which this chat has changed"),
        "{changed}"
    );
    let summary = result("d3");
    assert!(summary.contains("- api/src/a.ts: 1 error"), "{summary}");
    assert!(!summary.contains("web/src/b.ts"), "{summary}");
}

/// 4.10, section 12 item 10: an isolated folder chat, which works in its
/// view, finds the editor's markers its folder reported.
#[tokio::test]
async fn an_isolated_folder_chat_finds_the_editors_markers_through_its_view() {
    if !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[("src/a.ts", "export const a = 1;\n")]).await;
    h.isolate().await;
    let root = h.project.to_string_lossy().into_owned();
    crate::agent_tools::report_editor_diagnostics(root, "src/a.ts".into(), vec![marker("MARK-F")])
        .unwrap();
    let (url, _) = model(vec![
        Reply::Call {
            id: "d1",
            name: "diagnostics",
            args: json!({ "path": "src/a.ts" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Look"), quiet_permissions());
    request.isolate = Some(true);
    h.start(request).await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    let found = &result_of(&messages, "d1").content;
    assert!(found.contains("error: MARK-F"), "{found}");
}

/// A plan whose test written first (`api/extra.test.js`) needs a file only
/// one of the candidates writes.
const EXTRA_TASK_PLAN: &str = "# Make the check and the test pass\n\napi's check passes once api/ok.txt says ok, and its test once api/extra.txt says yes.\n\n## Criteria\n\n\
    1. `cd api && sh check.sh` passes: `api/check.sh` finds `api/ok.txt` saying ok.\n\
    2. `api/extra.test.js` passes: `api/extra.txt` says yes.\n\n\
    **Out of scope**: None.\n\n**Assumptions**: None.\n\n## Steps\n\n\
    1. **Write ok**\n   - Files: `api/ok.txt`, `api/extra.txt` (new)\n   - Replace the text of `api/ok.txt` with `ok`, and write `yes` to `api/extra.txt`.\n\
    2. **Check**\n   - Files: `api/check.sh` (read)\n   - Read `api/check.sh` to confirm it checks `api/ok.txt`; it keeps its code.\n\n\
    ## Validation\n- `cd api && sh check.sh`\n";

const EXTRA_TEST: &str = "const test = require('node:test');\nconst assert = require('assert');\nconst fs = require('fs');\ntest('extra.txt says yes', () => assert.strictEqual(fs.readFileSync(__dirname + '/extra.txt', 'utf8').trim(), 'yes'));\n";

/// T06 (4.9): the pick runs each test written first alone in its folder
/// of a candidate's copy (`cd api && node --test extra.test.js`): the
/// candidate whose change makes it pass wins, though the other's change is
/// smaller and its checks pass too.
#[tokio::test]
async fn a_thorough_tasks_pick_runs_each_test_written_first_in_its_folder() {
    if !crate::vfs::mount_works() || !has_npm() {
        return;
    }
    use super::autopilot::{Effort, Stage};
    use super::autopilot_tests::{
        candidate_script, delivered_slowly, model_routed as task_model, CHANGE_REVIEW, PLAN_REVIEW,
    };
    let h = workspace_harness(
        &[
            (
                "api",
                &[
                    (
                        "package.json",
                        r#"{ "name": "api", "scripts": { "test": "node --test" } }"#,
                    ),
                    ("check.sh", OK_CHECK),
                    ("ok.txt", "no\n"),
                ],
            ),
            ("web", &[("src/b.ts", "export const b = 1;\n")]),
        ],
        "all",
    )
    .await;
    set_checks(&h, "api", &[("test", "sh check.sh")]);
    let mut script = vec![
        Reply::Call {
            id: "p1",
            name: "submit_plan",
            args: json!({ "plan": EXTRA_TASK_PLAN }),
        },
        Reply::Text("Saved the plan."),
    ];
    script.extend(candidate_script(&[
        ("api/ok.txt", "ok\n"),
        ("api/extra.txt", "yes\n"),
    ]));
    script.extend(candidate_script(&[("api/ok.txt", "ok\n")]));
    let clean = || vec![Reply::Text("## Findings\n\nNone.")];
    let tests_first = vec![
        write("tw1", "api/extra.test.js", EXTRA_TEST),
        Reply::Text("## Summary\n\nChecks that extra.txt says yes (criterion 2).\n\n## Test files\n\n- `api/extra.test.js`: extra.txt says yes\n\n## Assumptions\n\nNone."),
    ];
    let (url, _) = task_model(
        vec![
            (PLAN_REVIEW, clean()),
            (CHANGE_REVIEW, clean()),
            (super::test_writer::FIRST_HEADING, tests_first),
        ],
        script,
    )
    .await;
    let mut request = shop_task(&h, &url);
    request.config.as_mut().unwrap().autopilot.effort = Effort::Thorough;
    h.start(request).await;
    let records = delivered_slowly(&h).await;
    let implement = records.iter().find(|r| r.name == Stage::Implement).unwrap();
    assert_eq!(
        implement.summary.as_deref(),
        Some("Candidate a's, picked"),
        "{records:#?}"
    );
    let messages = h.messages().await;
    let pick = messages
        .iter()
        .find_map(|m| m.meta.as_ref()?.get("pick"))
        .expect("the pick's record");
    let tests = |i: usize| pick["candidates"][i]["score"]["testsPassed"].clone();
    assert_eq!((tests(0), tests(1)), (json!(1), json!(0)), "{pick}");
    assert_eq!(h.file("api/extra.txt").as_deref(), Some("yes\n"));
}

/*
 * Connectors (4.10)
 */

/// The words of a workspace chat's section that say what its paths are.
const FOLDERS_SAID: &str = "every path starts with one (`api/src/main.rs`)";

/// 4.10: Claude Code with its own tools works in a workspace chat's view
/// (its change there lands in its folder), is told the chat's section, as
/// the view's top has no instructions of its own, and keeps its session:
/// the view's path is the chat's own on every run.
#[cfg(unix)]
#[tokio::test]
async fn claude_code_works_in_the_view_keeps_its_session_and_is_told_the_folders() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let program = crate::connectors::fake_claude(
        &h._dir.0,
        r#"
here="$(dirname "$0")"
echo "$@" >> "$here/args.txt"
pwd >> "$here/cwd.txt"
cat > /dev/null
prev=""
for arg in "$@"; do
  if [ "$prev" = "--append-system-prompt-file" ]; then cat "$arg" > "$here/appended.txt"; fi
  prev="$arg"
done
echo 'export const a = 2;' > api/src/a.ts
echo '{"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5","claude_code_version":"2.1.309"}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Done"}},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Done","usage":{"input_tokens":3,"output_tokens":2},"total_cost_usd":0.001,"num_turns":1}'
"#,
    );
    let send = |text: &str| {
        let send = StartKind::Send {
            message: UserMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content: text.into(),
                meta: Some(json!({ "mode": "agent" })),
                attachments: Vec::new(),
            },
        };
        let mut request = h.workspace_request("connector://claude-code", send, any_command());
        request.profile.id = "cc::sonnet".into();
        request.profile.model = "sonnet".into();
        request.profile.connector =
            Some(json!({ "program": program, "permissionMode": "bypassPermissions" }));
        request
    };
    h.start(send("Change a")).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    h.events.lock().unwrap().clear();
    h.start(send("Once more")).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    assert_eq!(
        h.file("api/src/a.ts").as_deref(),
        Some("export const a = 2;\n")
    );
    let read = |name: &str| std::fs::read_to_string(h._dir.0.join(name)).unwrap();
    let view = crate::overlay::chats::mountpoint(&h.dialog_id);
    let cwds = read("cwd.txt");
    let cwds: Vec<&str> = cwds.lines().collect();
    assert_eq!(cwds, [view.to_string_lossy(), view.to_string_lossy()]);
    let args = read("args.txt");
    let args: Vec<&str> = args.lines().collect();
    assert!(args[0].contains("--session-id"), "{}", args[0]);
    assert!(args[1].contains("--resume s-1"), "{}", args[1]);
    let appended = read("appended.txt");
    assert!(appended.contains(FOLDERS_SAID), "{appended}");
}

/// 4.10: Codex with its own tools is given a workspace chat's section as
/// its developer instructions, as the view's top has no AGENTS.md.
#[cfg(unix)]
#[tokio::test]
async fn codex_with_its_own_tools_is_told_the_folders() {
    if !crate::vfs::mount_works() {
        return;
    }
    let h = workspace_harness(SHOP, "all").await;
    let scenario = json!({ "steps": [{ "text": "Sure." }] });
    let Some(program) = crate::connectors::fake_codex(&h._dir.0, &scenario) else {
        eprintln!("no python3 with tomllib: skipped");
        return;
    };
    let mut request = super::codex_tests::codex_send(
        &h,
        &program,
        "agent",
        "Look",
        json!({ "permissionMode": "fullAccess" }),
        quiet_permissions(),
    );
    request.root = None;
    request.isolate = None;
    h.start(request).await.unwrap();
    assert_eq!(h.run_end().await, "done");
    let text = std::fs::read_to_string(h._dir.0.join("requests.jsonl")).unwrap();
    let started: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|r| r["method"] == "thread/start")
        .collect();
    let told = started[0]["params"]["developerInstructions"]
        .as_str()
        .unwrap_or_default();
    assert!(told.contains(FOLDERS_SAID), "{started:#?}");
}

/// A workspace chat on a daemon (4.11): `folders` (each a name and its
/// files) at the top of a worktree, the harness's `project`, which is one of
/// its project's folders, and a chat of that project, as the daemon has it.
/// Its runs are a server's; the overlays could open (a copy of the code
/// before a change), and none should.
async fn daemon_harness(folders: &[(&str, &[(&str, &str)])]) -> Harness {
    let TestCore { core, events, dir } = TestCore::new();
    core.agents.set_on_server();
    let worktree = dir.0.join("work").join("chat");
    for (name, files) in folders {
        for (path, content) in *files {
            let file = worktree.join(name).join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
    }
    let at = worktree.clone();
    let project = core
        .db
        .write(move |c| crate::chat_db::projects::resolve(c, &at).map(|r| r.opened.project.id))
        .await
        .unwrap();
    let dialog_id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: dialog_id.clone(),
        title: "Test".into(),
        created_at: 0,
        updated_at: 0,
        project_id: Some(project),
        folders: None,
    };
    core.db
        .write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    let mut h = Harness {
        core,
        project: worktree,
        events,
        dialog_id,
        _dir: dir,
        _isolated: None,
    };
    h.isolate().await;
    h
}

impl Harness {
    /// A daemon's start of its workspace chat: in its worktree, not
    /// isolated, with the folders `names`.
    fn daemon_request(
        &self,
        url: &str,
        start: StartKind,
        permissions: Permissions,
        names: &[&str],
    ) -> StartRequest {
        StartRequest {
            root: Some(self.project.to_string_lossy().into_owned()),
            isolate: Some(false),
            folders: names
                .iter()
                .map(|name| RunFolder {
                    name: name.to_string(),
                    settings: None,
                })
                .collect(),
            ..self.request(url, start, permissions)
        }
    }
}

/// On a daemon a workspace chat's run works in its worktree, where each of
/// its folders is the top-level folder of its name: no view, none needed.
#[tokio::test]
async fn on_a_daemon_a_workspace_chat_works_in_its_worktree_by_its_folders_names() {
    let h = daemon_harness(SHOP).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "api/src/a.ts" }),
        },
        write("w1", "web/src/b.ts", "export const b = 2;\n"),
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": "cd web && cat src/b.ts" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let names = ["api", "web"];
    h.start(h.daemon_request(&url, Harness::send("Change web"), any_command(), &names))
        .await;
    assert_eq!(h.run_end().await, "done");
    let messages = h.messages().await;
    assert!(result_of(&messages, "r1")
        .content
        .contains("export const a = 1;"));
    assert!(result_of(&messages, "c1")
        .content
        .contains("export const b = 2;"));
    // Written where it works, in its worktree: nothing to land.
    assert_eq!(
        h.file("web/src/b.ts").as_deref(),
        Some("export const b = 2;\n")
    );
    assert!(
        crate::overlay::chats::get(&h.dialog_id).is_none(),
        "no view"
    );
}

/// On a daemon there's no copy of the code before a change (the worktree
/// is the chat's own): a check failing there is the change's, never set
/// apart as failing before it, as a copy of the worktree would have it.
#[tokio::test]
async fn on_a_daemon_a_check_failing_in_the_worktree_is_reported_as_failing() {
    let h = daemon_harness(&[(
        "api",
        &[
            ("check.sh", &todo_check("$f")),
            ("src/old.ts", "// TODO\n"),
            ("src/a.ts", "a\n"),
        ],
    )])
    .await;
    set_checks(&h, "api", &[("typecheck", "sh check.sh")]);
    let (url, _) = model(vec![
        write("w1", "api/src/a.ts", "a2\n"),
        Reply::Text("Done."),
        Reply::Text("The TODO was there before: I left it."),
        Reply::Text("I left it."),
        Reply::Text("I left it."),
    ])
    .await;
    h.start(h.daemon_request(&url, Harness::send("Change api"), checked(), &["api"]))
        .await;
    let outcome = h.run_end_within(Duration::from_secs(60)).await;
    assert_ne!(outcome, "verified");
    let records = verifications(&h.messages().await);
    let check = check_in(&records[0], "cd api && sh check.sh");
    assert_eq!(check["status"], "failed", "{check:#}");
    assert_eq!(check["preexisting"], Value::Null, "{check:#}");
}
