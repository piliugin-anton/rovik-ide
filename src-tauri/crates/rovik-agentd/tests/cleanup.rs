//! What the daemon's cleanup takes (R01): what a chat deleted behind its
//! back left, its worktree, branch and refs, with Rovik's folders measured
//! after; a file uploaded for a message that never came, once its grace
//! period is over, while one `BlobsMissing` renewed stays; and none of the
//! temporary files a command may still use. Each chat's working copy is
//! measured by kind; "Clean up now" and the storage budget remove idle
//! chats' dependencies and build output, never a busy chat's.

mod common;

use common::project::{ended, message, ready_to_run, Setup, CHAT};
use common::write;
use git2::BranchType;
use rovik_core::chat_db::attachments;
use rovik_core::testing::{beating, beats, model, Reply};
use rovik_protocol::host::Storage;
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::Request;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Two hours, in ms: past an upload's grace period of one.
const TWO_HOURS: i64 = 2 * 60 * 60 * 1000;

/// A one-page PDF, its comment `note`.
fn pdf(note: &str) -> Vec<u8> {
    format!(
        "%PDF-1.4\n% {note}\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
         2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
         3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 3 3]>>endobj\n\
         trailer<</Root 1 0 R>>\n%%EOF"
    )
    .into_bytes()
}

#[tokio::test]
async fn the_cleanup_takes_what_a_deleted_chat_left_and_measures_again() {
    let setup = Setup::new().await;
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);
    write(&root.join("target/out.bin"), &"x".repeat(1 << 20));
    let before: Storage = setup.client.request(Request::Storage).await.unwrap();
    assert!(before.work >= 1 << 20, "{before:?}");
    // The chat's rows went without the daemon's hearing of it.
    let deleted = setup
        .db()
        .execute("DELETE FROM dialogs WHERE id = ?1", [CHAT]);
    assert_eq!(deleted.unwrap(), 1);

    let after: Storage = setup.client.request(Request::CleanUp).await.unwrap();
    assert!(!root.exists());
    let there = setup.there();
    assert!(there
        .find_branch("rovik/chat-1", BranchType::Local)
        .is_err());
    assert!(there.find_reference(&snapshot_ref(CHAT)).is_err());
    assert!(setup.daemon.state()["chats"].get(CHAT).is_none());
    assert!(
        after.work + (1 << 20) <= before.work,
        "{before:?} {after:?}"
    );
    assert!(after.projects > 0, "the project stays: {after:?}");
}

/// K43: only the grace period keeps an upload from its message on the
/// daemon, and `BlobsMissing` starts it again for a message on its way.
#[tokio::test]
async fn a_file_for_a_message_that_never_came_goes_after_its_grace() {
    let setup = Setup::new().await;
    let (lost, renewed) = (pdf("lost"), pdf("renewed"));
    let ids = [attachments::id_of(&lost), attachments::id_of(&renewed)];
    for (id, bytes) in ids.iter().zip([lost, renewed]) {
        setup.client.put_blob(id, bytes, |_| {}).await.unwrap();
    }
    let data = setup.daemon.data.join("data");
    assert!(ids
        .iter()
        .all(|id| attachments::file_for(&data, id).is_some()));
    // Both went up two hours ago; a message is about to name the second.
    let aged = setup.db().execute(
        "UPDATE attachments SET created_at = created_at - ?1",
        [TWO_HOURS],
    );
    assert_eq!(aged.unwrap(), 2);
    let asked = Request::BlobsMissing {
        ids: vec![ids[1].clone()],
    };
    let missing: Vec<String> = setup.client.request(asked).await.unwrap();
    assert!(missing.is_empty());

    setup
        .client
        .request::<Storage>(Request::CleanUp)
        .await
        .unwrap();
    assert!(
        attachments::file_for(&data, &ids[0]).is_none(),
        "past its grace"
    );
    assert!(attachments::file_for(&data, &ids[1]).is_some(), "renewed");
    let rows: i64 = setup
        .db()
        .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

/// "Clean up now" leaves the temporary files changed within the hour: a
/// command going may still use them. They're measured with the rest.
#[tokio::test]
async fn clean_up_now_leaves_the_temporary_files_a_command_may_use() {
    let setup = Setup::new().await;
    let made = setup.daemon.data.join("tmp/cargo-build/out.bin");
    write(&made, &"x".repeat(1 << 20));

    let after: Storage = setup.client.request(Request::CleanUp).await.unwrap();
    assert!(made.is_file());
    assert!(after.tmp >= 1 << 20, "{after:?}");
}

/// A second chat of the project, with its worktree: its root.
const OTHER_CHAT: &str = "chat-2";

async fn another_chat(setup: &Setup) -> PathBuf {
    let snapshot = setup.pushed(OTHER_CHAT).await;
    PathBuf::from(setup.sync(OTHER_CHAT, &snapshot).await.unwrap().root)
}

/// A megabyte of installed dependencies in the worktree at `root`, which
/// its git ignores.
fn install(root: &Path) -> PathBuf {
    write(&root.join(".gitignore"), "node_modules/\n");
    write(&root.join("package.json"), "{}");
    let modules = root.join("node_modules");
    write(&modules.join("big/index.js"), &"x".repeat(1 << 20));
    modules
}

async fn storage(setup: &Setup) -> Storage {
    setup.client.request(Request::Storage).await.unwrap()
}

async fn clean_up(setup: &Setup) -> Storage {
    setup.client.request(Request::CleanUp).await.unwrap()
}

#[tokio::test]
async fn storage_lists_the_chats_largest_first_with_what_each_holds() {
    let setup = Setup::new().await;
    ready_to_run(&setup).await;
    install(&another_chat(&setup).await);

    let chats = storage(&setup).await.chats;
    let ids: Vec<&str> = chats.iter().map(|c| c.chat.as_str()).collect();
    assert_eq!(ids, [OTHER_CHAT, CHAT]);
    assert!(chats[0].dependencies >= 1 << 20, "{chats:?}");
    assert!(chats[0].source > 0 && chats[1].source > 0, "{chats:?}");
    assert_eq!(chats[1].dependencies, 0);
    assert!(chats.iter().all(|c| !c.running && !c.keep_ready));
}

#[tokio::test]
async fn clean_up_removes_idle_chats_dependencies_and_keeps_the_chat() {
    let setup = Setup::new().await;
    let root = ready_to_run(&setup).await;
    write(&root.join("src/main.rs"), "fn main() { edited(); }\n");
    write(&root.join("notes.txt"), "the user's");
    let modules = install(&root);

    let after = clean_up(&setup).await;
    assert!(!modules.exists());
    assert_eq!(
        common::project::read(&root.join("src/main.rs")),
        "fn main() { edited(); }\n"
    );
    assert!(root.join("notes.txt").exists());
    assert_eq!(after.chats.len(), 1, "the chat stays: {after:?}");
    assert_eq!(after.chats[0].dependencies, 0);
    assert!(setup.daemon.state()["chats"].get(CHAT).is_some());
}

#[tokio::test]
async fn clean_up_leaves_a_chat_kept_ready() {
    let setup = Setup::new().await;
    let modules = install(&ready_to_run(&setup).await);
    let keep = Request::ChatKeepReady {
        dialog_id: CHAT.into(),
        keep: true,
    };
    setup.client.request::<()>(keep).await.unwrap();

    clean_up(&setup).await;
    assert!(modules.exists());
    let unknown = Request::ChatKeepReady {
        dialog_id: "chat-9".into(),
        keep: true,
    };
    assert!(setup.client.request::<()>(unknown).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_chat_keeps_its_dependencies() {
    let setup = Setup::new().await;
    let root = ready_to_run(&setup).await;
    let modules = install(&root);
    let (url, _) = model(vec![Reply::Hang(None)]).await;
    let start = Request::AgentStart(Box::new(message(&setup, &url, "later")));
    setup.client.call(start).await.unwrap();

    let during = clean_up(&setup).await;
    assert!(modules.exists(), "its run goes");
    assert!(during.chats[0].running, "{during:?}");

    let stop = Request::AgentStop {
        dialog_id: CHAT.into(),
    };
    setup.client.call(stop).await.unwrap();
    ended(&setup.client).await;
    clean_up(&setup).await;
    assert!(!modules.exists(), "its run ended");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_with_a_background_command_keeps_its_files() {
    let setup = Setup::new().await;
    let root = ready_to_run(&setup).await;
    let modules = install(&root);
    let serve = Reply::Call {
        id: "b1",
        name: "bash",
        args: json!({ "command": beating("listening"), "background": true }),
    };
    let (url, _) = model(vec![serve, Reply::Text("It serves.")]).await;
    let start = Request::AgentStart(Box::new(message(&setup, &url, "later")));
    setup.client.call(start).await.unwrap();
    ended(&setup.client).await;
    assert!(beats(&root), "it runs on after the run");

    let after = clean_up(&setup).await;
    assert!(modules.exists(), "a server may use them");
    assert!(after.chats[0].running, "{after:?}");
    let stop = Request::BackgroundStop {
        dialog_id: CHAT.into(),
        id: "bg1".into(),
    };
    setup.client.call(stop).await.unwrap();
}

#[tokio::test]
async fn over_its_budget_the_chat_idle_longest_loses_its_dependencies_unless_kept_ready() {
    let setup = Setup::new().await;
    let idle = install(&ready_to_run(&setup).await);
    let recent = install(&another_chat(&setup).await);
    let aged = setup.db().execute(
        "UPDATE dialogs SET updated_at = updated_at - ?1 WHERE id = ?2",
        rusqlite::params![TWO_HOURS, CHAT],
    );
    assert_eq!(aged.unwrap(), 1);
    let keep = |keep| Request::ChatKeepReady {
        dialog_id: CHAT.into(),
        keep,
    };
    let budget = Request::StorageBudget { bytes: Some(1) };

    setup.client.request::<()>(keep(true)).await.unwrap();
    setup.client.request::<()>(budget.clone()).await.unwrap();
    assert!(idle.exists() && recent.exists(), "kept ready; active");
    let kept = storage(&setup).await;
    assert_eq!(kept.budget, Some(1));
    let chat = kept.chats.iter().find(|c| c.chat == CHAT).unwrap();
    assert!(chat.keep_ready, "{kept:?}");

    setup.client.request::<()>(keep(false)).await.unwrap();
    setup.client.request::<()>(budget).await.unwrap();
    assert!(!idle.exists(), "idle for two hours");
    assert!(recent.exists(), "active within the hour");

    let off = Request::StorageBudget { bytes: None };
    setup.client.request::<()>(off).await.unwrap();
    assert_eq!(storage(&setup).await.budget, None);
}
