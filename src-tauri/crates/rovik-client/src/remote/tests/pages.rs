//! The pages a remote chat's commands open (D20, remote/pages.rs): a port
//! here forwarded to the page's own port on the host, the page opened in
//! the browser at it, and the port shown for the chat until its server
//! there is gone or the chat or the host goes.

use super::*;
use crate::remote::tab_port::FRAME_COOKIES;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use hyper::header::HOST as HOST_HEADER;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

fn page_wanted(dialog_id: &str, id: &str, url: &str, port: u16) -> Incoming {
    Incoming::Event {
        seq: 1,
        event: Event::PageWanted {
            dialog_id: dialog_id.into(),
            id: id.into(),
            url: url.into(),
            port,
        },
    }
}

fn page_closed(dialog_id: &str, port: u16) -> Incoming {
    Incoming::Event {
        seq: 1,
        event: Event::PageClosed {
            dialog_id: dialog_id.into(),
            port,
        },
    }
}

/// A page the remote opened: its address here, and whether a browser tab
/// of the app could show it.
#[derive(Clone, Debug, PartialEq)]
struct Opened {
    address: String,
    in_tab: bool,
}

/// The browser the remote opens pages in, and the ports it tells the
/// frontend of: each chat's latest.
#[derive(Clone, Default)]
struct Browser {
    opened: Arc<Mutex<Vec<Opened>>>,
    ports: Arc<Mutex<HashMap<String, Vec<ForwardedPort>>>>,
}

impl Browser {
    fn of(remote: &Remote) -> Browser {
        let browser = Browser::default();
        let opened = browser.opened.clone();
        remote.on_open_page(Arc::new(move |page| {
            opened.lock().unwrap().push(Opened {
                address: page.address.to_string(),
                in_tab: page.in_tab,
            });
            Ok(())
        }));
        let told = browser.ports.clone();
        remote.on_cloud(Arc::new(move |event| {
            if let CloudEvent::Ports { dialog_id, ports } = event {
                told.lock().unwrap().insert(dialog_id, ports);
            }
        }));
        browser
    }

    /// The next page opened, once one is: its address.
    async fn next_page(&self) -> String {
        self.next_opened().await.address
    }

    /// The next page opened, once one is.
    async fn next_opened(&self) -> Opened {
        until(|| !self.opened.lock().unwrap().is_empty()).await;
        self.opened.lock().unwrap().remove(0)
    }

    /// The chat's ports as the frontend was last told them.
    fn told(&self, chat: &str) -> Option<Vec<ForwardedPort>> {
        self.ports.lock().unwrap().get(chat).cloned()
    }
}

/// Waits until `done`, for a few seconds at most.
async fn until(done: impl Fn() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("it never happened");
}

/// A server of the tests' own on this computer, in place of one in the
/// user's private network on the host: it sends back what it gets.
async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    address
}

/// A port nothing listens on here now.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// What comes back for `bytes` sent to `port` here.
async fn echoed(port: u16, bytes: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    stream.write_all(bytes).await.unwrap();
    let mut back = vec![0; bytes.len()];
    let read = stream.read_exact(&mut back);
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("an answer in time")
        .unwrap();
    back
}

/// Waits until nothing listens on `port` here any more.
async fn until_closed(port: u16) {
    for _ in 0..500 {
        if TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("port {port} is still forwarded");
}

/// The `PageOpened` answers the fake had.
fn answers(fake: &Fake) -> Vec<Request> {
    let requests = fake.requests.lock().unwrap();
    requests
        .iter()
        .filter(|r| matches!(r, Request::PageOpened { .. }))
        .cloned()
        .collect()
}

fn opened(id: &str, local_port: u16) -> Request {
    Request::PageOpened {
        id: id.into(),
        local_port: Some(local_port),
        error: None,
    }
}

/// A remote of `app` connected to `fake`, with these chats on the host,
/// and the browser it opens pages in.
async fn with_pages(app: &App, fake: Fake, chats: &[&str]) -> (Arc<Remote>, Fake, Browser) {
    for chat in chats {
        app.chat(chat, None, Vec::new()).await;
    }
    let (remote, fake) = connected_to(app, fake).await;
    for chat in chats {
        remote.place(chat, HOST);
    }
    let browser = Browser::of(&remote);
    (remote, fake, browser)
}

#[tokio::test]
async fn a_page_is_forwarded_from_its_own_port_here_opened_and_closed_with_its_server() {
    let app = App::new();
    let fake = Fake::default();
    let port = free_port();
    let server = echo_server().await;
    fake.network.lock().unwrap().insert(port, server);
    let (remote, fake, browser) = with_pages(&app, fake, &["chat"]).await;

    let url = format!("http://localhost:{port}/app?x=1");
    remote
        .receive(HOST, page_wanted("chat", "page-1", &url, port))
        .await;
    assert_eq!(
        browser.next_opened().await,
        Opened {
            address: url,
            in_tab: true
        },
        "at its own port, free here, and in a tab: an http page"
    );
    assert_eq!(answers(&fake), vec![opened("page-1", port)]);
    let forwarded = remote.forwarded_ports("chat");
    let [ForwardedPort {
        port: there,
        local_port,
        tab_port: Some(tab_port),
    }] = forwarded[..]
    else {
        panic!("one port forwarded, with a tab port: {forwarded:?}");
    };
    assert_eq!((there, local_port), (port, port));
    assert_ne!(tab_port, port);
    assert_eq!(browser.told("chat"), Some(forwarded.clone()));
    assert_eq!(
        remote.all_forwarded_ports(),
        HashMap::from([("chat".to_string(), forwarded)])
    );
    // What the browser sends goes to the server there, and back.
    assert_eq!(
        echoed(port, b"GET / HTTP/1.1\r\n").await,
        b"GET / HTTP/1.1\r\n"
    );

    // Its server is gone there: the ports here go too.
    remote.receive(HOST, page_closed("chat", port)).await;
    assert_eq!(remote.forwarded_ports("chat"), vec![]);
    assert_eq!(browser.told("chat"), Some(vec![]));
    until_closed(port).await;
    until_closed(tab_port).await;
}

#[tokio::test]
async fn a_page_whose_port_is_taken_here_is_forwarded_from_another() {
    let app = App::new();
    let fake = Fake::default();
    // The tests' server holds the page's port on this computer.
    let server = echo_server().await;
    let port = server.port();
    fake.network.lock().unwrap().insert(port, server);
    let (remote, fake, browser) = with_pages(&app, fake, &["chat"]).await;

    let url = format!("http://127.0.0.1:{port}/");
    remote
        .receive(HOST, page_wanted("chat", "page-1", &url, port))
        .await;
    let page = browser.next_page().await;
    let [forward] = remote.forwarded_ports("chat")[..] else {
        panic!("one port forwarded");
    };
    assert_eq!(
        forward.port, port,
        "the server's port, as the chat shows it"
    );
    assert_ne!(forward.local_port, port);
    assert_eq!(page, format!("http://127.0.0.1:{}/", forward.local_port));
    assert_eq!(answers(&fake), vec![opened("page-1", forward.local_port)]);
    assert_eq!(echoed(forward.local_port, b"ping").await, b"ping");
}

#[tokio::test]
async fn a_second_page_of_a_forwarded_port_opens_through_it() {
    let app = App::new();
    let fake = Fake::default();
    let port = free_port();
    let server = echo_server().await;
    fake.network.lock().unwrap().insert(port, server);
    let (remote, fake, browser) = with_pages(&app, fake, &["a", "b"]).await;

    let first = format!("http://localhost:{port}/");
    remote
        .receive(HOST, page_wanted("a", "page-1", &first, port))
        .await;
    browser.next_page().await;
    let second = format!("http://localhost:{port}/login");
    remote
        .receive(HOST, page_wanted("b", "page-2", &second, port))
        .await;
    assert_eq!(browser.next_page().await, second);
    assert_eq!(
        answers(&fake),
        vec![opened("page-1", port), opened("page-2", port)]
    );
    // Forwarded for both: one's server gone for it, it stays for the other.
    remote.receive(HOST, page_closed("a", port)).await;
    assert_eq!(remote.forwarded_ports("a"), vec![]);
    assert_eq!(echoed(port, b"still").await, b"still");
    remote.receive(HOST, page_closed("b", port)).await;
    until_closed(port).await;
}

#[tokio::test]
async fn a_page_its_command_no_longer_waits_for_isnt_opened_or_forwarded() {
    let app = App::new();
    let fake = Fake::default();
    fake.unwanted_pages.store(true, Ordering::Relaxed);
    let port = free_port();
    let server = echo_server().await;
    fake.network.lock().unwrap().insert(port, server);
    let (remote, fake, browser) = with_pages(&app, fake, &["chat"]).await;

    let url = format!("http://localhost:{port}/");
    remote
        .receive(HOST, page_wanted("chat", "page-1", &url, port))
        .await;
    until(|| !answers(&fake).is_empty()).await;
    assert_eq!(answers(&fake), vec![opened("page-1", port)]);
    until_closed(port).await;
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(remote.forwarded_ports("chat"), vec![]);
    assert_eq!(browser.told("chat"), None, "no port was shown for it");
}

/// Whatever the host asks, only a page of the user's own servers, on the
/// port named, is forwarded and opened: the host is told why not, in
/// words for its agent. A chat that isn't this host's gets no answer.
#[tokio::test]
async fn only_a_page_of_the_users_own_servers_on_its_port_opens() {
    let app = App::new();
    let (remote, fake, browser) = with_pages(&app, Fake::default(), &["chat"]).await;

    remote
        .receive(
            HOST,
            page_wanted("stranger", "page-0", "http://localhost:3000/", 3000),
        )
        .await;
    remote
        .receive(
            HOST,
            page_wanted("chat", "page-1", "https://example.com/login", 443),
        )
        .await;
    until(|| !answers(&fake).is_empty()).await;
    remote
        .receive(
            HOST,
            page_wanted("chat", "page-2", "http://localhost:3000/", 4000),
        )
        .await;
    until(|| answers(&fake).len() == 2).await;
    let answered = answers(&fake);
    let why = |id: &str| {
        answered.iter().find_map(|r| match r {
            Request::PageOpened {
                id: i,
                local_port: None,
                error: Some(why),
            } if i == id => Some(why.clone()),
            _ => None,
        })
    };
    let remote_page = why("page-1").expect("refused");
    assert!(
        remote_page.contains("refused to open https://example.com/login"),
        "{remote_page}"
    );
    let other_port = why("page-2").expect("refused");
    assert!(
        other_port.contains("port 4000") && other_port.contains("port 3000"),
        "{other_port}"
    );
    assert!(why("page-0").is_none(), "{answered:?}");
    assert!(browser.opened.lock().unwrap().is_empty());
    assert!(remote.all_forwarded_ports().is_empty());
}

#[tokio::test]
async fn a_chats_pages_stop_being_forwarded_once_it_is_deleted() {
    let app = App::new();
    let fake = Fake::default();
    let (one, two) = (free_port(), free_port());
    let server = echo_server().await;
    fake.network
        .lock()
        .unwrap()
        .extend([(one, server), (two, server)]);
    let (remote, _fake, browser) = with_pages(&app, fake, &["gone", "kept"]).await;
    for (chat, port) in [("gone", one), ("kept", two)] {
        let url = format!("http://localhost:{port}/");
        remote
            .receive(HOST, page_wanted(chat, chat, &url, port))
            .await;
        browser.next_page().await;
    }

    remote.delete("gone").await.unwrap();
    assert_eq!(browser.told("gone"), Some(vec![]));
    until_closed(one).await;
    assert_eq!(echoed(two, b"kept").await, b"kept");
}

#[tokio::test]
async fn a_hosts_pages_stop_being_forwarded_once_it_missed_events_or_goes() {
    let app = App::new();
    let fake = Fake::default();
    let (one, two) = (free_port(), free_port());
    let server = echo_server().await;
    fake.network
        .lock()
        .unwrap()
        .extend([(one, server), (two, server)]);
    let (remote, _fake, browser) = with_pages(&app, fake, &["chat"]).await;
    let open = |port: u16| page_wanted("chat", "page", &format!("http://localhost:{port}/"), port);

    remote.receive(HOST, open(one)).await;
    browser.next_page().await;
    // Back with every event since: a `PageClosed` would have come.
    remote
        .receive(HOST, Incoming::Subscribed { complete: true })
        .await;
    assert_eq!(echoed(one, b"here").await, b"here");
    // Back having missed some: one may have been a `PageClosed`.
    remote
        .receive(HOST, Incoming::Subscribed { complete: false })
        .await;
    assert_eq!(browser.told("chat"), Some(vec![]));
    until_closed(one).await;

    remote.receive(HOST, open(two)).await;
    browser.next_page().await;
    remote.disconnect(HOST);
    assert_eq!(remote.forwarded_ports("chat"), vec![]);
    until_closed(two).await;
}

/// What a server says to keep its pages out of frames, and its session's
/// cookie, as Django, Rails and helmet do by default.
const KEPT_OUT: &[(&str, &str)] = &[
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "default-src 'self'; frame-ancestors 'none'",
    ),
    ("set-cookie", "sessionid=abc; HttpOnly; SameSite=Lax"),
];

/// A web server of the tests' own on this computer, in place of a dev
/// server in the user's private network on the host: it answers each
/// request with `headers` and its path for a body, keeping connections
/// open.
async fn web_server(headers: &'static [(&'static str, &'static str)]) -> SocketAddr {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| async move {
                    let mut response = hyper::Response::builder();
                    for (name, value) in headers {
                        response = response.header(*name, *value);
                    }
                    let path = Bytes::from(request.uri().path().to_string());
                    Ok::<_, std::convert::Infallible>(response.body(Full::new(path)).unwrap())
                },
            );
            let serving = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service);
            tokio::spawn(serving);
        }
    });
    address
}

/// A browser's connection to `port` here.
async fn browser_connection(port: u16) -> SendRequest<Empty<Bytes>> {
    let stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection.with_upgrades());
    sender
}

/// The answer to `request` over `connection`, once it came.
async fn answer(
    connection: &mut SendRequest<Empty<Bytes>>,
    request: hyper::Request<Empty<Bytes>>,
) -> hyper::Response<hyper::body::Incoming> {
    connection.ready().await.unwrap();
    let answering = connection.send_request(request);
    tokio::time::timeout(Duration::from_secs(5), answering)
        .await
        .expect("an answer in time")
        .unwrap()
}

/// `GET path` over `connection`: the answer's head, and its body.
async fn fetch(
    connection: &mut SendRequest<Empty<Bytes>>,
    path: &str,
) -> (hyper::http::response::Parts, String) {
    let request = hyper::Request::get(path)
        .header(HOST_HEADER, "localhost")
        .body(Empty::new())
        .unwrap();
    let (head, body) = answer(connection, request).await.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    (head, String::from_utf8(body.to_vec()).unwrap())
}

/// A remote whose chat "chat" opened the page `url` of a server on `port`
/// there, which `server` stands in for (none: nothing listens there): the
/// page as it opened, and its port as forwarded.
async fn page_opened(
    app: &App,
    url: &str,
    port: u16,
    server: Option<SocketAddr>,
) -> (Arc<Remote>, Opened, ForwardedPort) {
    let fake = Fake::default();
    if let Some(server) = server {
        fake.network.lock().unwrap().insert(port, server);
    }
    let (remote, _fake, browser) = with_pages(app, fake, &["chat"]).await;
    remote
        .receive(HOST, page_wanted("chat", "page-1", url, port))
        .await;
    let page = browser.next_opened().await;
    let [forward] = remote.forwarded_ports("chat")[..] else {
        panic!("one port forwarded");
    };
    (remote, page, forward)
}

#[tokio::test]
async fn a_browser_tab_shows_a_page_its_server_keeps_out_of_frames_and_the_browser_gets_it_as_sent()
{
    let app = App::new();
    let port = free_port();
    let server = web_server(KEPT_OUT).await;
    let url = format!("http://localhost:{port}/admin/");
    let (_remote, page, forward) = page_opened(&app, &url, port, Some(server)).await;
    assert!(page.in_tab, "an http page opens in a tab");
    let tab_port = forward.tab_port.expect("a tab port");

    let mut tab = browser_connection(tab_port).await;
    let (head, body) = fetch(&mut tab, "/admin/").await;
    assert_eq!((head.status.as_u16(), body.as_str()), (200, "/admin/"));
    assert!(head.headers.get("x-frame-options").is_none());
    assert_eq!(
        head.headers["content-security-policy"],
        "default-src 'self'"
    );
    let cookie = match FRAME_COOKIES {
        true => "sessionid=abc; HttpOnly; SameSite=None; Secure; Partitioned",
        false => "sessionid=abc; HttpOnly; SameSite=Lax",
    };
    assert_eq!(head.headers["set-cookie"], cookie);
    // The tab's next request goes over the same connection.
    let (_, body) = fetch(&mut tab, "/admin/users").await;
    assert_eq!(body, "/admin/users");

    // The default browser's port carries the server's answer as it was.
    let mut browser = browser_connection(forward.local_port).await;
    let (head, _) = fetch(&mut browser, "/admin/").await;
    for (name, value) in KEPT_OUT {
        assert_eq!(head.headers[*name], *value);
    }
}

/// A WebSocket server of the tests' own, as a dev server's hot reload is
/// one (Vite's, webpack's): it sends back each message it gets.
async fn websocket_server() -> SocketAddr {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                while let Some(Ok(message)) = socket.next().await {
                    if message.is_text() && socket.send(message).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    address
}

#[tokio::test]
async fn a_websocket_as_a_dev_servers_hot_reload_opens_goes_through_a_tab_port() {
    let app = App::new();
    let port = free_port();
    let server = websocket_server().await;
    let url = format!("http://localhost:{port}/");
    let (_remote, _page, forward) = page_opened(&app, &url, port, Some(server)).await;

    let tab_port = forward.tab_port.unwrap();
    let stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, tab_port))
        .await
        .unwrap();
    let address = format!("ws://localhost:{tab_port}/hmr");
    let (mut socket, switched) = tokio_tungstenite::client_async(address, stream)
        .await
        .expect("the WebSocket handshake, its accept key checked");
    assert_eq!(switched.status(), 101);
    for update in ["connected", "update /src/App.tsx"] {
        socket.send(Message::Text(update.into())).await.unwrap();
        let back = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an echo in time")
            .unwrap()
            .unwrap();
        assert_eq!(back, Message::Text(update.into()));
    }
}

#[tokio::test]
async fn an_https_page_opens_in_the_default_browser_without_a_tab_port() {
    let app = App::new();
    let port = free_port();
    let server = echo_server().await;
    let url = format!("https://localhost:{port}/");
    let (_remote, page, forward) = page_opened(&app, &url, port, Some(server)).await;
    assert_eq!(
        page,
        Opened {
            address: url,
            in_tab: false
        }
    );
    assert_eq!(forward.tab_port, None);
}

#[tokio::test]
async fn a_tab_port_whose_server_is_gone_there_says_why() {
    let app = App::new();
    let port = free_port();
    let url = format!("http://localhost:{port}/");
    let (_remote, _page, forward) = page_opened(&app, &url, port, None).await;

    let mut tab = browser_connection(forward.tab_port.unwrap()).await;
    let (head, body) = fetch(&mut tab, "/").await;
    assert_eq!(head.status.as_u16(), 502);
    assert!(
        body.starts_with("Rovik couldn't show this page: ")
            && body.contains(&format!("Nothing listens on port {port}")),
        "{body}"
    );
}
