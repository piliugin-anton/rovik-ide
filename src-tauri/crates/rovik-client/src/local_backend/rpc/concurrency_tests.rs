use super::*;
use crate::desktop_api::{chat::ChatRequest, projects::ProjectRequest};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(3);

fn ready() -> Ready {
    Ready {
        protocol: super::super::PROTOCOL,
        boot: "test".into(),
        version: "test".into(),
        session: "test".into(),
    }
}

#[tokio::test]
async fn replies_can_finish_out_of_order_while_an_operation_exceeds_the_frame_deadline() {
    let (stream, mut peer) = tests::stream_pair().await;
    let frame_wait = Duration::from_millis(10);
    let client = Arc::new(Client::start_with_frame_wait(
        Path::new(""),
        stream,
        ready(),
        frame_wait,
    ));
    let calling = client.clone();
    let slow = tokio::spawn(async move { calling.call(Request::AgentStatus).await });
    assert!(matches!(
        read::<ClientFrame>(&mut peer).await.unwrap(),
        ClientFrame::Call { id: 1, .. }
    ));
    // No reply bytes means executing work, not a truncated frame.
    tokio::time::sleep(frame_wait * 3).await;
    assert!(!slow.is_finished());
    let calling = client.clone();
    let fast = tokio::spawn(async move {
        calling
            .desktop(crate::desktop_api::vfs::VfsRequest::Status.into())
            .await
    });
    assert!(matches!(
        read::<ClientFrame>(&mut peer).await.unwrap(),
        ClientFrame::Desktop { id: 2, .. }
    ));
    write(
        &mut peer,
        &ServerFrame::DesktopReply {
            id: 2,
            res: Ok(serde_json::json!("fast")),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, fast)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap(),
        "fast"
    );
    assert!(!slow.is_finished());
    write(
        &mut peer,
        &ServerFrame::Reply {
            id: 1,
            res: Box::new(Ok(Response::Done)),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        slow.await.unwrap().unwrap().unwrap(),
        Response::Done
    ));
}

#[tokio::test]
async fn connection_loss_completes_every_waiter_without_replaying_any_request() {
    let (client, mut peer) = tests::pair().await;
    let client = Arc::new(client);
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call(Request::AgentStatus).await })
        })
        .collect();
    for id in 1..=3 {
        assert!(
            matches!(read::<ClientFrame>(&mut peer).await.unwrap(), ClientFrame::Call { id: actual, .. } if actual == id)
        );
    }
    drop(peer);
    for call in calls {
        assert!(tokio::time::timeout(WAIT, call)
            .await
            .unwrap()
            .unwrap()
            .is_err());
    }
    assert!(!client.usable());
    assert_eq!(
        client.call(Request::AgentStatus).await.unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
}

#[tokio::test]
async fn cancelled_ordinary_calls_keep_their_bound_without_consuming_stop_capacity() {
    let (client, mut peer) = tests::pair().await;
    let client = Arc::new(client);
    for id in 1..=ORDINARY_CALLS as u64 {
        let calling = client.clone();
        let call = tokio::spawn(async move { calling.call(Request::AgentStatus).await });
        assert!(
            matches!(read::<ClientFrame>(&mut peer).await.unwrap(), ClientFrame::Call { id: actual, .. } if actual == id)
        );
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
    }
    let calling = client.clone();
    let queued = tokio::spawn(async move { calling.call(Request::AgentStatus).await });
    let calling = client.clone();
    let stop = tokio::spawn(async move {
        calling
            .call(Request::AgentStop {
                dialog_id: "chat".into(),
            })
            .await
    });
    let frame = tokio::time::timeout(WAIT, read::<ClientFrame>(&mut peer))
        .await
        .unwrap()
        .unwrap();
    let ClientFrame::Call { id, req, .. } = frame else {
        panic!("expected Stop");
    };
    assert_eq!(
        *req,
        Request::AgentStop {
            dialog_id: "chat".into()
        }
    );
    assert_eq!(id, ORDINARY_CALLS as u64 + 1);
    assert!(!queued.is_finished());
    write(
        &mut peer,
        &ServerFrame::Reply {
            id,
            res: Box::new(Ok(Response::Done)),
        },
    )
    .await
    .unwrap();
    assert!(stop.await.unwrap().unwrap().is_ok());
    // Completing one abandoned request releases exactly one ordinary slot.
    write(
        &mut peer,
        &ServerFrame::Reply {
            id: 1,
            res: Box::new(Ok(Response::Done)),
        },
    )
    .await
    .unwrap();
    let frame = tokio::time::timeout(WAIT, read::<ClientFrame>(&mut peer))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(frame, ClientFrame::Call { id, .. } if id == ORDINARY_CALLS as u64 + 2));
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
}

async fn running_backend() -> (
    tempfile::TempDir,
    Arc<DesktopBackend>,
    Client,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(crate::desktop_backend::routing_tests::backend(root.path()));
    let (client, peer) = tests::stream_pair().await;
    let serving = backend.clone();
    let server = tokio::spawn(async move { serve(peer, serving, "test".into()).await });
    let client = Client::from_connected(root.path(), client, ready())
        .await
        .unwrap();
    (root, backend, client, server)
}

#[tokio::test]
async fn a_blocked_desktop_operation_does_not_block_chat_history_or_stop_on_the_same_connection() {
    let (_root, backend, client, server) = running_backend().await;
    let client = Arc::new(client);
    let held = backend.project_changes.lock().await;
    let mut blocked = Box::pin(client.desktop(ProjectRequest::Clear.into()));
    assert!(futures_util::poll!(blocked.as_mut()).is_pending());
    // These use the real dispatcher and database; the first operation remains
    // blocked by the same ownership guard used when applying verified changes.
    let history = client.desktop(
        ChatRequest::GetMessages {
            dialog_id: "missing".into(),
            before_seq: None,
            limit: None,
        }
        .into(),
    );
    assert!(tokio::time::timeout(WAIT, history)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    let stop = client.call(Request::AgentStop {
        dialog_id: "missing".into(),
    });
    assert!(tokio::time::timeout(WAIT, stop).await.unwrap().is_ok());
    assert!(futures_util::poll!(blocked.as_mut()).is_pending());
    drop(held);
    assert!(tokio::time::timeout(WAIT, blocked)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    drop(client);
    let _ = tokio::time::timeout(WAIT, server).await.unwrap().unwrap();
    backend.shutdown().await;
}

#[tokio::test]
async fn a_full_server_rejects_excess_work_but_still_dispatches_stop() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(crate::desktop_backend::routing_tests::backend(root.path()));
    let held = backend.project_changes.lock().await;
    let (mut client, peer) = tests::stream_pair().await;
    let serving = backend.clone();
    let server = tokio::spawn(async move { serve(peer, serving, "bounded".into()).await });
    for id in 1..=ORDINARY_CALLS as u64 {
        write(
            &mut client,
            &ClientFrame::Desktop {
                id,
                req: Box::new(ProjectRequest::Clear.into()),
            },
        )
        .await
        .unwrap();
    }
    let excess = ORDINARY_CALLS as u64 + 1;
    write(
        &mut client,
        &ClientFrame::Call {
            id: excess,
            req: Box::new(Request::AgentStatus),
            host: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(tokio::time::timeout(WAIT, read::<ServerFrame>(&mut client)).await.unwrap().unwrap(),
        ServerFrame::Reply { id, res } if id == excess && matches!(*res, Err(Fail { code: rovik_protocol::FailCode::Busy, .. })))
    );
    write(
        &mut client,
        &ClientFrame::Call {
            id: excess + 1,
            req: Box::new(Request::AgentStop {
                dialog_id: "missing".into(),
            }),
            host: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(tokio::time::timeout(WAIT, read::<ServerFrame>(&mut client)).await.unwrap().unwrap(),
        ServerFrame::Reply { id, .. } if id == excess + 1)
    );
    drop(client);
    drop(held);
    let _ = tokio::time::timeout(WAIT, server).await.unwrap().unwrap();
    backend.shutdown().await;
}

#[tokio::test]
async fn an_accepted_database_mutation_finishes_after_its_client_disconnects() {
    let (_root, backend, client, server) = running_backend().await;
    let client = Arc::new(client);
    let (entered, entered_rx) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let db = backend.core.db.clone();
    let held = tokio::spawn(async move {
        db.write(move |_| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
        .await
    });
    entered_rx.await.unwrap();
    let mut mutation = Box::pin(
        client.desktop(
            ChatRequest::CreateDialog {
                id: "accepted".into(),
                title: "Persisted".into(),
                created_at: 1,
                updated_at: 1,
                project_id: None,
            }
            .into(),
        ),
    );
    assert!(futures_util::poll!(mutation.as_mut()).is_pending());
    // A later reply fences receipt/admission of the earlier mutation.
    tokio::time::timeout(WAIT, client.call(Request::AgentStatus))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(futures_util::poll!(mutation.as_mut()).is_pending());
    let owner_session = backend.sessions.get("test").unwrap();
    drop(mutation);
    drop(client);
    tokio::time::timeout(WAIT, owner_session.closed())
        .await
        .unwrap();
    release.send(()).unwrap();
    held.await.unwrap().unwrap();
    let _ = tokio::time::timeout(WAIT, server).await.unwrap().unwrap();
    let dialog = ChatRequest::GetDialog {
        dialog_id: "accepted".into(),
    };
    let value = DesktopRequest::from(dialog)
        .dispatch(&backend)
        .await
        .unwrap();
    assert_eq!(value["title"], "Persisted");
    assert!(backend.sessions.get("test").is_err());
    backend.shutdown().await;
}
