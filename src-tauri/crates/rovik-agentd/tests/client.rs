//! The app's client against the real daemon (P07): its calls and events, a
//! daemon started again under it, and a reply too big for a frame while
//! events keep coming. And its SSH tunnel (H02, H03): each way of logging
//! in, the host key checked before anything is sent, the WebSocket and an
//! upload on channels of their own at once, and `attach --stdio` where the
//! host refuses streamlocal forwarding. And the connection test (H04), and
//! a shell on the host for the terminal panel's remote terminals.

mod common;

use common::ssh::{new_key, SshServer, PASSWORD, USER};
use common::{start_request, until, Daemon, BINARY, COMPUTER};
use rovik_client::connection_test::{test_connection, Check, Outcome, Step, TestOptions};
use rovik_client::ssh::{
    fingerprint, Answerer, Questions, ShellEvent, ShellOutput, Ssh, SshAuth, SshConnector,
    SshError, SshTarget,
};
use rovik_client::{Client, HostState, Identity, Incoming, UnixConnector};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::agent_run::{AgentConfig, StartKind, UserMessage};
use rovik_core::testing::{model, Reply};
use rovik_protocol::agent::AgentStatus;
use rovik_protocol::events::AgentEvent;
use rovik_protocol::host::HostInfo;
use rovik_protocol::{Event, FailCode, Request, Response, Secret, MAX_FRAME, PROTOCOL};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// What a client hears.
struct Heard(mpsc::UnboundedReceiver<Incoming>);

impl Heard {
    /// What comes, up to the first that `last` matches.
    async fn until(&mut self, last: impl Fn(&Incoming) -> bool) -> Vec<Incoming> {
        let mut heard = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(30), self.0.recv()).await;
            let incoming = next
                .ok()
                .flatten()
                .unwrap_or_else(|| panic!("nothing more came after {heard:?}"));
            let done = last(&incoming);
            heard.push(incoming);
            if done {
                return heard;
            }
        }
    }
}

/// `COMPUTER`'s client of the daemon, and what it hears.
fn client(daemon: &Daemon) -> (Client, Heard) {
    let (tx, rx) = mpsc::unbounded_channel();
    let identity = Identity {
        host: "test-server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "client tests".into(),
    };
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    let client = Client::connect(UnixConnector(daemon.socket.clone()), identity, sink);
    (client, Heard(rx))
}

fn project(daemon: &Daemon) -> PathBuf {
    let project = daemon.dir.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    project
}

fn subscribed(incoming: &Incoming) -> bool {
    matches!(incoming, Incoming::Subscribed { .. })
}

fn agent_event(incoming: &Incoming) -> Option<&AgentEvent> {
    match incoming {
        Incoming::Event {
            event: Event::Agent(event),
            ..
        } => Some(event),
        _ => None,
    }
}

fn run_ended(dialog: &'static str) -> impl Fn(&Incoming) -> bool {
    move |incoming| matches!(agent_event(incoming), Some(AgentEvent::RunEnded { dialog_id, .. }) if dialog_id == dialog)
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_and_events_go_through_the_real_daemon() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (client, mut heard) = client(&daemon);
    let first = heard.until(subscribed).await;
    assert_eq!(
        first,
        [
            Incoming::State(HostState::Ready),
            Incoming::Subscribed { complete: false }
        ],
        "a first connection catches up"
    );
    let info: HostInfo = client.request(Request::HostInfo).await.unwrap();
    assert_eq!(info.protocol, PROTOCOL);
    assert_eq!(client.daemon().unwrap().protocol, PROTOCOL);

    let (url, _) = model(vec![Reply::Text("Hello there.")]).await;
    let start = start_request(&url, &project, "Hi".into());
    let run_id: Option<String> = client
        .request(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();
    let events = heard.until(run_ended("chat-1")).await;
    let started = events.iter().filter_map(agent_event).any(
        |e| matches!(e, AgentEvent::RunStarted { run_id: id, .. } if Some(id) == run_id.as_ref()),
    );
    assert!(started, "{events:?}");
    let seqs: Vec<u64> = events
        .iter()
        .filter_map(|i| match i {
            Incoming::Event { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order: {seqs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_started_again_under_the_client_is_reconnected_and_caught_up() {
    let mut daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    let (client, mut heard) = client(&daemon);
    heard.until(subscribed).await;
    // A run whose command is going when the daemon goes.
    let (url, _) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": "echo ran >> runs && sleep 5" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut start = start_request(&url, &project, "Do it".into());
    start.config = Some(AgentConfig {
        permissions: Permissions {
            allow_any_command: true,
            verify_changes: false,
            ..Permissions::default()
        },
        ..AgentConfig::default()
    });
    client
        .request::<Option<String>>(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();
    until("the command ran", || {
        project.join("runs").exists().then_some(())
    })
    .await;

    daemon.kill().await;
    heard
        .until(|i| *i == Incoming::State(HostState::Offline))
        .await;
    let offline = client.call(Request::AgentStatus).await.unwrap_err();
    assert_eq!(offline.code, FailCode::Unreachable);
    assert!(offline.message.starts_with("test-server isn't reachable"));

    daemon.restart().await;
    // Back by itself, it catches up: what it had heard is another boot's.
    let back = heard.until(subscribed).await;
    assert!(
        back.contains(&Incoming::State(HostState::Ready)),
        "{back:?}"
    );
    assert_eq!(back.last(), Some(&Incoming::Subscribed { complete: false }));
    // Then what the daemon did before anyone could connect: the run it
    // restarted asks about the call it cut (5.4).
    heard
        .until(|i| matches!(agent_event(i), Some(AgentEvent::ApprovalRequested { request }) if request.tool_call_id == "c1"))
        .await;
    let status: AgentStatus = client.request(Request::AgentStatus).await.unwrap();
    assert_eq!(status.approvals.len(), 1);
    let stopped: bool = client
        .request(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await
        .unwrap();
    assert!(stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_too_big_for_a_frame_comes_through_post_call_while_events_keep_coming() {
    let daemon = Daemon::start(&["--same-machine"]).await;
    let project = project(&daemon);
    std::fs::write(project.join("big.txt"), "old\n").unwrap();
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
    let (client, mut heard) = client(&daemon);
    heard.until(subscribed).await;
    let start = start_request(&url, &project, "Rewrite it".into());
    client
        .request::<Option<String>>(Request::AgentStart(Box::new(start)))
        .await
        .unwrap();
    // The edit waits for the user, with an approval too big for a frame.
    heard
        .until(|i| {
            matches!(
                i,
                Incoming::Event {
                    event: Event::Resync { .. },
                    ..
                }
            )
        })
        .await;

    // Another chat, whose events come while the big read is answered.
    let (slow, _) = model(vec![Reply::Slow(300, Box::new(Reply::Text("Slowly.")))]).await;
    let mut other = start_request(&slow, &project, "And this".into());
    other.dialog_id = "chat-2".into();
    other.start = StartKind::Send {
        message: UserMessage {
            id: "u2".into(),
            content: "And this".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    client
        .request::<Option<String>>(Request::AgentStart(Box::new(other)))
        .await
        .unwrap();
    let status: AgentStatus = client.request(Request::AgentStatus).await.unwrap();
    let approval = status.approvals.iter().find(|a| a.dialog_id == "chat-1");
    let edit = approval.and_then(|a| a.edit.as_ref()).expect("the edit");
    assert!(edit.proposed.len() > MAX_FRAME);
    // The WebSocket carried on meanwhile, without a new connection.
    let after = heard.until(run_ended("chat-2")).await;
    assert!(
        !after.iter().any(|i| matches!(i, Incoming::State(_))),
        "{after:?}"
    );
    let stopped = client
        .call(Request::AgentStop {
            dialog_id: "chat-1".into(),
        })
        .await;
    assert_eq!(stopped, Ok(Response::Bool(true)));
}

/// The server in the test, as the hosts page would set it up, its key
/// pinned.
fn target(server: &SshServer, auth: SshAuth) -> SshTarget {
    SshTarget {
        host: "127.0.0.1".into(),
        port: server.port,
        user: USER.into(),
        auth,
        host_key: Some(server.host_key_line()),
    }
}

fn password(text: &str) -> SshAuth {
    SshAuth::Password(Secret(text.into()))
}

/// Answers each keyboard-interactive question with `answer`.
fn answering(answer: &'static str) -> Answerer {
    Arc::new(move |questions: Questions| {
        Box::pin(async move {
            let answers = questions.prompts.iter().map(|_| Secret(answer.into()));
            Some(answers.collect())
        })
    })
}

/// H02: the host key is shown before anything is sent, and each way in
/// works once it's pinned, a wrong one refused; a changed key is refused
/// whatever the credentials.
#[tokio::test(flavor = "multi_thread")]
async fn ssh_lets_each_way_in_once_the_host_key_is_pinned() {
    let server = SshServer::start().await;
    let dir = rovik_core::testing::temp_dir();
    let mut first = target(&server, password(PASSWORD));
    first.host_key = None;
    let unknown = Ssh::connect(&first).await.err().unwrap();
    let SshError::UnknownHostKey {
        key,
        fingerprint: shown,
    } = unknown
    else {
        panic!("not asked to check the key: {unknown}");
    };
    let shown_key = russh::keys::PublicKey::from_openssh(&key).unwrap();
    assert_eq!(
        shown_key.key_data(),
        server.host_key.public_key().key_data()
    );
    assert_eq!(shown, fingerprint(server.host_key.public_key()));
    assert!(shown.starts_with("SHA256:"), "{shown}");

    let ssh = Ssh::connect(&target(&server, password(PASSWORD)))
        .await
        .unwrap();
    let ran = ssh.exec("echo hello; exit 3").await.unwrap();
    assert_eq!((ran.status, ran.stdout), (Some(3), b"hello\n".to_vec()));
    let file = server.key_file(&dir.0, "file pass");
    let with_key = SshAuth::Key {
        path: file.clone(),
        passphrase: Some(Secret("file pass".into())),
    };
    Ssh::connect(&target(&server, with_key)).await.unwrap();
    let agent = server.agent(&dir.0).await;
    let with_agent = SshAuth::Agent {
        socket: Some(agent),
    };
    Ssh::connect(&target(&server, with_agent)).await.unwrap();
    let asked = SshAuth::KeyboardInteractive(answering(PASSWORD));
    Ssh::connect(&target(&server, asked)).await.unwrap();

    let wrong = Ssh::connect(&target(&server, password("guess")))
        .await
        .err();
    assert!(matches!(wrong, Some(SshError::Denied(_))), "{wrong:?}");
    let locked = SshAuth::Key {
        path: file,
        passphrase: None,
    };
    let locked = Ssh::connect(&target(&server, locked)).await.err();
    assert!(matches!(locked, Some(SshError::Key(_))), "{locked:?}");
    let asked = SshAuth::KeyboardInteractive(answering("guess"));
    let wrong = Ssh::connect(&target(&server, asked)).await.err();
    assert!(matches!(wrong, Some(SshError::Denied(_))), "{wrong:?}");

    let mut changed = target(&server, password(PASSWORD));
    changed.host_key = Some(new_key().public_key().to_openssh().unwrap());
    let refused = Ssh::connect(&changed).await.err().unwrap();
    let SshError::HostKeyChanged { found, .. } = &refused else {
        panic!("a changed key wasn't refused: {refused}");
    };
    assert_eq!(*found, fingerprint(server.host_key.public_key()));
}

/// H02: through the tunnel, the WebSocket and an upload go at once, each on
/// a `direct-streamlocal` channel of its own to the daemon's socket.
#[tokio::test(flavor = "multi_thread")]
async fn over_ssh_the_websocket_and_an_upload_go_at_once_on_channels_of_their_own() {
    let daemon = Daemon::start(&[]).await;
    let server = SshServer::start().await;
    let (tx, rx) = mpsc::unbounded_channel();
    let identity = Identity {
        host: "ssh-server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "ssh tests".into(),
    };
    let socket = daemon.socket.to_string_lossy().into_owned();
    let connector = SshConnector::new(target(&server, password(PASSWORD))).with_socket(socket);
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    let client = Client::connect(connector, identity, sink);
    let mut heard = Heard(rx);
    heard.until(subscribed).await;

    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.resize(2 << 20, b'%');
    let id = rovik_core::chat_db::attachments::id_of(&pdf);
    let (uploaded, info) = tokio::join!(
        client.put_blob(&id, pdf, |_| {}),
        client.request::<HostInfo>(Request::HostInfo)
    );
    uploaded.unwrap();
    info.unwrap();
    let most = server.most_open.load(std::sync::atomic::Ordering::SeqCst);
    assert!(most >= 2, "only {most} channel open at once");
    let missing: Vec<String> = client
        .request(Request::BlobsMissing { ids: vec![id] })
        .await
        .unwrap();
    assert!(missing.is_empty(), "the daemon has the file");
}

/// H03: a host that refuses streamlocal forwarding is reached through
/// `rovik-agentd attach --stdio` on exec channels, a stream each, and the
/// daemon's socket there comes from `$HOME`.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_refusing_streamlocal_is_reached_through_attach() {
    let daemon = Daemon::start(&[]).await;
    let server = SshServer::refusing_streamlocal().await;
    let socket = daemon.socket.to_string_lossy().into_owned();
    let attach = format!("{BINARY} attach --stdio --socket {socket}");
    let asked = SshConnector::new(target(&server, password(PASSWORD)));
    let home = std::env::var("HOME").unwrap();
    let expected = format!("{}/.rovik/agentd.sock", home.trim_end_matches('/'));
    assert_eq!(asked.socket_path().await.unwrap(), expected, "from $HOME");

    let (tx, rx) = mpsc::unbounded_channel();
    let identity = Identity {
        host: "ssh-server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "ssh tests".into(),
    };
    let connector = SshConnector::new(target(&server, password(PASSWORD)))
        .with_socket(socket)
        .with_attach(attach);
    let sink = Arc::new(move |incoming| {
        let _ = tx.send(incoming);
    });
    let client = Client::connect(connector, identity, sink);
    Heard(rx).until(subscribed).await;
    let info: HostInfo = client.request(Request::HostInfo).await.unwrap();
    assert_eq!(info.protocol, PROTOCOL);
    let pdf = b"%PDF-1.4\n% a file from the test\n".to_vec();
    let id = rovik_core::chat_db::attachments::id_of(&pdf);
    client.put_blob(&id, pdf, |_| {}).await.unwrap();
    let ordering = std::sync::atomic::Ordering::SeqCst;
    assert_eq!(server.most_open.load(ordering), 0, "no streamlocal channel");
    assert!(
        server.execs.load(ordering) >= 3,
        "the $HOME asked, the WebSocket, the upload"
    );
}

/// What a shell on the host printed until it printed `text`, and its end
/// if it ended first.
async fn printed_until(output: &mut ShellOutput, text: &str) -> (String, Option<Option<u32>>) {
    let mut printed = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(30), output.next()).await;
        let event = next.unwrap_or_else(|_| {
            let printed = String::from_utf8_lossy(&printed);
            panic!("nothing more came after {printed:?}")
        });
        match event.expect("the shell's output ended before the shell") {
            ShellEvent::Output(data) => printed.extend_from_slice(&data),
            ShellEvent::Exit(code) => {
                return (String::from_utf8_lossy(&printed).into_owned(), Some(code));
            }
        }
        let shown = String::from_utf8_lossy(&printed).into_owned();
        if shown.contains(text) {
            return (shown, None);
        }
    }
}

/// The terminal panel's remote terminals: the user's login shell on the
/// host, on a terminal of the type and size asked, which follows the
/// view's. What's typed goes in, what it prints comes out, then how it
/// ended. A terminal closed here hangs up: the host ends its shell, and
/// the connection closes.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_on_the_host_runs_on_a_terminal_of_the_size_asked() {
    let server = SshServer::start().await;
    let ssh = Ssh::connect(&target(&server, password(PASSWORD)))
        .await
        .unwrap();
    let ssh = Arc::new(ssh);
    let login = std::env::var("SHELL").unwrap_or_default();
    let name = login.rsplit('/').next().filter(|name| !name.is_empty());
    assert_eq!(ssh.login_shell().await.as_deref(), name, "from $SHELL");

    let opened = Arc::clone(&ssh).shell("xterm-256color", 100, 30).await;
    let (shell, mut output) = opened.unwrap();
    shell.resize(120, 40).unwrap();
    shell.write(b"echo hello from the host\n").unwrap();
    let (printed, ended) = printed_until(&mut output, "hello from the host").await;
    assert_eq!(ended, None, "{printed}");
    assert_eq!(
        *server.terminals.lock().unwrap(),
        ["pty xterm-256color 100x30", "resize 120x40"]
    );
    shell.write(b"exit 3\n").unwrap();
    let (printed, ended) = printed_until(&mut output, "not printed").await;
    assert_eq!(ended, Some(Some(3)), "{printed}");
    assert_eq!(output.next().await, None);
    drop(shell);

    // The connection closes once nothing else holds it: here, the last of
    // it goes to this shell.
    let (shell, mut output) = ssh.shell("xterm-256color", 80, 24).await.unwrap();
    shell.write(b"echo still here\n").unwrap();
    printed_until(&mut output, "still here").await;
    shell.close();
    let (printed, ended) = printed_until(&mut output, "not printed").await;
    assert_eq!(ended, Some(None), "hung up on: {printed}");
    assert!(shell.write(b"echo too late\n").is_err());
}

/// H04: the connection test, a step at a time. The first run stops at the
/// host's key, nothing sent; once it's pinned, a wrong password stops at
/// logging in; the right one lets the probe run on the server (this
/// machine) in one exec, which tries no provider from there, and the
/// daemon answers through the tunnel.
#[tokio::test(flavor = "multi_thread")]
async fn the_connection_test_stops_at_the_key_then_goes_through_to_the_daemon() {
    let daemon = Daemon::start(&[]).await;
    let server = SshServer::start().await;
    let told = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&told);
    let report = move |step: &Step| heard.lock().unwrap().push(step.clone());
    let outcomes = |steps: &[Step]| -> Vec<(Check, Outcome)> {
        steps.iter().map(|s| (s.check, s.outcome)).collect()
    };
    let ordering = std::sync::atomic::Ordering::SeqCst;

    let mut first = target(&server, password(PASSWORD));
    first.host_key = None;
    let options = TestOptions::default();
    let steps = test_connection(&SshConnector::new(first), &options, &report).await;
    assert_eq!(
        outcomes(&steps),
        [
            (Check::Address, Outcome::Passed),
            (Check::HostKey, Outcome::Confirm)
        ]
    );
    assert_eq!(*told.lock().unwrap(), steps, "each step told as it ended");
    let seen = steps[1].host_key.clone().expect("the key to confirm");
    assert_eq!(seen.fingerprint, fingerprint(server.host_key.public_key()));
    assert!(steps[1].message.contains(&seen.fingerprint));
    assert_eq!(server.execs.load(ordering), 0, "nothing ran");

    let mut wrong = target(&server, password("guess"));
    wrong.host_key = Some(seen.key.clone());
    let steps = test_connection(&SshConnector::new(wrong), &options, &report).await;
    assert_eq!(
        outcomes(&steps),
        [
            (Check::Address, Outcome::Passed),
            (Check::HostKey, Outcome::Passed),
            (Check::LogIn, Outcome::Failed)
        ]
    );

    let mut pinned = target(&server, password(PASSWORD));
    pinned.host_key = Some(seen.key);
    let socket = daemon.socket.to_string_lossy().into_owned();
    let connector = SshConnector::new(pinned).with_socket(socket);
    let options = TestOptions {
        deployed: true,
        ..Default::default()
    };
    told.lock().unwrap().clear();
    let steps = test_connection(&connector, &options, &report).await;
    assert_eq!(*told.lock().unwrap(), steps);
    let checks: Vec<Check> = steps.iter().map(|s| s.check).collect();
    let expected = [
        "address",
        "host-key",
        "log-in",
        "system",
        "account",
        "disk",
        "git",
        "chrome",
        "isolation",
        "service",
        "install",
        "daemon",
    ];
    assert_eq!(serde_json::to_value(&checks).unwrap(), json!(expected));
    let outcome = |at: usize| (steps[at].outcome, steps[at].message.as_str());
    assert_eq!(outcome(1).0, Outcome::Passed, "{}", outcome(1).1);
    assert!(
        outcome(2).1.contains("with your password"),
        "{}",
        outcome(2).1
    );
    assert_eq!(outcome(11).0, Outcome::Passed, "{}", outcome(11).1);
    let version = env!("CARGO_PKG_VERSION");
    assert!(outcome(11).1.contains(version), "{}", outcome(11).1);
    // The probe, in one exec, and doctor once deployed (R03): not installed
    // under /opt/rovik here, so the daemon's step stays as its health says.
    assert_eq!(server.execs.load(ordering), 2, "the probe and doctor");
    assert!(
        server.most_open.load(ordering) >= 1,
        "/health on a streamlocal channel"
    );
}
