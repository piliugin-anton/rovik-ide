use super::*;

/// The link a test transport runs over.
#[derive(Clone, Copy, Debug)]
enum Kind {
    WebSocket,
    #[cfg(unix)]
    Pipe,
}

/// Runs each `async fn name(kind: Kind)` over both links, as
/// `name::websocket` and (Unix) `name::pipe`.
macro_rules! over_both_links {
    ($($name:ident),+ $(,)?) => {$(
        mod $name {
            #[tokio::test]
            async fn websocket() {
                super::$name(super::Kind::WebSocket).await;
            }

            #[cfg(unix)]
            #[tokio::test]
            async fn pipe() {
                super::$name(super::Kind::Pipe).await;
            }
        }
    )+};
}

mod malformed_frames;
mod reader_writes;
mod stream_shutdown;

/// The far end of a test transport: a WebSocket server, or Chrome's ends
/// of the pipes.
enum Peer {
    WebSocket(WebSocketStream<TcpStream>),
    #[cfg(unix)]
    Pipe {
        /// Reads what the transport writes (Chrome's fd 3).
        commands: pipe::PipeReader,
        /// Writes what the transport reads (Chrome's fd 4).
        replies: tokio::net::unix::pipe::Sender,
    },
}

impl Peer {
    /// Sends one message as a text frame (WebSocket) or NUL-ended (pipe).
    async fn send_text(&mut self, text: impl Into<String>) -> std::result::Result<(), String> {
        self.send_frame(false, text.into().into_bytes()).await
    }

    /// Sends one message, as a binary frame when `binary` (WebSocket). A
    /// pipe has only bytes.
    async fn send_frame(
        &mut self,
        binary: bool,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), String> {
        match self {
            Self::WebSocket(peer) => {
                let message = if binary {
                    Message::Binary(bytes.into())
                } else {
                    Message::Text(
                        String::from_utf8(bytes)
                            .expect("text frames are UTF-8")
                            .into(),
                    )
                };
                peer.send(message).await.map_err(|error| error.to_string())
            }
            #[cfg(unix)]
            Self::Pipe { replies, .. } => {
                use tokio::io::AsyncWriteExt;
                let framed = [bytes.as_slice(), b"\0"].concat();
                replies
                    .write_all(&framed)
                    .await
                    .map_err(|error| error.to_string())
            }
        }
    }

    async fn send_json(&mut self, message: Value) {
        self.send_text(message.to_string()).await.unwrap();
    }

    /// The next message the transport sent, as text.
    async fn recv_text(&mut self) -> String {
        match self {
            Self::WebSocket(peer) => {
                let message = peer.next().await.unwrap().unwrap();
                message.to_text().unwrap().to_owned()
            }
            #[cfg(unix)]
            Self::Pipe { commands, .. } => String::from_utf8(commands.next().await.unwrap())
                .expect("the transport writes UTF-8"),
        }
    }

    async fn recv_json(&mut self) -> Value {
        serde_json::from_str(&self.recv_text().await).unwrap()
    }

    /// The next message, failing the test after two seconds.
    async fn recv_json_soon(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(2), self.recv_json())
            .await
            .expect("the transport must send promptly")
    }

    /// Nothing more arrives within `wait`.
    async fn assert_silent(&mut self, wait: Duration) {
        let next = async {
            match self {
                Self::WebSocket(peer) => drop(peer.next().await),
                #[cfg(unix)]
                Self::Pipe { commands, .. } => drop(commands.next().await),
            }
        };
        assert!(tokio::time::timeout(wait, next).await.is_err());
    }

    /// The WebSocket itself, for frames a pipe has no counterpart for.
    fn websocket(&mut self) -> &mut WebSocketStream<TcpStream> {
        match self {
            Self::WebSocket(peer) => peer,
            #[cfg(unix)]
            Self::Pipe { .. } => panic!("not a WebSocket peer"),
        }
    }
}

async fn test_peer(
    kind: Kind,
    proxy_auth: Option<(String, String)>,
    cdp_timeout_secs: u64,
    strip: bool,
) -> (Transport, Peer) {
    match kind {
        Kind::WebSocket => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(socket).await.unwrap()
            });
            let stream = Transport::ws_connect(&url, Duration::from_secs(30))
                .await
                .unwrap();
            let transport = Transport::build(
                None,
                Link::WebSocket(Box::new(stream)),
                proxy_auth,
                cdp_timeout_secs,
                false,
                strip,
            )
            .unwrap();
            (transport, Peer::WebSocket(server.await.unwrap()))
        }
        #[cfg(unix)]
        Kind::Pipe => {
            let (ours, chrome) = pipe::devtools_pipes().unwrap();
            let (read, write) = chrome.into_fds();
            let transport = Transport::build(
                None,
                Link::Pipe(ours),
                proxy_auth,
                cdp_timeout_secs,
                false,
                strip,
            )
            .unwrap();
            let peer = Peer::Pipe {
                commands: pipe::PipeReader::new(
                    tokio::net::unix::pipe::Receiver::from_owned_fd(read).unwrap(),
                ),
                replies: tokio::net::unix::pipe::Sender::from_owned_fd(write).unwrap(),
            };
            (transport, peer)
        }
    }
}

async fn pending_test_peer(kind: Kind) -> (Transport, Peer) {
    test_peer_with_header_policy(kind, false).await
}

async fn test_peer_with_proxy_credentials(kind: Kind) -> (Transport, Peer) {
    test_peer(
        kind,
        Some(("fixture-user".into(), "fixture-password".into())),
        2,
        false,
    )
    .await
}

async fn test_peer_with_header_policy(kind: Kind, strip: bool) -> (Transport, Peer) {
    test_peer(kind, None, 30, strip).await
}

/// The peer sees the connection end: EOF, a close frame or an error, after
/// whatever was already sent.
async fn assert_peer_terminated(peer: &mut Peer) {
    // Shutdown preserves bytes already accepted by the OS. Drain buffered
    // frames, then require termination within one deadline without logging data.
    tokio::time::timeout(Duration::from_secs(2), async {
        match peer {
            Peer::WebSocket(peer) => {
                while let Some(Ok(message)) = peer.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                }
            }
            #[cfg(unix)]
            Peer::Pipe { commands, .. } => while commands.next().await.is_ok() {},
        }
    })
    .await
    .expect("peer must observe termination while Transport is retained");
}

over_both_links!(
    proxy_credentials_only_answer_explicit_proxy_challenges,
    auth_without_proxy_credentials_is_left_to_the_caller,
    owned_request_pauses_resolve_once_and_fallback_preserves_header_policy,
    pending_registration_cleans_cancelled_writer_and_reply_waits,
    pending_registration_cleans_internal_timeout_and_write_failure,
    command_dispatch_is_one_shot_and_distinguishes_cancelled_writer_wait,
    command_dispatch_stays_ambiguous_after_lost_reply_error_and_timeout,
    completion_receipt_preserves_late_reply_after_caller_deadline,
    completion_receipt_is_released_by_target_session_detachment,
    pending_completion_owner_does_not_retain_browser_transport,
    closing_transport_is_visible_before_writer_and_reader_finish,
    closing_transport_times_out_writer_and_cleans_owned_runtime,
    binary_frames_preserve_event_payloads_and_command_replies,
);

async fn proxy_credentials_only_answer_explicit_proxy_challenges(kind: Kind) {
    let (transport, mut peer) = test_peer_with_proxy_credentials(kind).await;
    for source in [Some("Server"), None, Some("Unknown"), Some("Proxy")] {
        let mut challenge =
            json!({"origin":"http://fixture.invalid", "scheme":"basic", "realm":"fixture"});
        if let Some(source) = source {
            challenge["source"] = json!(source);
        }
        peer.send_json(json!({
            "method":"Fetch.authRequired", "sessionId":"challenged-session",
            "params":{"requestId":"challenged-request", "authChallenge":challenge}
        }))
        .await;
        let reply = peer.recv_json_soon().await;
        assert_eq!(reply["sessionId"], "challenged-session");
        assert_eq!(reply["method"], "Fetch.continueWithAuth");
        assert_eq!(reply["params"]["requestId"], "challenged-request");
        let expected = if source == Some("Proxy") {
            json!({"response":"ProvideCredentials", "username":"fixture-user", "password":"fixture-password"})
        } else {
            json!({"response":"CancelAuth"})
        };
        assert_eq!(reply["params"]["authChallengeResponse"], expected);
    }
    transport.close().await.unwrap();
}

async fn auth_without_proxy_credentials_is_left_to_the_caller(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let mut events = transport.subscribe();
    peer.send_json(json!({
        "method":"Fetch.authRequired", "sessionId":"caller",
        "params":{"requestId":"request", "authChallenge":{"source":"Server"}}
    }))
    .await;
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, CdpMessage::Event { method, .. } if method == "Fetch.authRequired"));
    transport.close().await.unwrap();
}

async fn owned_request_pauses_resolve_once_and_fallback_preserves_header_policy(kind: Kind) {
    async fn pause(peer: &mut Peer, id: &str) {
        peer.send_json(json!({
            "method":"Fetch.requestPaused", "sessionId":"owned",
            "params":{"requestId":id,"request":{"headers":{"X-Client-Data":"remove","Other":"keep"}}},
        }))
        .await;
    }
    async fn continuation(peer: &mut Peer, id: &str) -> Value {
        let command = peer.recv_json_soon().await;
        assert_eq!(command["method"], "Fetch.continueRequest");
        assert_eq!(command["sessionId"], "owned");
        assert_eq!(
            command["params"],
            json!({"requestId":id,"headers":[{"name":"Other","value":"keep"}]})
        );
        command
    }
    let (transport, mut peer) = test_peer_with_header_policy(kind, true).await;
    let (sender, mut receiver) = mpsc::channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    transport
        .install_request_route("owned", sender.clone(), dropped.clone())
        .unwrap();
    assert!(transport
        .install_request_route("owned", sender, dropped.clone())
        .is_err());
    pause(&mut peer, "queued").await;
    pause(&mut peer, "overflow").await;
    continuation(&mut peer, "overflow").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    let delivered = receiver.recv().await.unwrap();
    assert_eq!(delivered.params["requestId"], "queued");
    transport.remove_request_route("owned");
    let dispatch = CommandDispatch::new();
    let reply = async {
        let command = continuation(&mut peer, "queued").await;
        peer.send_json(json!({"id":command["id"],"result":{}}))
            .await;
    };
    let (result, ()) = tokio::join!(
        delivered.continue_request_with_dispatch(&transport, &dispatch),
        reply
    );
    result.unwrap();
    assert!(dispatch.may_have_been_sent());
    pause(&mut peer, "unowned").await;
    continuation(&mut peer, "unowned").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    transport
        .install_request_route("owned", sender, dropped.clone())
        .unwrap();
    pause(&mut peer, "closed").await;
    continuation(&mut peer, "closed").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    peer.assert_silent(Duration::from_millis(10)).await;
}

mod unicode_post_data {
    use super::*;

    const CASES: &[(&str, &str)] = &[
        (r"\uD800", "\u{fffd}"),
        (r"\uDC00", "\u{fffd}"),
        (r"\u", "\u{fffd}"),
        (r"\u0", "\u{fffd}"),
        (r"\u00", "\u{fffd}"),
        (r"\u123", "\u{fffd}"),
        (r"\uD83D\uDE80", "🚀"),
        (r"\\uD800 \\u00", r"\uD800 \u00"),
        (
            r#"雪 \"requestId\":\"decoy\" \\uD800 \uD83D\uDE80 \uD800"#,
            "雪 \"requestId\":\"decoy\" \\uD800 🚀 \u{fffd}",
        ),
    ];

    async fn pause(peer: &mut Peer, session: &str, id: &str, post_data: &str, response: bool) {
        let session = json!(session);
        let id = json!(id);
        let response = if response {
            r#","responseStatusCode":200"#
        } else {
            ""
        };
        peer.send_text(format!(
            r#"{{"method":"Fetch.requestPaused","sessionId":{session},"params":{{"request":{{"method":"POST","headers":{{"X-Client-Data":"remove","Other":"keep"}},"postData":"{post_data}"}},"requestId":{id}{response}}}}}"#
        ))
        .await
        .unwrap();
    }

    over_both_links!(
        repaired_unowned_pauses_preserve_session_and_header_policy,
        repaired_request_pauses_remain_owned_and_deliver_repaired_post_data,
        repaired_response_pauses_keep_response_route_ownership,
    );

    async fn repaired_unowned_pauses_preserve_session_and_header_policy(kind: Kind) {
        for strip in [false, true] {
            let (transport, mut peer) = test_peer_with_header_policy(kind, strip).await;
            for (index, (raw, _)) in CASES.iter().enumerate() {
                let id = format!("binary-{index}-\"quoted\"");
                pause(&mut peer, "unowned-binary", &id, raw, false).await;
                let reply = peer.recv_json_soon().await;
                assert_eq!(reply["method"], "Fetch.continueRequest");
                assert_eq!(reply["sessionId"], "unowned-binary");
                let mut expected = json!({"requestId":id});
                if strip {
                    expected["headers"] = json!([{"name":"Other","value":"keep"}]);
                }
                assert_eq!(reply["params"], expected);
            }
            transport.close().await.unwrap();
        }
    }

    async fn repaired_request_pauses_remain_owned_and_deliver_repaired_post_data(kind: Kind) {
        let (transport, mut peer) = test_peer_with_header_policy(kind, true).await;
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        transport
            .install_request_route("owned-binary", sender, dropped.clone())
            .unwrap();
        for (index, (raw, expected)) in CASES.iter().enumerate() {
            let id = format!("owned-{index}");
            pause(&mut peer, "owned-binary", &id, raw, false).await;
            let delivered = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(delivered.session_id, "owned-binary");
            assert_eq!(delivered.params["requestId"], id);
            assert_eq!(delivered.params["request"]["postData"], *expected);
            assert_eq!(
                delivered.params["request"]["headers"],
                json!({"X-Client-Data":"remove","Other":"keep"})
            );
            peer.assert_silent(Duration::from_millis(10)).await;
            let reply = async {
                let continuation = peer.recv_json_soon().await;
                assert_eq!(continuation["method"], "Fetch.continueRequest");
                assert_eq!(continuation["sessionId"], "owned-binary");
                assert_eq!(
                    continuation["params"],
                    json!({"requestId":id,"headers":[{"name":"Other","value":"keep"}]})
                );
                peer.send_json(json!({"id":continuation["id"],"result":{}}))
                    .await;
            };
            let (result, ()) = tokio::join!(delivered.continue_request(&transport), reply);
            result.unwrap();
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        transport.close().await.unwrap();
    }

    async fn repaired_response_pauses_keep_response_route_ownership(kind: Kind) {
        let (transport, mut peer) = test_peer_with_header_policy(kind, true).await;
        let (response_sender, mut responses) = mpsc::channel(1);
        let (request_sender, mut requests) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        transport.install_response_route("response-binary", response_sender, dropped.clone());
        transport
            .install_request_route("response-binary", request_sender, dropped.clone())
            .unwrap();
        pause(
            &mut peer,
            "response-binary",
            "owned-response",
            r"\uDC00",
            true,
        )
        .await;
        let delivered = tokio::time::timeout(Duration::from_secs(2), responses.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered["requestId"], "owned-response");
        assert_eq!(delivered["request"]["postData"], "\u{fffd}");
        assert_eq!(delivered["responseStatusCode"], 200);
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        peer.assert_silent(Duration::from_millis(10)).await;
        transport.remove_response_route("response-binary");
        pause(
            &mut peer,
            "response-binary",
            "unowned-response",
            r"\u00",
            true,
        )
        .await;
        let continuation = peer.recv_json_soon().await;
        assert_eq!(continuation["method"], "Fetch.continueRequest");
        assert_eq!(continuation["sessionId"], "response-binary");
        assert_eq!(
            continuation["params"],
            json!({"requestId":"unowned-response"})
        );
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        transport.close().await.unwrap();
    }
}

mod repaired_adjacent_messages {
    use super::*;

    over_both_links!(
        repaired_success_and_error_replies_complete_text_and_binary_commands,
        repaired_auth_metadata_preserves_proxy_credentials_and_challenge_routing,
    );

    async fn repaired_success_and_error_replies_complete_text_and_binary_commands(kind: Kind) {
        let (transport, mut peer) = pending_test_peer(kind).await;
        for binary in [false, true] {
            for surrogate in [r"\uD800", r"\uDC00"] {
                for error in [false, true] {
                    let reply = async {
                        let sent = peer.recv_json_soon().await;
                        assert_eq!(sent["method"], "Fixture.repaired");
                        assert_eq!(sent["sessionId"], "reply-session");
                        let id = &sent["id"];
                        let text = if error {
                            format!(
                                r#"{{"id":{id},"error":{{"code":-32000,"message":"failed {surrogate}"}}}}"#
                            )
                        } else {
                            format!(
                                r#"{{"id":{id},"result":{{"value":"{surrogate}","literal":"\\uD800","valid":"\uD83D\uDE80","count":7}}}}"#
                            )
                        };
                        peer.send_frame(binary, text.into_bytes()).await.unwrap();
                    };
                    let params = json!({});
                    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                        tokio::join!(
                            transport.send_to_session::<_, Value>(
                                "reply-session",
                                "Fixture.repaired",
                                &params
                            ),
                            reply
                        )
                    })
                    .await
                    .expect("repaired replies must complete before the command deadline");
                    if error {
                        match result.unwrap_err() {
                            Error::Cdp { code, message, .. } => {
                                assert_eq!(code, Some(-32000));
                                assert_eq!(message, "failed \u{fffd}");
                            }
                            other => panic!("expected the repaired CDP error, got {other:?}"),
                        }
                    } else {
                        assert_eq!(
                            result.unwrap(),
                            json!({"value":"\u{fffd}","literal":r"\uD800","valid":"🚀","count":7})
                        );
                    }
                }
            }
        }
        transport.close().await.unwrap();
    }

    async fn repaired_auth_metadata_preserves_proxy_credentials_and_challenge_routing(kind: Kind) {
        let (transport, mut peer) = test_peer_with_proxy_credentials(kind).await;
        for binary in [false, true] {
            for (index, source) in [Some("Proxy"), Some("Server"), None, Some("Unknown")]
                .into_iter()
                .enumerate()
            {
                let session = format!("challenged-session-{binary}-{index}-\"quoted\"");
                let request = format!("challenged-request-{binary}-{index}-\"quoted\"");
                let mut challenge =
                    json!({"origin":"http://fixture.invalid","scheme":"basic","realm":"fixture"});
                if let Some(source) = source {
                    challenge["source"] = json!(source);
                }
                let text = format!(
                    r#"{{"method":"Fetch.authRequired","sessionId":{},"params":{{"requestId":{},"authChallenge":{challenge},"request":{{"url":"http://fixture.invalid/\uDC00","postData":"\uD800 \"requestId\":\"decoy\", \"sessionId\":\"decoy\", \"source\":\"Proxy\""}}}}}}"#,
                    json!(session),
                    json!(request)
                );
                peer.send_frame(binary, text.into_bytes()).await.unwrap();
                let reply = peer.recv_json_soon().await;
                assert_eq!(reply["method"], "Fetch.continueWithAuth");
                assert_eq!(reply["sessionId"], session);
                let expected = if source == Some("Proxy") {
                    json!({"response":"ProvideCredentials","username":"fixture-user","password":"fixture-password"})
                } else {
                    json!({"response":"CancelAuth"})
                };
                assert_eq!(
                    reply["params"],
                    json!({"requestId":request,"authChallengeResponse":expected})
                );
            }
        }
        transport.close().await.unwrap();
    }
}

async fn pending_registration_cleans_cancelled_writer_and_reply_waits(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let writer = transport.writer.lock().await;
    let (unrelated, _receiver) = oneshot::channel();
    lock_pending(&transport.pending).insert(9999, unrelated);
    for _ in 0..8 {
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            transport.send::<_, Value>("Fixture.wait", &json!({}))
        )
        .await
        .is_err());
        assert_eq!(lock_pending(&transport.pending).len(), 1);
        assert!(lock_pending(&transport.pending).contains_key(&9999));
    }
    drop(writer);
    for _ in 0..8 {
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            transport.send::<_, Value>("Fixture.wait", &json!({}))
        )
        .await
        .is_err());
        assert_eq!(lock_pending(&transport.pending).len(), 1);
        peer.recv_text().await;
    }
    assert!(!transport.reader_handle.as_ref().unwrap().is_finished());
    let reply = async {
        let command = peer.recv_json().await;
        peer.send_json(json!({"id":command["id"], "result":{"ok":true}}))
            .await;
    };
    let params = json!({});
    let (response, ()) = tokio::join!(transport.send::<_, Value>("Fixture.normal", &params), reply);
    assert_eq!(response.unwrap()["ok"], true);
    assert_eq!(lock_pending(&transport.pending).len(), 1);
}

async fn pending_registration_cleans_internal_timeout_and_write_failure(kind: Kind) {
    let (mut transport, _peer) = pending_test_peer(kind).await;
    transport.cmd_timeout = Duration::from_millis(10);
    assert!(transport
        .send::<_, Value>("Fixture.wait", &json!({}))
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.writer.lock().await.close().await.unwrap();
    assert!(transport
        .send::<_, Value>("Fixture.write", &json!({}))
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
}

async fn command_dispatch_is_one_shot_and_distinguishes_cancelled_writer_wait(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let writer = transport.writer.lock().await;
    let dispatch = CommandDispatch::new();
    let params = json!({"requestId":"pause"});
    {
        let send = transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &dispatch,
        );
        tokio::pin!(send);
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut send)
            .await
            .is_err());
        assert!(!dispatch.may_have_been_sent());
        let error = transport
            .send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.continueRequest",
                &params,
                &dispatch,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already used"));
    }
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(!dispatch.may_have_been_sent());
    drop(writer);
    let fallback = CommandDispatch::new();
    let response = async {
        let command = peer.recv_json().await;
        assert_eq!(command["method"], "Fetch.continueRequest");
        peer.send_json(json!({"id":command["id"],"result":{}}))
            .await;
    };
    let (result, ()) = tokio::join!(
        transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.continueRequest",
            &params,
            &fallback
        ),
        response
    );
    result.unwrap();
    assert!(fallback.may_have_been_sent());
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &fallback
        )
        .await
        .is_err());
    peer.assert_silent(Duration::from_millis(10)).await;
}

async fn command_dispatch_stays_ambiguous_after_lost_reply_error_and_timeout(kind: Kind) {
    let (mut transport, mut peer) = pending_test_peer(kind).await;
    let params = json!({"requestId":"pause"});
    let cancelled = CommandDispatch::new();
    {
        let send = transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &cancelled,
        );
        tokio::pin!(send);
        tokio::select! {
            _ = &mut send => panic!("reply was withheld"),
            _ = peer.recv_text() => {}
        }
    }
    assert!(cancelled.may_have_been_sent());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.cmd_timeout = Duration::from_millis(10);
    let timed_out = CommandDispatch::new();
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &timed_out
        )
        .await
        .is_err());
    assert!(timed_out.may_have_been_sent());
    peer.recv_text().await;
    let failed = CommandDispatch::new();
    let response = async {
        let command = peer.recv_json().await;
        peer.send_json(
            json!({"id":command["id"],"error":{"code":-32000,"message":"unknown result"}}),
        )
        .await;
    };
    let (result, ()) = tokio::join!(
        transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &failed
        ),
        response
    );
    assert!(result.is_err());
    assert!(failed.may_have_been_sent());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.writer.lock().await.close().await.unwrap();
    let write_failure = CommandDispatch::new();
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.continueRequest",
            &params,
            &write_failure
        )
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
}

async fn completion_receipt_preserves_late_reply_after_caller_deadline(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let session: Arc<str> = "capture-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    assert!(dispatch.may_have_been_sent());
    let command = peer.recv_json().await;
    let id = command["id"].as_u64().unwrap();
    let mut owner = tokio::spawn(response.wait());
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut owner)
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).contains_key(&id));
    assert_eq!(
        lock_completions(&transport.completion_routes).get(session.as_ref()),
        Some(&id)
    );
    peer.send_json(json!({"id":id,"result":{"data":"native-completion"}}))
        .await;
    assert_eq!(
        owner.await.unwrap().unwrap(),
        json!({"data":"native-completion"})
    );
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(lock_completions(&transport.completion_routes).is_empty());
    transport.close().await.unwrap();
}

async fn completion_receipt_is_released_by_target_session_detachment(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let session: Arc<str> = "detached-capture-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.recv_text().await;
    peer.send_json(json!({
        "method":"Target.detachedFromTarget",
        "params":{"sessionId":session.as_ref(),"targetId":"closed-tab"}
    }))
    .await;
    let result = tokio::time::timeout(Duration::from_secs(2), response.wait())
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::Transport { .. })));
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(lock_completions(&transport.completion_routes).is_empty());
    transport.close().await.unwrap();
}

async fn pending_completion_owner_does_not_retain_browser_transport(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let transport = Arc::new(transport);
    let weak = Arc::downgrade(&transport);
    let session: Arc<str> = "owned-completion-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.recv_text().await;
    let owner = tokio::spawn(response.wait());
    drop(transport);
    assert!(weak.upgrade().is_none());
    let result = tokio::time::timeout(Duration::from_secs(2), owner)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(Error::Transport { .. })));
}

async fn closing_transport_is_visible_before_writer_and_reader_finish(kind: Kind) {
    let (transport, _peer) = pending_test_peer(kind).await;
    let writer = transport.writer.lock().await;
    let mut closing = Box::pin(transport.close());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut closing)
            .await
            .is_err()
    );
    assert!(transport.is_closed());
    assert!(transport
        .send::<_, Value>("Fixture.afterClose", &json!({}))
        .await
        .is_err());
    drop(writer);
    closing.await.unwrap();
}

async fn closing_transport_times_out_writer_and_cleans_owned_runtime(kind: Kind) {
    let (mut transport, _peer) = pending_test_peer(kind).await;
    transport.cmd_timeout = Duration::from_secs(1);
    let runtime = RuntimeDirectory::create(None).unwrap();
    let runtime_path = runtime.path().to_owned();
    *transport.runtime.get_mut().unwrap() = Some(runtime);
    let (_, response) = transport
        .prepare_command(None, "Fixture.pendingAtClose", &json!({}), None)
        .unwrap();
    let writer = transport.writer.lock().await;
    // Socket setup needs real time; only the contended close uses virtual time.
    tokio::time::pause();

    tokio::time::timeout(
        transport.cmd_timeout + Duration::from_millis(10),
        transport.close(),
    )
    .await
    .expect("close must not wait indefinitely for the writer")
    .unwrap();

    let error = tokio::time::timeout(Duration::from_millis(10), response.wait())
        .await
        .expect("close must fail pending commands")
        .unwrap_err();
    assert!(error.to_string().contains("CDP transport closed"));
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(transport.is_closed());
    assert!(!runtime_path.exists(), "close must clean the owned runtime");
    drop(writer);
}

async fn binary_frames_preserve_event_payloads_and_command_replies(kind: Kind) {
    let (transport, mut peer) = pending_test_peer(kind).await;
    let mut events = transport.subscribe();
    let params = json!({"nested":{"text":"quoted \"Unicode: λ\"","values":[null,true,42]}});
    peer.send_frame(
        true,
        serde_json::to_vec(&json!({
            "method":"Fixture.binary",
            "sessionId":"binary-session",
            "params":params,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        CdpMessage::Event {
            method,
            params: received,
            session_id,
        } => {
            assert_eq!(method, "Fixture.binary");
            assert_eq!(received, params);
            assert_eq!(session_id.as_deref(), Some("binary-session"));
        }
        _ => panic!("expected binary event"),
    }
    let reply = async {
        let command = peer.recv_json().await;
        peer.send_frame(
            true,
            serde_json::to_vec(&json!({"id":command["id"],"result":params})).unwrap(),
        )
        .await
        .unwrap();
    };
    let command_params = json!({});
    let (result, ()) = tokio::join!(
        transport.send::<_, Value>("Fixture.binary", &command_params),
        reply
    );
    assert_eq!(result.unwrap(), params);
    transport.close().await.unwrap();
}
