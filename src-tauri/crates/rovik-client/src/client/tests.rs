//! The client against daemons of the tests' own: one that answers, one
//! that serves only newer apps, and none at all.

use super::*;
use crate::connector::{Connecting, Stream as Io};
use crate::testing::{identity, Nowhere};

/// A daemon on the other end of each connection, which welcomes with
/// `min_protocol` and answers every call `Done`.
struct Fake {
    min_protocol: u32,
}

impl Connector for Fake {
    fn connect(&self) -> Connecting<'_> {
        let min_protocol = self.min_protocol;
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 16);
            tokio::spawn(serve(server, min_protocol));
            Ok(Box::new(client) as Io)
        })
    }
}

async fn serve(io: tokio::io::DuplexStream, min_protocol: u32) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(io).await else {
        return;
    };
    let Some(Frame::Hello { .. }) = next_frame(&mut socket).await else {
        return;
    };
    let welcome = Frame::Welcome {
        protocol: min_protocol,
        min_protocol,
        daemon: "fake".into(),
        boot: "boot".into(),
        features: Vec::new(),
    };
    let _ = socket.send(text(&welcome)).await;
    while let Some(frame) = next_frame(&mut socket).await {
        let answer = match frame {
            Frame::Subscribe { .. } => Frame::Subscribed {
                replayed: 0,
                complete: false,
            },
            Frame::Call { id, .. } => Frame::Reply {
                id,
                res: Ok(Response::Done),
            },
            _ => continue,
        };
        let _ = socket.send(text(&answer)).await;
    }
}

fn client(connector: impl Connector) -> (Client, mpsc::UnboundedReceiver<Incoming>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    (Client::connect(connector, identity("test-host"), sink), rx)
}

async fn until_state(heard: &mut mpsc::UnboundedReceiver<Incoming>, state: HostState) {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), heard.recv()).await {
            Ok(Some(Incoming::State(s))) if s == state => return,
            Ok(Some(_)) => continue,
            _ => panic!("never {state:?}"),
        }
    }
}

#[tokio::test]
async fn calls_are_answered_on_the_websocket_by_their_id() {
    let (client, mut heard) = client(Fake {
        min_protocol: PROTOCOL,
    });
    until_state(&mut heard, HostState::Ready).await;
    assert_eq!(client.call(Request::HostInfo).await, Ok(Response::Done));
    let (a, b) = tokio::join!(
        client.call(Request::AgentStatus),
        client.call(Request::ClmStatus {
            dialog_id: "d".into()
        })
    );
    assert_eq!((a, b), (Ok(Response::Done), Ok(Response::Done)));
}

/// 4.5: a daemon that serves only newer apps is no use to this one.
#[tokio::test]
async fn a_daemon_for_newer_apps_is_a_version_mismatch() {
    let (client, mut heard) = client(Fake {
        min_protocol: PROTOCOL + 1,
    });
    until_state(&mut heard, HostState::VersionMismatch).await;
    assert_eq!(client.daemon().unwrap().min_protocol, PROTOCOL + 1);
    let refused = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(refused.code, FailCode::Unreachable);
}

#[tokio::test]
async fn without_a_daemon_calls_fail_as_unreachable() {
    let (client, mut heard) = client(Nowhere);
    until_state(&mut heard, HostState::Offline).await;
    let refused = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(refused.code, FailCode::Unreachable);
    assert_eq!(
        refused.message,
        "test-host isn't reachable. This chat is read-only until it is."
    );
}

/// 4.1: a call over 64 MB isn't sent at all, and says why.
#[tokio::test]
async fn a_call_over_64_mb_is_refused_with_the_reason() {
    let (client, _heard) = client(Nowhere);
    let rename = Request::ChatRename {
        dialog_id: "d".into(),
        title: "x".repeat(65 << 20),
    };
    let refused = client.call(rename).await.unwrap_err();
    assert_eq!(refused.code, FailCode::TooLarge);
    assert_eq!(
        refused.message,
        "This message is too big to send to test-host: 66 MB, 64 MB at most."
    );
}

/// 4.3: the next `Subscribe` goes on from the last event of the same
/// boot; a daemon started again since is subscribed from its first.
#[tokio::test]
async fn a_daemon_started_again_is_subscribed_from_its_first_event() {
    let (client, _heard) = client(Nowhere);
    let shared = &client.shared;
    assert_eq!(
        shared.resume_from("one"),
        (None, false),
        "a first connection"
    );
    shared.lock().since = Some(("one".into(), 41));
    assert_eq!(shared.resume_from("one"), (Some(("one".into(), 41)), true));
    assert_eq!(shared.resume_from("two"), (Some(("two".into(), 0)), false));
}

/// 6.10: a wall clock that jumps ahead of the monotonic one is a sleep;
/// one that keeps pace isn't.
#[tokio::test]
async fn a_jump_of_the_wall_clock_is_a_sleep() {
    use std::sync::atomic::AtomicU32;
    let tick = Duration::from_millis(10);
    let wall = |jump_after: u32| {
        let looks = Arc::new(AtomicU32::new(0));
        move || {
            let look = looks.fetch_add(1, Ordering::Relaxed);
            let jump = if look >= jump_after {
                SLEPT * 2
            } else {
                Duration::ZERO
            };
            SystemTime::now() + jump
        }
    };
    let steady = tokio::time::timeout(tick * 20, slept(tick, wall(u32::MAX))).await;
    assert!(steady.is_err(), "no sleep");
    let woke = tokio::time::timeout(Duration::from_secs(10), slept(tick, wall(3))).await;
    assert!(woke.is_ok(), "a sleep");
}
