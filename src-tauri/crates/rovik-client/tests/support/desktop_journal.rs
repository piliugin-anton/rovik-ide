use rovik_client::{
    desktop_connection::DesktopConnection,
    desktop_journal::{Batch, Scope},
};
use rovik_core::chat_db::{api, store, DbState};

pub async fn seed(db: &DbState) -> [String; 2] {
    let mut projects = Vec::new();
    for n in 0..2 {
        let root = db.data_dir().join(format!("journal-project-{n}"));
        std::fs::create_dir_all(&root).unwrap();
        let project = api::chat_open_project(db, root.to_string_lossy().into_owned())
            .await
            .unwrap()
            .project
            .id;
        projects.push(project.clone());
        db.write(move |c| {
            let id = format!("journal-chat-{n}");
            store::create_dialog(c, &store::NewDialog { id: id.clone(), title: id.clone(), project_id: Some(project), created_at: 1, updated_at: 1 })?;
            let message = serde_json::from_value(serde_json::json!({"id":format!("journal-message-{n}"),"role":"assistant","createdAt":1,"content":"saved"})).unwrap();
            store::append_messages(c, &id, &[message]).map(|_| ())
        }).await.unwrap();
    }
    projects.try_into().unwrap()
}

pub async fn check(a: &DesktopConnection, b: &DesktopConnection, projects: [String; 2]) {
    let scopes = projects.map(|id| Scope {
        project_id: Some(id),
        chats: vec![],
    });
    let first: Batch =
        serde_json::from_value(a.poll_events(scopes[0].clone(), None).await.unwrap()).unwrap();
    let second: Batch =
        serde_json::from_value(b.poll_events(scopes[1].clone(), None).await.unwrap()).unwrap();
    assert!(first.reset && second.reset);
    for n in 0..2 {
        a.request(rovik_protocol::Request::ChatDismissChecklist {
            dialog_id: format!("journal-chat-{n}"),
            message_id: format!("journal-message-{n}"),
        })
        .await
        .unwrap();
    }
    let seen_a: Batch = serde_json::from_value(
        a.poll_events(scopes[0].clone(), Some(first.cursor.clone()))
            .await
            .unwrap(),
    )
    .unwrap();
    let seen_b: Batch = serde_json::from_value(
        b.poll_events(scopes[1].clone(), Some(second.cursor))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!seen_a.reset && !seen_b.reset);
    assert_eq!(seen_a.events.len(), 1);
    assert_eq!(seen_b.events.len(), 1);
    assert_eq!(seen_a.events[0].payload["dialogId"], "journal-chat-0");
    assert_eq!(seen_b.events[0].payload["dialogId"], "journal-chat-1");
    // A different connection can resume the checkpoint; reads do not consume history.
    let replay: Batch = serde_json::from_value(
        b.poll_events(scopes[0].clone(), Some(first.cursor))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(replay.events[0].payload, seen_a.events[0].payload);
    let reset: Batch = serde_json::from_value(
        a.poll_events(scopes[1].clone(), Some(seen_a.cursor))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(reset.reset);
    assert!(reset.events.is_empty());
    assert!(a.event_status(scopes[0].clone()).await.unwrap()["runs"].is_array());
}
