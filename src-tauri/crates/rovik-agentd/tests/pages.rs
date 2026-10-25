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
use rovik_protocol::{Request, Response, Secret};
use serde_json::{json, Value};
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

/// A daemon whose agent's commands run in the user's private network; None
/// (the test skipped) where python3, the tests' server, is missing or the
/// daemon can't make the network.
async fn daemon_with_network() -> Option<Daemon> {
    if !rovik_core::process::on_path("python3") {
        eprintln!("python3 isn't installed: skipped");
        return None;
    }
    let daemon = Daemon::start_with(&["--same-machine"], &[("ROVIK_PRIVATE_NETWORK", "1")]).await;
    let log = until("the daemon's word on its network", || {
        let log = daemon.log();
        log.contains("private network").then_some(log)
    })
    .await;
    if !log.contains("the agent's commands run in this user's private network") {
        eprintln!("no private network here: skipped\n{log}");
        return None;
    }
    Some(daemon)
}

/// The app with the chat, its remote connected to the daemon and the chat
/// placed there, and the pages its browser was asked to open.
async fn app_connected(daemon: &Daemon) -> (TestCore, Arc<Remote>, Arc<Mutex<Vec<String>>>) {
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
    (app, remote, opened)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_a_command_opens_opens_in_the_users_browser_through_a_forwarded_port() {
    let Some(daemon) = daemon_with_network().await else {
        return;
    };
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

    let (app, remote, opened) = app_connected(&daemon).await;
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

/// The result of the model's call `id`, once a request carries it.
fn result_of(seen: &Mutex<Vec<Value>>, id: &str) -> Option<String> {
    let requests = seen.lock().unwrap();
    requests.iter().find_map(|body| {
        let messages = body["messages"].as_array()?;
        let result = messages.iter().find(|m| m["tool_call_id"] == id)?;
        Some(result["content"].as_str()?.to_string())
    })
}

/// The first event the app heard that `which` accepts.
fn heard(app: &TestCore, which: impl Fn(&Value) -> bool) -> Option<Value> {
    app.events
        .lock()
        .unwrap()
        .iter()
        .find(|e| which(e))
        .cloned()
}

/// A server the agent leaves running (bash's `background`) outlives the
/// call that started it and the run: the page a later command opens is
/// still served here, through the forwarded port and the tab's, once the
/// run has ended. The app hears what the chat runs there, and once the
/// user stops it, the ports here close.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_left_running_stays_forwarded_after_the_run_until_the_user_stops_it() {
    let Some(daemon) = daemon_with_network().await else {
        return;
    };
    let project = daemon.dir.0.join("project");
    write(&project.join("hello.txt"), "hello from the server\n");
    let port = free_port();
    // What it prints first, its start's result tells: a line the shell
    // prints at once. Python's own comes once http.server has loaded, which
    // on a busy machine can be after a start stops waiting for it.
    let server = format!(
        "printf 'serving on %s\\n' {port}; python3 -u -m http.server {port} --bind 127.0.0.1"
    );
    // The page opens once the server answers, as an agent's would: one
    // opened before is watched as a server still starting, and its port
    // would outlast a Stop that comes before the server was seen.
    let open = format!(
        "until python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port}))' \
         2>/dev/null; do sleep 0.1; done; xdg-open http://localhost:{port}/hello.txt"
    );
    let (url, seen) = model(vec![
        Reply::Call {
            id: "c1",
            name: "bash",
            args: json!({ "command": server, "background": true }),
        },
        Reply::Call {
            id: "c2",
            name: "bash",
            args: json!({ "command": open }),
        },
        Reply::Text("Done."),
    ])
    .await;
    let mut start = start_request(&url, &project, "Run the server and show me.".into());
    if let Some(config) = start.config.as_mut() {
        config.permissions.allow_any_command = true;
    }
    let (app, remote, opened) = app_connected(&daemon).await;
    remote.start(start).await.unwrap();

    let page = until("the page opened", || {
        opened.lock().unwrap().first().cloned()
    })
    .await;
    assert_eq!(page, format!("http://localhost:{port}/hello.txt"));
    until("the run's end", || heard(&app, |e| e["type"] == "runEnded")).await;
    // Its start told the agent how it started, and as what.
    let started = until("the start's result", || result_of(&seen, "c1")).await;
    assert!(
        started.contains("\nrunning in the background as bg"),
        "{started}"
    );
    assert!(
        started.contains(&format!("\nserving on {port}\n")),
        "{started}"
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
    // With the commands and the run over, its page is still served here.
    tokio::time::sleep(Duration::from_secs(1)).await;
    for at in [port, tab_port] {
        let answer = get(at, "/hello.txt").await;
        assert!(answer.ends_with("hello from the server\n"), "{answer}");
    }

    // The app heard what the chat runs, and asks it where the chat runs.
    let told = until("the app told it runs", || {
        heard(&app, |e| {
            e["type"] == "backgroundTasks" && e["tasks"].as_array().is_some_and(|t| !t.is_empty())
        })
    })
    .await;
    assert_eq!(told["dialogId"], CHAT);
    assert_eq!(told["tasks"][0]["command"], json!(server));
    let id = told["tasks"][0]["id"].as_str().unwrap().to_string();
    let request = Request::BackgroundTasks {
        dialog_id: CHAT.into(),
    };
    let Ok(Response::BackgroundTasks(tasks)) = remote.route(CHAT, request).await else {
        panic!("the chat's host didn't list what it runs");
    };
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, [id.as_str()]);

    // The user's Stop, sent where the chat runs.
    let request = Request::BackgroundStop {
        dialog_id: CHAT.into(),
        id,
    };
    let stopped = remote.route(CHAT, request).await;
    assert!(matches!(stopped, Ok(Response::Bool(true))), "{stopped:?}");
    until("the app told it ended", || {
        heard(&app, |e| {
            e["type"] == "backgroundTasks" && e["tasks"].as_array().is_some_and(|t| t.is_empty())
        })
    })
    .await;
    // Its server is gone: the ports here close.
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
