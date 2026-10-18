use super::*;

/// What makes the reader write on its own.
enum Trigger {
    /// A WebSocket ping, answered with a pong.
    Ping,
    /// An event answered with a CDP command.
    Event(Value),
}

async fn blocked_reader_write_fails_native_completion(kind: Kind, trigger: Trigger) {
    let (transport, mut peer) = test_peer_with_proxy_credentials(kind).await;
    let session: Arc<str> = "blocked-reader".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.recv_text().await;
    let writer = transport.writer.lock().await;
    match trigger {
        Trigger::Ping => peer
            .websocket()
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap(),
        Trigger::Event(event) => peer.send_json(event).await,
    }
    let result = tokio::time::timeout(
        transport.cmd_timeout + Duration::from_secs(1),
        response.wait(),
    )
    .await;
    let closed = transport.is_closed();
    assert_peer_terminated(&mut peer).await;
    drop(writer);
    transport.close().await.unwrap();
    assert!(
        matches!(result, Ok(Err(Error::Transport { .. }))),
        "reader never failed its stalled write: {result:?}"
    );
    assert!(closed, "stalled reader must invalidate the connection");
    assert!(
        dispatch.may_have_been_sent(),
        "failure does not permit command replay"
    );
}

// Real time is required: a paused Tokio clock can expire the outer deadline
// before the kernel delivers the frame that starts the reader's write deadline.
#[tokio::test]
async fn pong_writer_contention_cannot_strand_native_completion() {
    blocked_reader_write_fails_native_completion(Kind::WebSocket, Trigger::Ping).await;
}

over_both_links!(
    fetch_writer_contention_cannot_strand_native_completion,
    auth_writer_contention_cannot_strand_native_completion,
    automatic_reply_backpressure_cannot_strand_native_completion,
);

async fn fetch_writer_contention_cannot_strand_native_completion(kind: Kind) {
    blocked_reader_write_fails_native_completion(
        kind,
        Trigger::Event(json!({"method":"Fetch.requestPaused","sessionId":"paused-session","params":{"requestId":"paused-request","request":{"headers":{}}}})),
    )
    .await;
}

async fn auth_writer_contention_cannot_strand_native_completion(kind: Kind) {
    blocked_reader_write_fails_native_completion(
        kind,
        Trigger::Event(json!({"method":"Fetch.authRequired","sessionId":"auth-session","params":{"requestId":"auth-request","authChallenge":{"source":"Proxy"}}})),
    )
    .await;
}

#[tokio::test]
async fn silent_websocket_upgrade_obeys_connection_deadline() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let mut connection = tokio::spawn(async move { Transport::connect(&url, 1).await });
    let (mut socket, _) = listener.accept().await.unwrap();
    // TCP connects and the HTTP upgrade reaches the peer, which never replies.
    let mut first_byte = [0];
    socket.read_exact(&mut first_byte).await.unwrap();
    // Advance time only after the real socket has delivered the upgrade request.
    tokio::time::pause();
    let result = tokio::time::timeout(Duration::from_secs(2), &mut connection).await;
    connection.abort();
    assert!(
        matches!(result, Ok(Ok(Err(Error::Timeout(_))))),
        "connection waited beyond its deadline for the WebSocket upgrade"
    );
}

/// A transport whose automatic replies the peer never reads: a WebSocket
/// over a TCP connection with small buffers, or the pipe, whose buffer the
/// kernel bounds.
async fn backpressure_peer(kind: Kind) -> (Transport, Peer) {
    match kind {
        Kind::WebSocket => small_buffer_websocket_peer().await,
        #[cfg(unix)]
        Kind::Pipe => test_peer(kind, None, 1, true).await,
    }
}

async fn small_buffer_websocket_peer() -> (Transport, Peer) {
    let listener = tokio::net::TcpSocket::new_v4().unwrap();
    // Bound the advertised receive window before the handshake. A small sender
    // buffer alone does not bound buffering on every platform.
    listener.set_recv_buffer_size(64 * 1024).unwrap();
    listener.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = listener.listen(1).unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_send_buffer_size(1024).unwrap();
    let socket = socket.connect(address).await.unwrap();
    let (stream, _) =
        tokio_tungstenite::client_async(format!("ws://{address}"), MaybeTlsStream::Plain(socket))
            .await
            .unwrap();
    let transport = Transport::build(
        None,
        Link::WebSocket(Box::new(stream)),
        None,
        1,
        false,
        true,
    )
    .unwrap();
    (transport, Peer::WebSocket(server.await.unwrap()))
}

async fn automatic_reply_backpressure_cannot_strand_native_completion(kind: Kind) {
    let (transport, mut peer) = backpressure_peer(kind).await;
    let session: Arc<str> = "backpressure".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.recv_text().await;
    // A single write can exceed SO_SNDBUF on Windows, and a brief stall can
    // recover after a delayed ACK on macOS. Keep requesting automatic replies
    // without reading them, up to 64 MiB, under one outer deadline. This lets the
    // reader's own write deadline detect sustained socket backpressure.
    let pause = json!({
        "method":"Fetch.requestPaused", "sessionId":"backpressure",
        "params":{"requestId":"blocked", "request":{"headers":{
            "X-Client-Data":"remove", "Large":"x".repeat(1024 * 1024)
        }}}
    })
    .to_string();
    let produce = async {
        for _ in 0..64 {
            if peer.send_text(pause.clone()).await.is_err() {
                break;
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let completion = response.wait();
        tokio::pin!(completion);
        tokio::select! {
            result = &mut completion => result,
            () = produce => completion.await,
        }
    })
    .await;
    let closed = transport.is_closed();
    // Check the write deadline before resuming reads, which would relieve
    // backpressure. This must be a terminal transport error, not our outer timeout.
    assert!(
        matches!(result, Ok(Err(Error::Transport { .. }))),
        "{result:?}"
    );
    assert!(closed);
    assert!(dispatch.may_have_been_sent());
    // Some platforms finish sending an already queued frame during shutdown.
    // Drain it and require EOF/error while Transport is still retained.
    assert_peer_terminated(&mut peer).await;
    transport.close().await.unwrap();
}
