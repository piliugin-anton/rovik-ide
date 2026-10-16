//! What the daemon's cleanup takes (R01): what a chat deleted behind its
//! back left, its worktree, branch and refs, with Rovik's folders measured
//! after; and a file uploaded for a message that never came, once its
//! grace period is over, while one `BlobsMissing` renewed stays.

mod common;

use common::project::{Setup, CHAT};
use common::write;
use git2::BranchType;
use rovik_core::chat_db::attachments;
use rovik_protocol::host::Storage;
use rovik_protocol::packs::snapshot_ref;
use rovik_protocol::Request;
use std::path::PathBuf;

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
