//! The daemon's socket, sessions and calls (P03), against the real binary.

mod common;

use common::{
    closed_with, hello, send, start_request, websocket, Daemon, Session, BINARY, COMPUTER,
};
use hyper::StatusCode;
use rovik_core::testing::{model, Reply};
use rovik_protocol::{FailCode, Frame, Request, Response, Secret, PROTOCOL};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[tokio::test(flavor = "multi_thread")]
async fn health_answers_without_a_token() {
    let daemon = Daemon::start(&[]).await;
    let (status, body) = daemon.http("GET", "/health", None, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    let health: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["protocol"], PROTOCOL);
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_is_answered_with_welcome() {
    let daemon = Daemon::start(&[]).await;
    let mut session = daemon.session().await;
    let Frame::Welcome {
        protocol,
        min_protocol,
        daemon: name,
        boot,
        ..
    } = &session.welcome
    else {
        unreachable!()
    };
    assert_eq!(*protocol, PROTOCOL);
    assert!(*min_protocol <= PROTOCOL);
    assert!(name.starts_with("rovik-agentd "), "{name}");
    assert!(!boot.is_empty());
    // A call and a ping go through the session.
    let status = session.call(Request::AgentStatus).await.unwrap();
    assert_eq!(status, Response::AgentStatus(Default::default()));
    session.send(&Frame::Ping).await;
    assert_eq!(session.recv().await, Some(Frame::Pong));
    assert!(
        daemon.log().contains(&format!("{COMPUTER} connected")),
        "{}",
        daemon.log()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_is_refused() {
    let daemon = Daemon::start(&[]).await;
    // At the upgrade, and on /call.
    let upgrade = websocket(daemon.stream().await, "not-the-token").await;
    let err = upgrade.err().expect("refused").to_string();
    assert!(err.contains("401"), "{err}");
    let body = serde_json::to_vec(&Frame::Call {
        id: 1,
        req: Request::AgentStatus,
    })
    .unwrap();
    let (status, _) = daemon
        .http("POST", "/call", Some("not-the-token"), body.clone())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = daemon.http("POST", "/call", None, body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A Hello whose token isn't the upgrade's closes the session.
    let mut socket = websocket(daemon.stream().await, &daemon.token)
        .await
        .unwrap();
    send(&mut socket, &hello("not-the-token")).await;
    let (code, reason) = closed_with(&mut socket).await.expect("a close frame");
    assert_eq!(code, 1008, "policy");
    assert!(reason.contains("token"), "{reason}");

    // So does one naming another computer, with the right token.
    let mut socket = websocket(daemon.stream().await, &daemon.token)
        .await
        .unwrap();
    let other = Frame::Hello {
        protocol: PROTOCOL,
        client: "tests".into(),
        device: "desk".into(),
        token: Secret(daemon.token.clone()),
    };
    send(&mut socket, &other).await;
    assert_eq!(closed_with(&mut socket).await.map(|c| c.0), Some(1008));
}

/// Another user of the server can't use the socket, even with its mode
/// opened: the daemon checks each connection's uid. Needs root, to be
/// another user.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_from_another_uid_is_refused() {
    // SAFETY: geteuid can't fail.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: only root can connect as another user");
        return;
    }
    let daemon = Daemon::start(&[]).await;
    std::fs::set_permissions(&daemon.socket, std::fs::Permissions::from_mode(0o666)).unwrap();
    // A copy of the binary that user can run.
    let binary = daemon.dir.0.join("rovik-agentd");
    std::fs::copy(BINARY, &binary).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let health = |uid: Option<u32>| {
        let mut command = Command::new(&binary);
        command
            .args(["attach", "--stdio", "--socket"])
            .arg(&daemon.socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(uid) = uid {
            command.uid(uid).gid(uid);
        }
        async move {
            let mut child = command.spawn().unwrap();
            // stdin stays open until the answer is in: a half-closed
            // connection may get none.
            let mut stdin = child.stdin.take().unwrap();
            stdin
                .write_all(b"GET /health HTTP/1.1\r\nHost: d\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let (mut stdout, mut answer) = (child.stdout.take().unwrap(), String::new());
            tokio::time::timeout(Duration::from_secs(20), stdout.read_to_string(&mut answer))
                .await
                .unwrap()
                .unwrap();
            drop(stdin);
            let _ = child.wait().await;
            answer
        }
    };
    assert!(health(None).await.starts_with("HTTP/1.1 200"));
    let refused = health(Some(65534)).await;
    assert_eq!(refused, "", "nothing answers another user");
    assert!(
        daemon.log().contains("refused a connection from uid 65534"),
        "{}",
        daemon.log()
    );
}

/// Where the server can't forward a Unix socket over SSH, the client runs
/// `attach --stdio` on an exec channel: the session goes through it.
#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_session_works_through_attach_stdio() {
    let daemon = Daemon::start(&[]).await;
    let mut attach = Command::new(BINARY)
        .args(["attach", "--stdio", "--socket"])
        .arg(&daemon.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let io = tokio::io::join(attach.stdout.take().unwrap(), attach.stdin.take().unwrap());
    let mut session = Session::open(io, &daemon.token).await.unwrap();
    assert!(matches!(session.welcome, Frame::Welcome { .. }));
    let status = session.call(Request::AgentStatus).await;
    assert_eq!(status, Ok(Response::AgentStatus(Default::default())));
    let info = session.call(Request::HostInfo).await.unwrap();
    let Response::HostInfo(info) = info else {
        panic!("{info:?}")
    };
    assert_eq!(info.protocol, PROTOCOL);
    assert!(!info.virtual_files, "--no-virtual-files");
}

/// A call too big for a frame goes through `POST /call` and gets the reply
/// the WebSocket would give; a reply too big for a frame comes back on the
/// WebSocket as `TooLarge`, and whole through `/call` (4.1).
#[tokio::test(flavor = "multi_thread")]
async fn big_calls_and_replies_go_through_post_call() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let mut session = daemon.session().await;
    // The same request, both ways, the same reply.
    let small = session.call(Request::AgentStatus).await;
    assert_eq!(
        daemon.post_call(1, Request::AgentStatus).await,
        Frame::Reply { id: 1, res: small }
    );

    // A 3 MB message: a run asks to write 2 MB over a file, which waits for
    // the user's approval, the whole file in it.
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("big.txt"), "old\n").unwrap();
    // Lines that differ, which no repetition check stops.
    let big_file: String = (0..)
        .map(|n| format!("line {n}\n"))
        .scan(0, |size, line| {
            *size += line.len();
            (*size <= 2 << 20).then_some(line)
        })
        .collect();
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "big.txt", "content": big_file }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let start = start_request(&url, &project, "y".repeat(3 << 20));
    let started = daemon.post_call(2, Request::AgentStart(start)).await;
    let Frame::Reply {
        id: 2,
        res: Ok(Response::RunId(Some(_))),
    } = started
    else {
        panic!("{started:?}\n{}", daemon.output());
    };

    // Its status carries the whole file: too big for a frame.
    let mut too_large = None;
    for _ in 0..800 {
        match session.call(Request::AgentStatus).await {
            Err(fail) => {
                too_large = Some(fail);
                break;
            }
            Ok(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    let fail = too_large.unwrap_or_else(|| panic!("no approval waited\n{}", daemon.output()));
    assert_eq!(fail.code, FailCode::TooLarge, "{fail:?}");
    let Frame::Reply {
        res: Ok(Response::AgentStatus(status)),
        ..
    } = daemon.post_call(3, Request::AgentStatus).await
    else {
        panic!("no status through /call");
    };
    let edit = status.approvals[0].edit.as_ref().unwrap();
    assert_eq!(edit.proposed, big_file);

    let stopped = session
        .call(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_needs_same_machine_until_projects_are_carried() {
    let daemon = Daemon::start(&[]).await;
    let project = daemon.dir.0.join("project");
    let start = start_request("http://127.0.0.1:9", &project, "Hi".into());
    let mut session = daemon.session().await;
    let fail = session.call(Request::AgentStart(start)).await.unwrap_err();
    assert_eq!(fail.code, FailCode::BadRequest);
    assert!(fail.message.contains("--same-machine"), "{}", fail.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_daemon_runs_for_a_data_folder() {
    let daemon = Daemon::start(&[]).await;
    let second = Command::new(BINARY)
        .arg("serve")
        .arg("--data")
        .arg(&daemon.data)
        .arg("--socket")
        .arg(daemon.dir.0.join("second.sock"))
        .arg("--no-virtual-files")
        .env("SHELL", "/bin/sh")
        .output()
        .await
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("Another rovik-agentd"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn version_names_the_protocol() {
    let out = Command::new(BINARY).arg("version").output().await.unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.trim(),
        format!(
            "rovik-agentd {} (protocol {PROTOCOL})",
            env!("CARGO_PKG_VERSION")
        )
    );
}
