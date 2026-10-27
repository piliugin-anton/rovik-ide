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
    let folders = check_project(&h.core.db, &h.dialog_id, None)
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
