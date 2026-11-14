//! Memory transport and agent use against the real daemon, with two distinct
//! worktrees and a desktop database. No external provider is contacted.
#![cfg(unix)]
mod common;
use common::app::{App, HOST};
use common::project::{client_as, ended, start, unattended, Setup, CHAT, PROJECT};
use common::{until, OTHER};
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::memory::{self, sync::Page, MemoryState};
use rovik_core::testing::{model, Reply};
use rovik_protocol::{FailCode, Request};
use serde_json::{json, Value};
use std::path::Path;
use verified_code_memory::{Applicability, NewMemory, SharedMemoryStore, Verification};

fn local(app: &App, setup: &Setup) -> (SharedMemoryStore, String) {
    let repo = setup.project.to_string_lossy().into_owned();
    (
        app.test
            .core
            .memory
            .open(&app.test.core.paths, &setup.project)
            .unwrap(),
        repo,
    )
}
fn cloud(setup: &Setup) -> (SharedMemoryStore, String) {
    let paths = rovik_core::Paths::new(
        setup.daemon.data.join("data"),
        setup.daemon.data.join("config"),
    );
    let repo = MemoryState::project_repo(PROJECT);
    (
        SharedMemoryStore::open(memory::store_path(&paths, Path::new(&repo))).unwrap(),
        repo,
    )
}
async fn exchange(app: &App, setup: &Setup) {
    app.remote
        .sync_project_memory(&setup.project)
        .await
        .unwrap();
}

#[tokio::test]
async fn cloud_chats_share_project_memory_and_findings_return_to_the_desktop() {
    let mut setup = Setup::new().await;
    setup.ensure().await;
    let app = App::new(&mut setup).await;
    let (memory, repo) = local(&app, &setup);
    let seed = memory
        .remember(
            NewMemory::project_fact(
                &repo,
                "Zanzibar invariant",
                "The zanzibar boundary must remain serialized.",
            )
            .with_files(["src/main.rs"]),
        )
        .unwrap();
    memory
        .verify(seed.id, "source review confirmed serialization")
        .unwrap();
    app.connect(&setup).await;
    let (url,seen)=model(vec![Reply::Call {id:"m1",name:"memory_remember",args:json!({"kind":"fact","title":"Cloud finding","claim":"The cloud worker keeps a yggdrasil lock.","applies_to":["src/main.rs"],"evidence":"src/main.rs source observation"})},Reply::Text("Done.")]).await;
    let mut request = start(&setup, &url, unattended());
    if let rovik_core::agent_run::StartKind::Send { message } = &mut request.start {
        message.content = "Inspect the Zanzibar invariant".into();
    }
    app.remote.start(request).await.unwrap();
    ended(&setup.client).await;
    let finding = until("cloud finding synchronized to desktop", || {
        memory
            .list(&repo, "")
            .unwrap()
            .into_iter()
            .find(|m| m.title == "Cloud finding")
    })
    .await;
    assert_eq!(finding.verification, Verification::Provisional);
    assert!(seen.lock().unwrap()[0]
        .to_string()
        .contains("The zanzibar boundary must remain serialized."));
    let (there, cloud_repo) = cloud(&setup);
    assert_eq!(there.get(seed.id).unwrap().repo, cloud_repo);
    assert_eq!(
        there.get(finding.id).unwrap().source_episode,
        finding.source_episode
    );

    let dialog = NewDialog {
        id: "chat-2".into(),
        title: "Second chat".into(),
        created_at: 0,
        updated_at: 0,
        project_id: Some(PROJECT.into()),
    };
    app.test
        .core
        .db
        .write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    app.remote.place("chat-2", HOST);
    let (url, seen) = model(vec![
        Reply::Call {
            id: "g1",
            name: "memory_get",
            args: json!({"id":finding.id.to_string()}),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut request = start(&setup, &url, unattended());
    request.dialog_id = "chat-2".into();
    if let rovik_core::agent_run::StartKind::Send { message } = &mut request.start {
        message.id = "u2".into();
        message.content = "Inspect the Zanzibar invariant".into();
    }
    app.remote.start(request).await.unwrap();
    until("second cloud chat ended", || {
        app.test
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "runEnded" && e["dialogId"] == "chat-2")
            .then_some(())
    })
    .await;
    {
        let requests = seen.lock().unwrap();
        assert!(requests[0]
            .to_string()
            .contains("The zanzibar boundary must remain serialized."));
        assert!(requests[1]
            .to_string()
            .contains("The cloud worker keeps a yggdrasil lock."));
    }
    memory
        .edit(
            seed.id,
            "Zanzibar invariant",
            "Use a per-project zanzibar lock.",
        )
        .unwrap();
    exchange(&app, &setup).await;
    let edited = there.get(seed.id).unwrap();
    assert_eq!(edited.content, "Use a per-project zanzibar lock.");
    assert_eq!(edited.applicability, Applicability::NeedsRevalidation);
    assert!(!edited.evidence_matches_claim());
    memory.delete(finding.id).unwrap();
    exchange(&app, &setup).await;
    assert!(there.get(finding.id).is_err());
}

#[tokio::test]
async fn clear_survives_host_disconnect_and_a_replaced_database_gets_the_full_history() {
    let mut setup = Setup::new().await;
    setup.ensure().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let (memory, repo) = local(&app, &setup);
    let old = memory
        .remember(NewMemory::project_fact(
            &repo,
            "Old fact",
            "History before clear",
        ))
        .unwrap();
    exchange(&app, &setup).await;
    let (_, cloud_repo) = cloud(&setup);
    setup.daemon.kill().await;
    memory.clear(&repo).unwrap();
    let fresh = memory
        .remember(NewMemory::project_fact(
            &repo,
            "New fact",
            "History after clear",
        ))
        .unwrap();
    setup.daemon.restart().await;
    app.connect(&setup).await;
    exchange(&app, &setup).await;
    let (there, _) = cloud(&setup);
    assert!(there.get(old.id).is_err());
    assert!(there.get(fresh.id).is_ok());
    let old_replica = there.sync_replica().unwrap();
    drop(there);
    setup.daemon.kill().await;
    let paths = rovik_core::Paths::new(
        setup.daemon.data.join("data"),
        setup.daemon.data.join("config"),
    );
    let file = memory::store_path(&paths, Path::new(&cloud_repo));
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{}{suffix}", file.display());
        if Path::new(&path).exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    setup.daemon.restart().await;
    app.connect(&setup).await;
    exchange(&app, &setup).await;
    let (there, _) = cloud(&setup);
    assert_ne!(there.sync_replica().unwrap(), old_replica);
    assert!(there.get(old.id).is_err());
    assert_eq!(there.get(fresh.id).unwrap().content, fresh.content);
    assert_eq!(there.list(&cloud_repo, "").unwrap().len(), 1);
}

#[tokio::test]
async fn another_computer_cannot_read_or_write_project_memory() {
    let setup = Setup::new().await;
    setup.ensure().await;
    let client = client_as(&setup.daemon, OTHER, &setup.daemon.other_token).await;
    let fail = client
        .request::<Value>(Request::MemorySync {
            project: PROJECT.into(),
            replica: None,
            after: 0,
            changes: None,
        })
        .await
        .unwrap_err();
    assert_eq!(fail.code, FailCode::NotOwner);
    let fail = setup
        .client
        .request::<Value>(Request::MemorySync {
            project: "unknown-project".into(),
            replica: None,
            after: 0,
            changes: None,
        })
        .await
        .unwrap_err();
    assert_eq!(fail.code, FailCode::BadRequest);
}

#[tokio::test]
async fn old_cloud_worktree_histories_migrate_into_the_shared_project_store() {
    let setup = Setup::new().await;
    setup.ensure().await;
    let snapshot = setup.pushed(CHAT).await;
    let one = setup.sync(CHAT, &snapshot).await.unwrap().root;
    let snapshot = setup.pushed("chat-2").await;
    let two = setup.sync("chat-2", &snapshot).await.unwrap().root;
    let paths = rovik_core::Paths::new(
        setup.daemon.data.join("data"),
        setup.daemon.data.join("config"),
    );
    let mut ids = Vec::new();
    for (root, title) in [(&one, "First history"), (&two, "Second history")] {
        let path = memory::store_path(&paths, Path::new(root));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let store = SharedMemoryStore::open(path).unwrap();
        ids.push(
            store
                .remember(NewMemory::project_fact(
                    root,
                    title,
                    "Existing cloud observation",
                ))
                .unwrap()
                .id,
        );
    }
    let value = setup
        .client
        .request::<Value>(Request::MemorySync {
            project: PROJECT.into(),
            replica: None,
            after: 0,
            changes: None,
        })
        .await
        .unwrap();
    let page: Page = serde_json::from_value(value).unwrap();
    for id in ids {
        assert!(page.changes.iter().any(|c| c.id == id));
    }
    assert_eq!(page.changes.len(), 2);
}

#[tokio::test]
async fn cloud_memory_still_obeys_the_worktrees_enabled_setting() {
    let mut setup = Setup::new().await;
    setup.ensure().await;
    let app = App::new(&mut setup).await;
    let (memory, repo) = local(&app, &setup);
    let record = memory
        .remember(NewMemory::project_fact(
            &repo,
            "Zanzibar invariant",
            "A disabled memory must not enter the model context.",
        ))
        .unwrap();
    memory.verify(record.id, "source review passed").unwrap();
    rovik_core::project_profile::save_project_settings(
        &app.test.core.paths.config_dir,
        repo.clone(),
        json!({"memory":false}),
    )
    .unwrap();
    app.connect(&setup).await;
    let (url, seen) = model(vec![Reply::Text("Done.")]).await;
    let mut request = start(&setup, &url, unattended());
    if let rovik_core::agent_run::StartKind::Send { message } = &mut request.start {
        message.content = "Inspect the Zanzibar invariant".into();
    }
    app.remote.start(request).await.unwrap();
    ended(&setup.client).await;
    let requests = seen.lock().unwrap();
    assert!(!requests[0]
        .to_string()
        .contains("A disabled memory must not enter the model context."));
    assert!(!requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "memory_remember"));
    assert_eq!(memory.list(&repo, "").unwrap().len(), 1);
}

#[tokio::test]
async fn remembered_hosts_still_receive_changes_without_any_loaded_remote_chats() {
    use rovik_client::{HostState, Identity, Remote, UnixConnector};
    use rovik_protocol::Secret;
    let mut setup = Setup::new().await;
    setup.ensure().await;
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let (memory, repo) = local(&app, &setup);
    let record = memory
        .remember(NewMemory::project_fact(
            &repo,
            "Queue",
            "Before desktop restart",
        ))
        .unwrap();
    exchange(&app, &setup).await;
    let restarted = Remote::new(app.test.core.clone());
    // No session caches or remote chats: only the durable acknowledgement
    // table records that this connected host holds a project replica.
    restarted.connect(
        HOST,
        UnixConnector(setup.daemon.socket.clone()),
        Identity {
            host: HOST.into(),
            computer: common::COMPUTER.into(),
            token: Secret(setup.daemon.token.clone()),
            client: "memory restart test".into(),
        },
    );
    until("restarted desktop connection", || {
        restarted
            .states()
            .iter()
            .any(|(_, state)| *state == HostState::Ready)
            .then_some(())
    })
    .await;
    memory
        .edit(record.id, "Queue", "After desktop restart")
        .unwrap();
    restarted.sync_project_memory(&setup.project).await.unwrap();
    assert_eq!(
        cloud(&setup).0.get(record.id).unwrap().content,
        "After desktop restart"
    );
}

#[tokio::test]
async fn an_unavailable_memory_folder_does_not_block_a_cloud_start() {
    let mut setup = Setup::new().await;
    setup.ensure().await;
    // A real I/O failure at the optional store: creating memory databases
    // cannot succeed while a regular file occupies their directory.
    std::fs::write(setup.daemon.data.join("data/memory"), "unavailable").unwrap();
    let app = App::new(&mut setup).await;
    app.connect(&setup).await;
    let (url, seen) = model(vec![Reply::Text("Done.")]).await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();
    ended(&setup.client).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(setup
        .daemon
        .rows(CHAT)
        .iter()
        .any(|row| row.role == "assistant" && row.content == "Done."));
    assert!(app.test.events.lock().unwrap().iter().any(|event| {
        event["type"] == "notice"
            && event["message"].as_str().is_some_and(|message| {
                message.contains("Shared project memory couldn't be prepared")
            })
    }));
}
