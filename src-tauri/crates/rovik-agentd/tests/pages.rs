//! The pages a command opens on a team server (D20), against the real
//! daemon and the app's remote: a command in the user's private network
//! starts a server there and opens its page; the app forwards the same port
//! of this computer to it and opens the page, and what the browser asks
//! reaches the server there, through that port and through the port a
//! browser tab of the app shows it through. The command hears that it
//! opened, and once its server is gone the ports here close. Skipped where
//! the daemon can't make a private network.

mod common;

use common::{start_request, until, write, Daemon, COMPUTER};
use rovik_client::{ForwardedPort, HostState, Identity, Remote, UnixConnector};
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::testing::{model, Reply, TestCore};
use rovik_protocol::Secret;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const HOST: &str = "server";
const CHAT: &str = "chat-1";

/// A port nothing listens on here now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// The answer to `GET path` at `port` here, once one comes: the server may
/// still be starting.
async fn get(port: u16, path: &str) -> String {
    for _ in 0..100 {
        let asked = async {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
            let request = format!("GET {path} HTTP/1.0\r\nHost: localhost:{port}\r\n\r\n");
            stream.write_all(request.as_bytes()).await.ok()?;
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.ok()?;
            let ok = ["HTTP/1.0 200", "HTTP/1.1 200"];
            ok.iter().any(|s| answer.starts_with(s)).then_some(answer)
        };
        if let Ok(Some(answer)) = tokio::time::timeout(Duration::from_secs(5), asked).await {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("port {port} never answered");
}

/// The command's output, as the model got it, once it has.
fn command_result(seen: &Mutex<Vec<serde_json::Value>>) -> Option<String> {
    let requests = seen.lock().unwrap();
    requests.iter().find_map(|body| {
        let messages = body["messages"].as_array()?;
        let result = messages.iter().find(|m| m["role"] == "tool")?;
        Some(result["content"].to_string())
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_a_command_opens_opens_in_the_users_browser_through_a_forwarded_port() {
    if !rovik_core::process::on_path("python3") {
        eprintln!("python3 isn't installed: skipped");
        return;
    }
    let daemon = Daemon::start_with(&["--same-machine"], &[("ROVIK_PRIVATE_NETWORK", "1")]).await;
    let log = until("the daemon's word on its network", || {
        let log = daemon.log();
        log.contains("private network").then_some(log)
    })
    .await;
    if !log.contains("the agent's commands run in this user's private network") {
        eprintln!("no private network here: skipped\n{log}");
        return;
    }
    let project = daemon.dir.0.join("project");
    write(&project.join("hello.txt"), "hello from the server\n");
    let port = free_port();
    // The server runs until the test is done with it, then goes with the
    // command.
    let command = format!(
        "python3 -m http.server {port} --bind 127.0.0.1 >/dev/null 2>&1 & server=$!; sleep 1; \
         xdg-open http://localhost:{port}/hello.txt; \
         for i in $(seq 600); do [ -e done ] && break; sleep 0.1; done; kill $server"
    );
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": command }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut start = start_request(&url, &project, "Show me the page.".into());
    if let Some(config) = start.config.as_mut() {
        config.permissions.allow_any_command = true;
    }

    let app = TestCore::new();
    let dialog = NewDialog {
        id: CHAT.into(),
        title: "Chat".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
    };
    app.core
        .db
        .write(move |c| store::create_dialog(c, &dialog))
        .await
        .unwrap();
    let remote = Remote::new(app.core.clone());
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let browser = opened.clone();
    remote.on_open_page(Arc::new(move |page| {
        browser.lock().unwrap().push(page.address.to_string());
        Ok(())
    }));
    let identity = Identity {
        host: HOST.into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "pages tests".into(),
    };
    remote.connect(HOST, UnixConnector(daemon.socket.clone()), identity);
    remote.place(CHAT, HOST);
    until("connected", || {
        (remote.states() == [(HOST.to_string(), HostState::Ready)]).then_some(())
    })
    .await;
    remote.start(start).await.unwrap();

    let mut page = None;
    for _ in 0..600 {
        page = opened.lock().unwrap().first().cloned();
        if page.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let Some(page) = page else {
        // What the command was told, for why.
        std::fs::write(project.join("done"), "").unwrap();
        let mut told = None;
        for _ in 0..100 {
            told = command_result(&seen);
            if told.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "the page never opened: {told:?}\n{}\n{}\nmodel requests: {}\nevents: {:?}",
            daemon.output(),
            daemon.log(),
            seen.lock().unwrap().len(),
            app.events.lock().unwrap()
        );
    };
    assert_eq!(
        page,
        format!("http://localhost:{port}/hello.txt"),
        "its own port, free here"
    );
    let forwarded = remote.forwarded_ports(CHAT);
    let [ForwardedPort {
        port: there,
        local_port,
        tab_port: Some(tab_port),
    }] = forwarded[..]
    else {
        panic!("one port forwarded, with a tab port: {forwarded:?}");
    };
    assert_eq!((there, local_port), (port, port));
    // What the browser asks reaches the server in the user's network there,
    // and so does what a browser tab of the app asks through its own port.
    for at in [port, tab_port] {
        let answer = get(at, "/hello.txt").await;
        assert!(answer.ends_with("hello from the server\n"), "{answer}");
    }

    // The command goes on, and hears its page opened.
    std::fs::write(project.join("done"), "").unwrap();
    let result = until("the command's result", || command_result(&seen)).await;
    assert!(
        result.contains(&format!(
            "Rovik opened http://localhost:{port}/hello.txt on the user's computer"
        )),
        "{result}\n{}",
        daemon.output()
    );
    // Its server is gone: the port here closes.
    until("the port closed", || {
        remote.forwarded_ports(CHAT).is_empty().then_some(())
    })
    .await;
    for at in [port, tab_port] {
        until("nothing listens here", || {
            std::net::TcpStream::connect(("127.0.0.1", at))
                .is_err()
                .then_some(())
        })
        .await;
    }
}
