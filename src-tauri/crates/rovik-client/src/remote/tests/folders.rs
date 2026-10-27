//! Chats across a workspace's folders on hosts (workspaces-plan.md, 4.11):
//! they go only to a host that runs them (`features::FOLDERS`), whose
//! refusal says how to update it, and a folder such a chat works in there
//! doesn't leave the workspace, or the chat, while the chat has changes in
//! it there that haven't landed, or its host can't say (D19, D32).

use super::*;
use rovik_core::chat_db::workspaces;
use rovik_protocol::work::{Change, WorkChange};

/// A workspace "Shop" of the folders `api` and `web` under the test's
/// folder, and a chat across both of them: the workspace's ID.
async fn shop(app: &App, chat: &str) -> String {
    let folders: Vec<workspaces::Prepared> = ["api", "web"]
        .iter()
        .map(|name| {
            let path = app.dir.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            workspaces::prepare(&path.to_string_lossy())
        })
        .collect();
    let made = app
        .core
        .db
        .write(move |c| workspaces::create(c, "Shop", &folders));
    let workspace = made.await.unwrap().id;
    app.chat(chat, Some(&workspace), Vec::new()).await;
    workspace
}

/// A host that takes chats but not chats across folders: its Rovik is
/// 0.1.0, and `user` is who the app is there, of the administrators ann
/// and bo.
fn without_folders(user: &str) -> Fake {
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::MIRROR.into()];
    *fake.info.lock().unwrap() = Some(rovik_protocol::host::HostInfo {
        version: "0.1.0".into(),
        user: user.into(),
        install: Some(rovik_protocol::host::Install {
            admins: vec!["ann".into(), "bo".into()],
        }),
        ..Default::default()
    });
    fake
}

#[tokio::test]
async fn a_chat_across_folders_isnt_moved_to_a_host_that_cant_run_it() {
    let app = App::new();
    shop(&app, "chat").await;
    let (remote, _fake) = connected_to(&app, without_folders("bo")).await;

    // An administrator there is offered the upgrade, as for a host too old
    // for chats.
    let preflight = remote.move_preflight("chat", HOST).await.unwrap();
    assert_eq!(
        preflight.blocked.as_deref(),
        Some(
            "test-server runs Rovik 0.1.0, which can't run a chat across folders yet: upgrade \
             it for everyone first."
        )
    );
    assert!(preflight.upgrade);
    let moved = remote.move_chat("chat", HOST, None).await.unwrap_err();
    assert_eq!(moved.code, FailCode::Version);
    assert_eq!(
        moved.message,
        "test-server runs an older Rovik, which can't run a chat across folders yet: upgrade it \
         first."
    );

    // A folder chat moves there all the same.
    let api = project_in(&app, "notes").await;
    app.chat("notes", Some(&api), Vec::new()).await;
    let preflight = remote.move_preflight("notes", HOST).await.unwrap();
    assert_eq!(preflight.blocked, None);
}

#[tokio::test]
async fn a_chat_across_folders_doesnt_start_in_a_wsl_that_cant_run_it() {
    let app = App::new();
    shop(&app, "chat").await;
    let (remote, fake) = connected_to(&app, without_folders("me")).await;
    // On Windows a chat's first start moves it to WSL, this computer's
    // home host.
    remote.set_home(Some(HOST));
    let mut request = start("chat", StartKind::ContinueRun, None);
    request.root = None;

    let refused = remote.start(request).await.unwrap_err();

    assert_eq!(refused.code, FailCode::Version);
    assert_eq!(refused.message, moving::WSL_TOO_OLD);
    assert!(refused.message.contains("Settings → This computer → WSL"));
    assert!(starts(&fake).is_empty());
    assert_eq!(remote.host_of("chat"), None, "it stays here");
    // Its move dialog says the same.
    let preflight = remote.move_preflight("chat", HOST).await.unwrap();
    assert_eq!(preflight.blocked.as_deref(), Some(moving::WSL_TOO_OLD));
    assert!(!preflight.upgrade);
}

/// The workspace's chat across both folders, on `HOST`, whose worktree
/// there has a change in `web` that hasn't landed: the workspace's ID.
async fn changed_in_web(app: &App, fake: &Fake) -> String {
    let workspace = shop(app, "chat").await;
    let change = WorkChange {
        path: "web/b.txt".into(),
        change: Change::Changed,
        from: None,
        merged: false,
    };
    fake.changes
        .lock()
        .unwrap()
        .insert("chat".into(), vec![change]);
    workspace
}

#[tokio::test]
async fn a_folder_a_chat_on_a_host_has_changes_in_doesnt_leave_the_workspace() {
    let app = App::new();
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::MIRROR.into(), features::FOLDERS.into()];
    let workspace = changed_in_web(&app, &fake).await;
    let (remote, _fake) = connected_to(&app, fake).await;
    remote.place("chat", HOST);

    let held = remote.folder_held_there(&workspace, "web", "removed", true);
    assert_eq!(
        held.await.unwrap().as_deref(),
        Some(
            "web can't be removed now: “Chat” has changes in it on test-server that haven't \
             landed (land or discard them)."
        )
    );
    let api = remote.folder_held_there(&workspace, "api", "removed", true);
    assert_eq!(api.await.unwrap(), None, "none there");
    // Located as the same project, its changes there go along.
    let same = remote.folder_held_there(&workspace, "web", "located elsewhere", false);
    assert_eq!(same.await.unwrap(), None);

    // Nor does it leave the chat (D32).
    let left = remote.leaving_held_there("chat", r#"["api"]"#);
    assert_eq!(
        left.await.unwrap().as_deref(),
        Some(
            "“Chat” has changes in web on test-server that aren't landed: land or discard them \
             first."
        )
    );
    let kept = remote.leaving_held_there("chat", r#"["web"]"#);
    assert_eq!(kept.await.unwrap(), None, "api has none there");
    assert_eq!(
        remote.leaving_held_there("chat", "all").await.unwrap(),
        None
    );

    // At work there, its folders stay as they are.
    let running = AgentEvent::RunStarted {
        dialog_id: "chat".into(),
        run_id: "r1".into(),
        steerable: true,
    };
    remote.receive(HOST, event(running)).await;
    let at_work = remote.leaving_held_there("chat", r#"["web"]"#);
    assert_eq!(
        at_work.await.unwrap().as_deref(),
        Some("“Chat” is at work on test-server: its folders can change once it stops.")
    );
    let api = remote.folder_held_there(&workspace, "api", "removed", true);
    assert!(api
        .await
        .unwrap()
        .unwrap()
        .contains("is at work in it on test-server"));
}

#[tokio::test]
async fn a_folder_a_chat_on_a_host_that_cant_say_works_in_doesnt_leave_the_workspace() {
    let app = App::new();
    let workspace = shop(&app, "chat").await;
    let remote = Remote::new(app.core.clone());
    remote.place("chat", "gone");

    let held = remote.folder_held_there(&workspace, "api", "removed", true);
    assert_eq!(
        held.await.unwrap().as_deref(),
        Some(
            "api can't be removed now: “Chat” is on gone, which can't say whether it has \
             changes in it (gone isn't set up)."
        )
    );
    let left = remote.leaving_held_there("chat", r#"["web"]"#);
    assert_eq!(
        left.await.unwrap().as_deref(),
        Some(
            "“Chat” is on gone, which can't say whether it has changes in api (gone isn't set \
             up): try again once it's connected."
        )
    );
    // A chat here is the core's to ask.
    let here = Remote::new(app.core.clone());
    assert_eq!(
        here.leaving_held_there("chat", r#"["web"]"#).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn a_start_across_folders_names_each_with_its_settings_and_its_paths_from_its_name() {
    let app = App::new();
    shop(&app, "chat").await;
    let (api, web) = (app.dir.0.join("api"), app.dir.0.join("web"));
    let checks =
        json!({ "checks": [{ "kind": "test", "command": "cargo test", "enabled": true }] });
    let config = &app.core.paths.config_dir;
    let at = |folder: &std::path::Path| folder.to_string_lossy().into_owned();
    rovik_core::project_profile::save_project_settings(config, at(&api), checks.clone()).unwrap();
    // A host that runs chats across folders, and keeps no projects: no sync.
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::MIRROR.into(), features::FOLDERS.into()];
    let (remote, fake) = connected_to(&app, fake).await;
    remote.place("chat", HOST);
    let context = format!(
        "[Active editor context — file {}, and {}]\n\nfix both",
        api.join("src").join("a.rs").display(),
        web.join("b.ts").display()
    );
    let mut request = start("chat", StartKind::ContinueRun, None);
    request.root = None;
    request.start = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "fix both".into(),
            meta: Some(json!({ "sentContent": context })),
            attachments: Vec::new(),
        },
    };

    remote.start(request).await.unwrap();

    let sent = starts(&fake).remove(0);
    let names: Vec<(&str, Option<&Value>)> = (sent.folders.iter())
        .map(|f| (f.name.as_str(), f.settings.as_ref()))
        .collect();
    assert_eq!(names, [("api", Some(&checks)), ("web", None)]);
    let StartKind::Send { message } = &sent.start else {
        panic!("{:?}", sent.start)
    };
    assert_eq!(
        message.meta.as_ref().unwrap()["sentContent"],
        "[Active editor context — file api/src/a.rs, and web/b.ts]\n\nfix both"
    );
    assert_eq!(sent.root, None);

    // A message to its run while it works: the same.
    let steer = UserMessage {
        id: "s1".into(),
        content: "and this".into(),
        meta: Some(json!({ "sentContent": format!("see {}", web.join("c.ts").display()) })),
        attachments: Vec::new(),
    };
    remote.steer("chat", steer).await.unwrap();
    let steered = fake.requests.lock().unwrap().iter().find_map(|r| match r {
        Request::AgentSteer { message, .. } => Some(message.clone()),
        _ => None,
    });
    assert_eq!(
        steered.unwrap().meta.unwrap()["sentContent"],
        "see web/c.ts"
    );
}
