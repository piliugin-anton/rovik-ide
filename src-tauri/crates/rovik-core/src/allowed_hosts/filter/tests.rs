//! The filter against servers on this computer: the test's own upstream
//! reaches each name it knows at a port of 127.0.0.1, as no listed host is
//! reachable from a test.

use super::*;
use rovik_protocol::permissions::Permissions;
use std::collections::HashMap;

/// Reaches each name it knows at a port of 127.0.0.1.
struct Mapped(HashMap<String, u16>);

impl Upstream for Mapped {
    fn open<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Result<Link, Refusal>> {
        Box::pin(async move {
            let Some(local) = self.0.get(host) else {
                return Err(Refusal::Failed(format!(
                    "{host}:{port} can't be reached.\n"
                )));
            };
            crate::net::open_tunnel("127.0.0.1", *local, &Hop::Direct, Duration::from_secs(5))
                .await
                .map_err(Refusal::Failed)
        })
    }
}

/// Goes on through `hop`, as production does through a proxy.
struct Through(Hop);

impl Upstream for Through {
    fn open<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Result<Link, Refusal>> {
        Box::pin(async move {
            crate::net::open_tunnel(host, port, &self.0, Duration::from_secs(5))
                .await
                .map_err(Refusal::Failed)
        })
    }
}

fn mapped(names: &[(&str, u16)]) -> Mapped {
    Mapped(
        names
            .iter()
            .map(|(name, port)| (name.to_string(), *port))
            .collect(),
    )
}

fn list(entries: &[&str]) -> Option<Arc<AllowedHosts>> {
    AllowedHosts::of(&Permissions {
        allow_any_host: false,
        allowed_hosts: entries.iter().map(|e| e.to_string()).collect(),
        ..Permissions::default()
    })
    .map(Arc::new)
}

/// A filter for the commands checking `list` (None: any host) and going
/// on by `upstream`: its port, and what it refused.
async fn filter(
    list: Option<Arc<AllowedHosts>>,
    upstream: impl Upstream + 'static,
) -> (u16, Arc<Denials>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let denials = Arc::new(Denials::default());
    let filter = Arc::new(Filter {
        client: Client::Commands,
        list: Arc::new(move || list.clone()),
        upstream: Arc::new(upstream),
        denials: denials.clone(),
    });
    tokio::spawn(filter.serve(listener));
    (port, denials)
}

/// A server that echoes what it's sent: its port.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// A request head read to its blank line; none once the peer closed.
async fn head_of(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// An HTTP server that answers `body` to one request a connection, then
/// closes: its port, and the heads it got.
async fn origin(body: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                if let Some(head) = head_of(&mut stream).await {
                    seen.lock().unwrap().push(head);
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                }
            });
        }
    });
    (port, heads)
}

/// Sends `request` to the filter at `port`: the connection, and the head
/// of its answer.
async fn ask(port: u16, request: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let head = head_of(&mut stream).await.unwrap_or_default();
    (stream, head)
}

/// The filter's whole answer to `request`, to the close.
async fn answer_to(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer).await;
    String::from_utf8_lossy(&answer).into_owned()
}

/// A `Proxy-Authorization` naming `user`, as curl sends it for
/// `http://user@127.0.0.1:port`.
fn naming(user: &str) -> String {
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:"));
    format!("Proxy-Authorization: Basic {token}\r\n")
}

#[tokio::test]
async fn a_listed_host_is_tunnelled_and_another_is_refused() {
    let echo = echo().await;
    let names = [("docs.rs", echo), ("evil.example", echo)];
    let (port, denials) = filter(list(&["docs.rs"]), mapped(&names)).await;
    let started = Instant::now();
    let (mut tunnel, head) = ask(
        port,
        "CONNECT docs.rs:443 HTTP/1.1\r\nHost: docs.rs:443\r\n\r\n",
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    tunnel.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    tunnel.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");

    let refused = answer_to(
        port,
        &format!(
            "CONNECT evil.example:443 HTTP/1.1\r\n{}\r\n",
            naming("chat-a")
        ),
    )
    .await;
    assert!(refused.starts_with("HTTP/1.1 403 Forbidden"), "{refused}");
    assert!(
        refused.contains("evil.example isn't in the user's allowed hosts (docs.rs)"),
        "{refused}"
    );
    assert_eq!(
        denials.since(started, Client::Commands, Some("chat-a")),
        ["evil.example"]
    );
}

/// A plain HTTP request reaches its host in the host's own form, the
/// proxy's headers left out, one request a connection.
#[tokio::test]
async fn plain_http_reaches_its_host_in_its_own_form() {
    let (origin_port, heads) = origin("hello").await;
    let (port, _) = filter(list(&["docs.rs"]), mapped(&[("docs.rs", origin_port)])).await;
    let answer = answer_to(
        port,
        "GET http://docs.rs/tokio?v=1 HTTP/1.1\r\nHost: docs.rs\r\nProxy-Authorization: Basic eDo=\r\n\
         Proxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n",
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.ends_with("hello"), "{answer}");
    let head = heads.lock().unwrap()[0].clone();
    assert!(head.starts_with("GET /tokio?v=1 HTTP/1.1\r\n"), "{head}");
    for line in [
        "Host: docs.rs\r\n",
        "Accept: */*\r\n",
        "Connection: close\r\n",
    ] {
        assert!(head.contains(line), "{line:?} in {head}");
    }
    assert!(!head.to_ascii_lowercase().contains("proxy-"), "{head}");
    // A request with no Host header gets the URL's.
    answer_to(port, "GET http://docs.rs:8080/ HTTP/1.0\r\n\r\n").await;
    let head = heads.lock().unwrap()[1].clone();
    assert!(
        head.starts_with("GET / HTTP/1.0\r\nHost: docs.rs:8080\r\n"),
        "{head}"
    );
}

#[tokio::test]
async fn this_computers_own_services_are_never_reached_through_the_filter() {
    let echo = echo().await;
    let names = [("localhost", echo), ("127.0.0.1", echo), ("::1", echo)];
    // Not even with any host allowed.
    let (port, _) = filter(None, mapped(&names)).await;
    for target in ["localhost:5432", "127.0.0.1:5432", "[::1]:5432"] {
        let answer = answer_to(port, &format!("CONNECT {target} HTTP/1.1\r\n\r\n")).await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{target}: {answer}");
        assert!(answer.contains("connect to them directly"), "{answer}");
    }
    let answer = answer_to(port, "GET http://localhost:3000/ HTTP/1.1\r\n\r\n").await;
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
}

#[tokio::test]
async fn with_any_host_every_host_passes() {
    let echo = echo().await;
    let (port, denials) = filter(None, mapped(&[("evil.example", echo)])).await;
    let (_, head) = ask(port, "CONNECT evil.example:443 HTTP/1.1\r\n\r\n").await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(denials
        .since(
            Instant::now() - Duration::from_secs(60),
            Client::Commands,
            None
        )
        .is_empty());
}

#[tokio::test]
async fn what_a_proxy_doesnt_take_is_a_bad_request() {
    let (port, _) = filter(list(&["docs.rs"]), mapped(&[])).await;
    for request in [
        "GET / HTTP/1.1\r\nHost: docs.rs\r\n\r\n",
        "GET https://docs.rs/ HTTP/1.1\r\n\r\n",
        "CONNECT docs.rs HTTP/1.1\r\n\r\n",
        "hello\r\n\r\n",
    ] {
        let answer = answer_to(port, request).await;
        assert!(answer.starts_with("HTTP/1.1 400"), "{request:?}: {answer}");
    }
}

#[tokio::test]
async fn a_listed_host_that_cant_be_reached_is_a_bad_gateway() {
    let (port, _) = filter(list(&["docs.rs"]), mapped(&[])).await;
    let answer = answer_to(port, "CONNECT docs.rs:443 HTTP/1.1\r\n\r\n").await;
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
    assert!(answer.contains("docs.rs:443 can't be reached"), "{answer}");
}

/// Through an upstream HTTP proxy, a plain request is a tunnel to the host
/// it was checked for too: the proxy is asked to CONNECT there, never
/// handed the request to send where it says.
#[tokio::test]
async fn through_a_proxy_each_request_is_a_tunnel_to_its_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let connect = head_of(&mut stream).await.unwrap();
        seen.lock().unwrap().push(connect);
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let request = head_of(&mut stream).await.unwrap();
        seen.lock().unwrap().push(request);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await;
    });
    let hop = Hop::Http {
        host: "127.0.0.1".into(),
        port: proxy_port,
        auth: None,
    };
    let (port, _) = filter(list(&["docs.rs"]), Through(hop)).await;
    let answer = answer_to(
        port,
        "GET http://docs.rs/x HTTP/1.1\r\nHost: docs.rs\r\n\r\n",
    )
    .await;
    assert!(answer.ends_with("ok"), "{answer}");
    let heads = heads.lock().unwrap().clone();
    assert!(
        heads[0].starts_with("CONNECT docs.rs:80 HTTP/1.1\r\n"),
        "{heads:?}"
    );
    assert!(heads[1].starts_with("GET /x HTTP/1.1\r\n"), "{heads:?}");
}

#[test]
fn refusals_are_told_to_their_own_chat() {
    let denials = Denials::default();
    let started = Instant::now();
    denials.add("a.example", Client::Commands, Some("chat-a".into()));
    denials.add("b.example", Client::Commands, Some("chat-b".into()));
    denials.add("anyone.example", Client::Commands, None);
    denials.add("a.example", Client::Commands, Some("chat-a".into()));
    denials.add("page.example", Client::Browser, None);
    assert_eq!(
        denials.since(started, Client::Commands, Some("chat-a")),
        ["a.example", "anyone.example"]
    );
    assert_eq!(
        denials.since(started, Client::Browser, None),
        ["page.example"]
    );
    let later = Instant::now();
    assert!(denials.since(later, Client::Commands, None).is_empty());
}

#[test]
fn the_proxys_user_is_read_from_its_sign_in() {
    let token = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let user = |value: String| proxy_user(value.as_bytes());
    assert_eq!(
        user(format!("Basic {}", token("chat-a:"))).as_deref(),
        Some("chat-a")
    );
    assert_eq!(
        user(format!("basic {}", token("a%3Ab:pw"))).as_deref(),
        Some("a:b")
    );
    assert_eq!(user(format!("Bearer {}", token("chat-a:"))), None);
    assert_eq!(user(format!("Basic {}", token(":pw"))), None);
    assert_eq!(user("Basic !!!".into()), None);
}

/// A listed name is looked up here and refused when it leads to this
/// computer or a link-local address (a cloud's metadata service).
#[tokio::test]
async fn a_listed_name_leading_to_this_computer_is_refused() {
    let echo = echo().await;
    let refused = open_direct("localhost", echo, Duration::from_secs(5)).await;
    assert!(
        matches!(&refused, Err(Refusal::Denied(why)) if why.contains("leads to")),
        "{:?}",
        refused.map(|_| ())
    );
    for ip in [
        "127.0.0.1",
        "127.9.9.9",
        "::1",
        "0.0.0.0",
        "::",
        "169.254.169.254",
        "fe80::1",
    ] {
        assert!(!reachable(ip.parse().unwrap()), "{ip}");
    }
    assert!(!reachable("::ffff:127.0.0.1".parse().unwrap()));
    for ip in [
        "10.0.0.5",
        "192.168.1.1",
        "93.184.215.14",
        "2606:4700::1111",
    ] {
        assert!(reachable(ip.parse().unwrap()), "{ip}");
    }
}

/// In a sealed network the filter, listening inside, is a command's only
/// way out (allowed-hosts-plan.md, J04): a listed host is reached through
/// it, one off the list refused; an address isn't reached directly, and no
/// name is looked up.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn in_a_sealed_network_the_filter_is_a_commands_only_way_out() {
    let Some((_dir, network)) = crate::overlay::confine::network::sealed_for_test() else {
        return;
    };
    if !crate::process::on_path("curl") {
        eprintln!("curl isn't installed: skipped");
        return;
    }
    let (origin, _) = origin("listed, through the filter").await;
    let inside = network.listen().unwrap();
    inside.set_nonblocking(true).unwrap();
    let inside = TcpListener::from_std(inside).unwrap();
    let port = inside.local_addr().unwrap().port();
    let filter = Arc::new(Filter {
        client: Client::Commands,
        list: Arc::new(|| list(&["listed.test"])),
        upstream: Arc::new(mapped(&[("listed.test", origin)])),
        denials: Arc::new(Denials::default()),
    });
    tokio::spawn(filter.serve(inside));
    let proxy = format!("http://127.0.0.1:{port}");
    let script = "curl -sS --max-time 5 http://listed.test/; echo \" [$?]\"
        curl -sS --max-time 5 -o /dev/null -w '%{http_code}' http://unlisted.test/; echo \" [$?]\"
        curl -sS --max-time 5 --noproxy '*' http://192.0.2.1/; echo \"direct [$?]\"
        getent hosts example.com; echo \"lookup [$?]\"";
    let out = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", script])
            .env("http_proxy", &proxy)
            .env("HTTPS_PROXY", &proxy);
        network.join(&mut cmd);
        cmd.output().unwrap()
    })
    .await
    .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines,
        [
            "listed, through the filter [0]",
            "403 [0]",
            "direct [7]",
            "lookup [2]"
        ],
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
