//! The client against daemons of the tests' own: one that answers, a newer
//! one that still serves this app, one that serves only newer apps, and
//! none at all.

use super::*;
use crate::connector::{Connecting, Stream as Io};
use crate::testing::{identity, Nowhere};

/// A daemon on the other end of each connection, which welcomes with its
/// `protocol` and `min_protocol` and answers every call `Done`.
struct Fake {
    protocol: u32,
    min_protocol: u32,
}

impl Fake {
    /// A daemon that speaks `protocol` and serves only apps that do.
    fn speaking(protocol: u32) -> Fake {
        Fake {
            protocol,
            min_protocol: protocol,
        }
    }
}

impl Connector for Fake {
    fn connect(&self) -> Connecting<'_> {
        let (protocol, min_protocol) = (self.protocol, self.min_protocol);
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 16);
            tokio::spawn(serve(server, protocol, min_protocol));
            Ok(Box::new(client) as Io)
        })
    }
}

async fn serve(io: tokio::io::DuplexStream, protocol: u32, min_protocol: u32) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(io).await else {
        return;
    };
    let Some(Frame::Hello { .. }) = next_frame(&mut socket).await else {
        return;
    };
    let welcome = Frame::Welcome {
        protocol,
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
    let (client, mut heard) = client(Fake::speaking(PROTOCOL));
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

/// 4.5, R02: a daemon newer than this app that still serves it is fine.
#[tokio::test]
async fn a_newer_daemon_that_still_serves_this_app_is_ready() {
    let (client, mut heard) = client(Fake {
        protocol: PROTOCOL + 1,
        min_protocol: PROTOCOL,
    });
    until_state(&mut heard, HostState::Ready).await;
    assert_eq!(client.daemon().unwrap().protocol, PROTOCOL + 1);
    assert_eq!(client.call(Request::HostInfo).await, Ok(Response::Done));
}

/// 4.5: a daemon that serves only newer apps is no use to this one.
#[tokio::test]
async fn a_daemon_for_newer_apps_is_a_version_mismatch() {
    let (client, mut heard) = client(Fake::speaking(PROTOCOL + 1));
    until_state(&mut heard, HostState::VersionMismatch).await;
    assert_eq!(client.daemon().unwrap().min_protocol, PROTOCOL + 1);
    let refused = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(refused.code, FailCode::Unreachable);
}

/// A host that turns this computer away, counting each time it's asked:
/// for good (`may_pass` false: the credentials), or for now (Rovik's
/// install there, until its release's list is published), letting the
/// computer in from try `lets_in_at` on.
struct Turning {
    tries: Arc<AtomicU64>,
    may_pass: bool,
    lets_in_at: u64,
}

impl Turning {
    fn for_good(tries: &Arc<AtomicU64>) -> Turning {
        Turning {
            tries: tries.clone(),
            may_pass: false,
            lets_in_at: u64::MAX,
        }
    }
}

impl Connector for Turning {
    fn connect(&self) -> Connecting<'_> {
        let tries = self.tries.fetch_add(1, Ordering::Relaxed) + 1;
        if tries >= self.lets_in_at {
            return Box::pin(async {
                let (client, server) = tokio::io::duplex(1 << 16);
                tokio::spawn(serve(server, PROTOCOL, PROTOCOL));
                Ok(Box::new(client) as Io)
            });
        }
        let refusal = Refusal {
            why: match self.may_pass {
                true => "Rovik on this server isn't the build you trusted.".into(),
                false => "The host didn't let you in: that password isn't the way in.".into(),
            },
            may_pass: self.may_pass,
        };
        Box::pin(async move { Err(refusal.into_error()) })
    }
}

/// A host that turns this computer away for what only the user can
/// change (the credentials, a host key to pin, the token) is refused,
/// says why, and isn't asked again: the next try would send the
/// credentials once more for the same answer.
#[tokio::test]
async fn a_host_that_turns_the_app_away_is_refused_and_not_asked_again() {
    let tries = Arc::new(AtomicU64::new(0));
    let (client, mut heard) = client(Turning::for_good(&tries));
    until_state(&mut heard, HostState::Refused).await;
    assert_eq!(
        client.refusal().as_deref(),
        Some("The host didn't let you in: that password isn't the way in.")
    );
    // Past the first waits between tries, which a lost link would take,
    // and the wait of a refusal that may pass.
    tokio::time::sleep(FIRST_BACKOFF * 3).await;
    assert_eq!(tries.load(Ordering::Relaxed), 1, "asked once");
    let refused = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(refused.code, FailCode::Unreachable);
}

/// A refusal that may come right on its own (Rovik's install there, whose
/// release's list of builds isn't published yet) is asked again now and
/// then, quietly, Refused meanwhile, and the host connects once it passes.
#[tokio::test]
async fn a_refusal_that_may_pass_is_asked_again_quietly_until_it_does() {
    let tries = Arc::new(AtomicU64::new(0));
    let (client, mut heard) = client(Turning {
        tries: tries.clone(),
        may_pass: true,
        lets_in_at: 3,
    });
    until_state(&mut heard, HostState::Refused).await;
    let why = client.refusal().unwrap();
    assert!(
        why.starts_with("Rovik on this server isn't the build you trusted.")
            && why.contains("Rovik tries again every 10 minutes."),
        "{why}"
    );
    // The next state is Ready, without Connecting at each try between.
    let next = loop {
        match tokio::time::timeout(Duration::from_secs(10), heard.recv()).await {
            Ok(Some(Incoming::State(state))) => break state,
            Ok(Some(_)) => continue,
            _ => panic!("never connected"),
        }
    };
    assert_eq!(next, HostState::Ready);
    assert_eq!(tries.load(Ordering::Relaxed), 3);
    assert_eq!(client.refusal(), None, "the refusal went with it");
    assert_eq!(client.call(Request::HostInfo).await, Ok(Response::Done));
}

/// What isn't a refusal is a link that didn't come up, tried again soon:
/// the SSH agent not up yet, a key file on a drive not mounted, or the
/// system's own "permission denied" on a socket.
#[tokio::test]
async fn an_error_that_isnt_a_refusal_is_tried_again() {
    struct Denied(Arc<AtomicU64>);
    impl Connector for Denied {
        fn connect(&self) -> Connecting<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(std::io::ErrorKind::PermissionDenied.into()) })
        }
    }
    let tries = Arc::new(AtomicU64::new(0));
    let (client, mut heard) = client(Denied(tries.clone()));
    until_state(&mut heard, HostState::Offline).await;
    assert_eq!(client.refusal(), None);
    let deadline = Instant::now() + FIRST_BACKOFF * 5;
    while tries.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "not tried again");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_ne!(client.state(), HostState::Refused);
}

/// 4.5: a daemon that serves only newer apps is asked again at the
/// slowest pace, without the state going back to Connecting each time.
#[tokio::test]
async fn a_version_mismatch_is_asked_again_quietly() {
    let (_client, mut heard) = client(Fake::speaking(PROTOCOL + 1));
    until_state(&mut heard, HostState::VersionMismatch).await;
    tokio::time::sleep(FIRST_BACKOFF * 3).await;
    while let Ok(incoming) = heard.try_recv() {
        assert!(
            !matches!(incoming, Incoming::State(_)),
            "no state flaps: {incoming:?}"
        );
    }
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
