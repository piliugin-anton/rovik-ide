//! The pages a remote chat's commands open (D20, remote/pages.rs): a port
//! here forwarded to the page's own port on the host, the page opened in
//! the browser at it, and the port shown for the chat until its server
//! there is gone or the chat or the host goes.

use super::*;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

/// The browser the remote opens pages in, and the ports it tells the
/// frontend of: each chat's latest.
#[derive(Clone, Default)]
struct Browser {
    opened: Arc<Mutex<Vec<String>>>,
    ports: Arc<Mutex<HashMap<String, Vec<ForwardedPort>>>>,
}

impl Browser {
    fn of(remote: &Remote) -> Browser {
        let browser = Browser::default();
        let opened = browser.opened.clone();
        remote.on_open_page(Arc::new(move |url| {
            opened.lock().unwrap().push(url.to_string());
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

    /// The next page opened, once one is.
    async fn next_page(&self) -> String {
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
    assert_eq!(browser.next_page().await, url, "at its own port, free here");
    assert_eq!(answers(&fake), vec![opened("page-1", port)]);
    let forwarded = vec![ForwardedPort {
        port,
        local_port: port,
    }];
    assert_eq!(remote.forwarded_ports("chat"), forwarded);
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

    // Its server is gone there: the port here goes too.
    remote.receive(HOST, page_closed("chat", port)).await;
    assert_eq!(remote.forwarded_ports("chat"), vec![]);
    assert_eq!(browser.told("chat"), Some(vec![]));
    until_closed(port).await;
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
