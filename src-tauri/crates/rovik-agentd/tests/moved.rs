//! A chat moves with its overlay (T09, 6.4): its changes, folded into its
//! snapshot, are its worktree's own changes on the server, and after a run
//! there and the import, its result lands as the overlay would have,
//! merged with a file the user edited after the chat did.

mod common;

use common::app::{landed_as, App};
use common::project::{read, start, unattended, Setup, CHAT, PROJECT};
use common::{commit_all, write};
use git2::{Repository, Status, StatusOptions};
use rovik_client::shadow::Shadow;
use rovik_core::overlay::chats;
use rovik_core::overlay::fs::{OverlayFs, DEFAULT_LIMIT};
use rovik_core::testing::{model, Reply};
use rovik_core::vfs::backend::Backend;
use rovik_core::vfs::store::{Kind, RenameMode, SetAttr, ROOT};
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::work::Synced;
use rovik_protocol::Request;
use std::path::PathBuf;

/// Writes a file in the overlay: the chat's own copy of it.
fn put(fs: &OverlayFs, path: &str, content: &str) {
    let ino = fs.resolve(path).unwrap_or_else(|_| {
        let (parent, name) = fs.resolve_parent(path).unwrap();
        let made = fs.create(parent, name, Kind::File, 0o644, 0, 0, None);
        made.unwrap().ino
    });
    let empty = SetAttr {
        size: Some(0),
        ..SetAttr::default()
    };
    fs.setattr(ino, &empty).unwrap();
    fs.write(ino, 0, content.as_bytes()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_chats_changes_land_as_its_overlays_would() {
    let mut setup = Setup::new().await;
    for (path, content) in [
        ("edited.txt", "1\n2\n3\n"),
        ("deleted.txt", "gone\n"),
        ("moved.txt", "moving\n"),
    ] {
        write(&setup.project.join(path), content);
    }
    commit_all(&setup.project);
    let app = App::new(&mut setup).await;
    // The chat's changes, in its overlay here.
    let store = app.test.core.paths.data_dir.join("overlays").join(CHAT);
    std::fs::create_dir_all(&store).unwrap();
    let project = setup.project.to_string_lossy().into_owned();
    std::fs::write(store.join("project.txt"), project).unwrap();
    let fs = OverlayFs::open(&setup.project, &store, DEFAULT_LIMIT).unwrap();
    put(&fs, "edited.txt", "ONE\n2\n3\n");
    fs.create(ROOT, "new", Kind::Dir, 0o755, 0, 0, None)
        .unwrap();
    put(&fs, "new/file.txt", "new\n");
    fs.unlink(ROOT, "deleted.txt").unwrap();
    let to = fs.create(ROOT, "to", Kind::Dir, 0o755, 0, 0, None).unwrap();
    let moved = fs.rename(
        ROOT,
        "moved.txt",
        to.ino,
        "moved.txt",
        RenameMode::NoReplace,
    );
    moved.unwrap();
    fs.flush();
    drop(fs);
    // The user edits the file after the chat did.
    write(&setup.project.join("edited.txt"), "1\n2\nTHREE\n");

    // The chat moves, its overlay folded into its snapshot.
    let shadow = Shadow::open(&setup.shadow, &setup.project).unwrap();
    let folded = shadow.fold(CHAT, &chats::changes(CHAT)).unwrap();
    let name = snapshot_ref(CHAT);
    let pushed = setup
        .client
        .push(PROJECT, &setup.shadow, &name, folded.start, |_| {});
    pushed.await.unwrap();
    setup.ensure().await;
    let sync = Request::WorkSync {
        dialog_id: CHAT.into(),
        project: PROJECT.into(),
        snapshot: folded.base.to_string(),
        excludes: Vec::new(),
        changes: Some(folded.start.to_string()),
    };
    let synced: Synced = setup.client.request(sync).await.unwrap();
    chats::discard(CHAT).unwrap();

    // There, the chat's changes are its own.
    let root = PathBuf::from(synced.root);
    assert_eq!(read(&root.join("edited.txt")), "ONE\n2\n3\n");
    let worktree = Repository::open(&root).unwrap();
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = worktree.statuses(Some(&mut opts)).unwrap();
    let mut status: Vec<(String, Status)> = statuses
        .iter()
        .map(|entry| (entry.path().unwrap().to_string(), entry.status()))
        .collect();
    status.sort_by(|a, b| a.0.cmp(&b.0));
    let expected = [
        ("deleted.txt", Status::WT_DELETED),
        ("edited.txt", Status::WT_MODIFIED),
        ("moved.txt", Status::WT_DELETED),
        ("new/file.txt", Status::WT_NEW),
        ("to/moved.txt", Status::WT_NEW),
    ];
    let expected: Vec<(String, Status)> = expected
        .into_iter()
        .map(|(path, status)| (path.to_string(), status))
        .collect();
    assert_eq!(status, expected);

    // A run there, and its result back here.
    app.connect(&setup).await;
    let (url, _) = model(vec![Reply::Text("Carried on.")]).await;
    app.remote
        .start(start(&setup, &url, unattended()))
        .await
        .unwrap();

    landed_as(&setup, "edited.txt", "ONE\n2\nTHREE\n").await;
    let project = |path: &str| setup.project.join(path);
    assert_eq!(read(&project("new/file.txt")), "new\n");
    assert_eq!(read(&project("to/moved.txt")), "moving\n");
    assert!(!project("deleted.txt").exists());
    assert!(!project("moved.txt").exists());
}
