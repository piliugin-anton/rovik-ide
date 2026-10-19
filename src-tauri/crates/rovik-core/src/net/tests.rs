//! Settings → Network: the settings and their file, the shared client's
//! connections, and connections through proxies, against servers and
//! proxies made here. Each test has a `Net` of its own, not the process's.

use super::tunnel::{self, Hop};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn proxy(kind: ProxyKind, host: &str, port: u16, username: Option<&str>) -> CustomProxy {
    CustomProxy {
        kind,
        host: host.into(),
        port,
        username: username.map(String::from),
    }
}

fn custom(proxy: CustomProxy, cloud_hosts: bool) -> NetworkSettings {
    NetworkSettings {
        proxy: Some(ProxyMode::Custom),
        custom: Some(proxy),
        cloud_hosts,
        ..NetworkSettings::default()
    }
}

fn secret(text: &str) -> Secret {
    Secret(text.into())
}

/// A request's head, read to its blank line; none once the peer closed.
async fn read_head(stream: &mut TcpStream) -> Option<String> {
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

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// An HTTP/1.1 server that keeps connections open and answers `body` to
/// every request: its port, and the connections it took.
async fn server(body: &'static str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = connections.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                while read_head(&mut stream).await.is_some() {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if stream.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, connections)
}

/// An HTTP proxy that answers every request itself with `body`: its port,
/// and the request heads it got.
async fn http_proxy(body: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                while let Some(head) = read_head(&mut stream).await {
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

/// A proxy that answers CONNECT with `answer`, in one write with the
/// host's first bytes `first`, then echoes what it's sent: its port, and
/// the heads it got.
async fn connect_proxy(
    answer: &'static str,
    first: &'static str,
) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let Some(head) = read_head(&mut stream).await else {
                    return;
                };
                seen.lock().unwrap().push(head);
                let _ = stream
                    .write_all(format!("{answer}{first}").as_bytes())
                    .await;
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, heads)
}

/// What a SOCKS5 proxy here was asked: who signed in, and where to.
#[derive(Default)]
struct Asked {
    users: Vec<(String, String)>,
    targets: Vec<String>,
}

/// A SOCKS5 proxy taking `user` and `password` (none: no sign-in), that
/// connects every CONNECT to 127.0.0.1:`to` whatever it names, or answers
/// `refuse` instead.
async fn socks5_proxy(
    user: Option<(&'static str, &'static str)>,
    to: u16,
    refuse: Option<u8>,
) -> (u16, Arc<Mutex<Asked>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(Mutex::new(Asked::default()));
    let noted = asked.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let noted = noted.clone();
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                stream.read_exact(&mut head).await.unwrap();
                let mut methods = vec![0u8; usize::from(head[1])];
                stream.read_exact(&mut methods).await.unwrap();
                match user {
                    Some((name, password)) if methods.contains(&2) => {
                        stream.write_all(&[5, 2]).await.unwrap();
                        let mut version_len = [0u8; 2];
                        stream.read_exact(&mut version_len).await.unwrap();
                        let mut got_name = vec![0u8; usize::from(version_len[1])];
                        stream.read_exact(&mut got_name).await.unwrap();
                        let mut len = [0u8; 1];
                        stream.read_exact(&mut len).await.unwrap();
                        let mut got_password = vec![0u8; usize::from(len[0])];
                        stream.read_exact(&mut got_password).await.unwrap();
                        let got = (
                            String::from_utf8(got_name).unwrap(),
                            String::from_utf8(got_password).unwrap(),
                        );
                        let ok = got.0 == name && got.1 == password;
                        noted.lock().unwrap().users.push(got);
                        stream
                            .write_all(&[1, if ok { 0 } else { 1 }])
                            .await
                            .unwrap();
                        if !ok {
                            return;
                        }
                    }
                    Some(_) => {
                        let _ = stream.write_all(&[5, 0xFF]).await;
                        return;
                    }
                    None => stream.write_all(&[5, 0]).await.unwrap(),
                }
                let mut request = [0u8; 4];
                stream.read_exact(&mut request).await.unwrap();
                let target = match request[3] {
                    3 => {
                        let mut len = [0u8; 1];
                        stream.read_exact(&mut len).await.unwrap();
                        let mut name = vec![0u8; usize::from(len[0])];
                        stream.read_exact(&mut name).await.unwrap();
                        String::from_utf8(name).unwrap()
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        stream.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    _ => {
                        let mut ip = [0u8; 16];
                        stream.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv6Addr::from(ip).to_string()
                    }
                };
                let mut target_port = [0u8; 2];
                stream.read_exact(&mut target_port).await.unwrap();
                noted
                    .lock()
                    .unwrap()
                    .targets
                    .push(format!("{target}:{}", u16::from_be_bytes(target_port)));
                if let Some(code) = refuse {
                    let _ = stream.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                    return;
                }
                let mut onward = TcpStream::connect(("127.0.0.1", to)).await.unwrap();
                stream
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut onward).await;
            });
        }
    });
    (port, asked)
}

/// A server that echoes what it's sent, after saying `first`.
async fn echo_server(first: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = stream.write_all(first.as_bytes()).await;
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

/// A port nothing listens on.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

async fn get(net: &Net, url: &str) -> Result<String, String> {
    let client = net.client().await;
    let response = client.get(url).send().await.map_err(|e| format!("{e:?}"))?;
    response.text().await.map_err(|e| e.to_string())
}

/// Reads `n` bytes from `link`.
async fn read_n(link: &mut Link, n: usize) -> String {
    let mut buf = vec![0u8; n];
    tokio::time::timeout(Duration::from_secs(5), link.read_exact(&mut buf))
        .await
        .expect("bytes in time")
        .unwrap();
    String::from_utf8(buf).unwrap()
}

#[test]
fn settings_are_checked_and_tidied() {
    let defaults = NetworkSettings::default();
    assert_eq!(defaults.timeout(), Duration::from_secs(30));
    assert_eq!(defaults.check(), Ok(()));

    let timeout = |secs| NetworkSettings {
        timeout_secs: Some(secs),
        ..NetworkSettings::default()
    };
    assert_eq!(timeout(5).check(), Ok(()));
    assert_eq!(timeout(300).timeout(), Duration::from_secs(300));
    for wrong in [0, 4, 301] {
        assert_eq!(
            timeout(wrong).check().unwrap_err(),
            "The timeout can be from 5 to 300 seconds."
        );
    }

    let none = NetworkSettings {
        proxy: Some(ProxyMode::Custom),
        ..NetworkSettings::default()
    };
    assert_eq!(
        none.check().unwrap_err(),
        "Enter the proxy's host and port."
    );

    let check = |host: &str, port: u16| {
        custom(proxy(ProxyKind::Http, host, port, None), false)
            .normalized()
            .check()
    };
    assert_eq!(check("proxy.example.com", 3128), Ok(()));
    assert_eq!(check("10.0.0.1", 8080), Ok(()));
    assert_eq!(check("::1", 8080), Ok(()));
    assert_eq!(check("[2001:db8::1]", 8080), Ok(()));
    assert_eq!(check("  ", 3128).unwrap_err(), "Enter the proxy's host.");
    assert_eq!(
        check("proxy.example.com", 0).unwrap_err(),
        "Enter the proxy's port."
    );
    for with_more in [
        "http://proxy.example.com",
        "proxy.example.com/x",
        "me@proxy",
    ] {
        assert_eq!(
            check(with_more, 3128).unwrap_err(),
            "Enter only the proxy's name or address, such as proxy.example.com."
        );
    }
    assert_eq!(
        check("proxy.example.com:3128", 3128).unwrap_err(),
        "Enter the proxy's port in its own field."
    );
    assert_eq!(
        check("proxy example", 3128).unwrap_err(),
        "The proxy's host can't have spaces in it."
    );

    let tidied = custom(
        proxy(ProxyKind::Socks5, "  socks.example.com ", 1080, Some("   ")),
        true,
    )
    .normalized();
    assert_eq!(
        tidied.custom,
        Some(proxy(ProxyKind::Socks5, "socks.example.com", 1080, None))
    );

    // SOCKS5 takes 255 bytes of each; HTTP has no such limit.
    let long = "u".repeat(256);
    let socks = proxy(ProxyKind::Socks5, "socks.example.com", 1080, Some(&long));
    assert_eq!(
        socks.check().unwrap_err(),
        "A SOCKS5 proxy takes a username of 255 bytes at most."
    );
    let http = proxy(ProxyKind::Http, "proxy.example.com", 3128, Some(&long));
    assert_eq!(http.check(), Ok(()));
    assert!(http.check_password(&secret(&long)).is_ok());
    let socks = proxy(ProxyKind::Socks5, "socks.example.com", 1080, Some("me"));
    assert_eq!(
        socks.check_password(&secret(&long)).unwrap_err(),
        "A SOCKS5 proxy takes a password of 255 bytes at most."
    );
}

#[test]
fn the_file_keeps_its_form() {
    let saved = Saved {
        settings: NetworkSettings {
            timeout_secs: Some(45),
            ..custom(
                proxy(ProxyKind::Http, "proxy.example.com", 3128, Some("alice")),
                true,
            )
        },
        password_saved: true,
        password_changed: Some(7),
    };
    assert_eq!(
        serde_json::to_value(&saved).unwrap(),
        serde_json::json!({
            "timeoutSecs": 45,
            "proxy": "custom",
            "custom": {"kind": "http", "host": "proxy.example.com", "port": 3128, "username": "alice"},
            "cloudHosts": true,
            "browser": false,
            "browserPlaces": "local",
            "passwordSaved": true,
            "passwordChanged": 7,
        })
    );
    assert_eq!(
        serde_json::to_value(Saved::default()).unwrap(),
        serde_json::json!({
            "cloudHosts": false,
            "browser": false,
            "browserPlaces": "local",
        })
    );
    let read: Saved = serde_json::from_str("{}").unwrap();
    assert_eq!(read.settings, NetworkSettings::default());
    assert!(!read.password_saved);
}

/// Until a proxy is chosen, the system's is used, which is none while the
/// system has none set; saving the rest chooses none. A process without
/// settings of its own, a cloud host's daemon, does the same.
#[test]
fn until_a_proxy_is_chosen_the_systems_is_used() {
    let unchosen = NetworkSettings::default();
    assert_eq!(unchosen.proxy, None);
    assert_eq!(unchosen.mode(), ProxyMode::System);
    assert!(unchosen.proxied());
    assert_eq!(unchosen.custom_in_use(), None);
    assert_eq!(Net::default().lock().settings, unchosen);

    let dir = crate::testing::temp_dir();
    let file = dir.0.join("network-settings.json");
    let written =
        || -> serde_json::Value { serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap() };
    let net = Net::default();
    net.init(file.clone(), Arc::new(|| Ok(None)));
    assert_eq!(net.view().settings, unchosen, "no file yet");
    net.save(NetworkSettings {
        timeout_secs: Some(60),
        ..unchosen.clone()
    })
    .unwrap();
    assert_eq!(written().get("proxy"), None, "the rest saved, none chosen");
    assert_eq!(net.lock().settings.mode(), ProxyMode::System);

    for (chosen, said) in [(ProxyMode::Direct, "direct"), (ProxyMode::System, "system")] {
        net.save(NetworkSettings {
            proxy: Some(chosen),
            ..unchosen.clone()
        })
        .unwrap();
        assert_eq!(written()["proxy"], said);
        assert_eq!(net.lock().settings.mode(), chosen, "kept as chosen");
    }
}

#[test]
fn a_custom_proxy_is_signed_in_to_by_its_url() {
    let http = proxy(ProxyKind::Http, "proxy.example.com", 3128, Some("al ice"));
    let url = http.url(Some(&secret("p@ss:w/rd"))).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("proxy.example.com"));
    assert_eq!(url.port(), Some(3128));
    assert_eq!(url.username(), "al%20ice");
    assert_eq!(url.password(), Some("p%40ss%3Aw%2Frd"));

    // The proxy looks the names up, and an address in brackets.
    let socks = proxy(ProxyKind::Socks5, "::1", 1080, None);
    let url = socks.url(None).unwrap();
    assert_eq!(url.as_str(), "socks5h://[::1]:1080");
    assert_eq!(socks.address(), "[::1]:1080");
}

#[test]
fn this_computer_is_told_from_the_rest() {
    for here in [
        "localhost",
        "LOCALHOST",
        "app.localhost",
        "127.0.0.1",
        "127.8.0.1",
        "::1",
        "[::1]",
    ] {
        assert!(on_this_computer(here), "{here}");
    }
    for elsewhere in ["example.com", "10.0.0.1", "localhost.example.com", "::2"] {
        assert!(!on_this_computer(elsewhere), "{elsewhere}");
    }
}

#[test]
fn a_sleep_is_told_by_the_clocks() {
    let (wall, monotonic) = (SystemTime::now(), Instant::now());
    let later = |wall_secs: i64, monotonic_secs: u64| {
        let wall = if wall_secs >= 0 {
            wall + Duration::from_secs(wall_secs as u64)
        } else {
            wall - Duration::from_secs(wall_secs.unsigned_abs())
        };
        (wall, monotonic + Duration::from_secs(monotonic_secs))
    };
    assert!(!slept((wall, monotonic), later(10, 10)));
    assert!(!slept((wall, monotonic), later(35, 10)));
    assert!(slept((wall, monotonic), later(41, 10)));
    assert!(slept((wall, monotonic), later(3600, 0)));
    // A clock set back is no sleep.
    assert!(!slept((wall, monotonic), later(-600, 10)));
}

/// The client keeps its connection from one request to the next, and
/// connects afresh after `refresh`, a change of the settings, a sleep or
/// ten minutes.
#[tokio::test]
async fn the_shared_client_keeps_its_connection() {
    let (port, connections) = server("ok").await;
    // A custom proxy, which this computer's server goes around: whatever
    // proxy this machine has set doesn't count.
    let unused = closed_port().await;
    let net = Net::default();
    net.save(custom(
        proxy(ProxyKind::Http, "127.0.0.1", unused, None),
        false,
    ))
    .unwrap();
    let url = format!("http://127.0.0.1:{port}/");
    let count = || connections.load(Ordering::SeqCst);

    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 1, "one connection for three requests");

    net.refresh();
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 2, "after a refresh");
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 2);

    let mut longer = net.lock().settings.clone();
    longer.timeout_secs = Some(60);
    net.save(longer.clone()).unwrap();
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 3, "after the settings changed");
    // Saved again as they are: nothing to connect afresh for.
    net.save(longer).unwrap();
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 3);

    // The computer slept: an hour on the wall clock, none on the process's.
    let _ = net.shared(
        SystemTime::now() + Duration::from_secs(3600),
        Instant::now(),
    );
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 4, "after a sleep");

    let _ = net.shared(SystemTime::now(), Instant::now() + MAX_AGE);
    assert_eq!(get(&net, &url).await.unwrap(), "ok");
    assert_eq!(count(), 5, "once ten minutes old");
}

/// Requests go through a custom HTTP proxy, signed in to with the saved
/// password, but nothing on this computer does.
#[tokio::test]
async fn a_custom_http_proxy_carries_requests_and_signs_in() {
    let (proxy_port, heads) = http_proxy("through the proxy").await;
    let net = Net::default();
    net.save(custom(
        proxy(ProxyKind::Http, "127.0.0.1", proxy_port, Some("alice")),
        false,
    ))
    .unwrap();
    net.password_saved(Some(secret("s3cret"))).unwrap();

    assert_eq!(
        get(&net, "http://provider.invalid/v1/models")
            .await
            .unwrap(),
        "through the proxy"
    );
    let head = heads.lock().unwrap()[0].clone();
    assert!(
        head.starts_with("GET http://provider.invalid/v1/models HTTP/1.1\r\n"),
        "{head}"
    );
    assert_eq!(
        header(&head, "proxy-authorization"),
        Some(tunnel::basic("alice", "s3cret").as_str())
    );

    let (port, connections) = server("here").await;
    for host in ["127.0.0.1", "localhost"] {
        assert_eq!(
            get(&net, &format!("http://{host}:{port}/")).await.unwrap(),
            "here"
        );
    }
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert_eq!(
        heads.lock().unwrap().len(),
        1,
        "nothing local through the proxy"
    );
}

/// A SOCKS5 proxy is given the names to look up, and signed in to.
#[tokio::test]
async fn a_custom_socks5_proxy_carries_requests_by_name() {
    let (port, _) = server("by socks").await;
    let (proxy_port, asked) = socks5_proxy(Some(("bob", "pw")), port, None).await;
    let net = Net::default();
    net.save(custom(
        proxy(ProxyKind::Socks5, "127.0.0.1", proxy_port, Some("bob")),
        false,
    ))
    .unwrap();
    net.password_saved(Some(secret("pw"))).unwrap();

    assert_eq!(
        get(&net, "http://provider.invalid:8000/").await.unwrap(),
        "by socks"
    );
    let asked = asked.lock().unwrap();
    assert_eq!(asked.targets, ["provider.invalid:8000"]);
    assert_eq!(asked.users, [("bob".to_string(), "pw".to_string())]);
}

/// "No proxy" goes around the system's proxy, which "System proxy" goes
/// through, as do the settings while none is chosen. The system's proxy is
/// the process's variables here, so this runs in a process of its own.
#[test]
fn no_proxy_goes_around_the_systems_proxy() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "net::tests::no_proxy_in_a_process_of_its_own"])
        .args(["--ignored", "--test-threads=1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[tokio::test]
#[ignore = "run in a process of its own by no_proxy_goes_around_the_systems_proxy"]
async fn no_proxy_in_a_process_of_its_own() {
    let (proxy_port, heads) = http_proxy("through the system's proxy").await;
    let (port, connections) = server("directly").await;
    // This process's system proxy, for this computer's addresses too.
    for name in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        std::env::remove_var(name);
    }
    let system = format!("http://127.0.0.1:{proxy_port}");
    std::env::set_var("HTTP_PROXY", &system);
    std::env::set_var("HTTPS_PROXY", &system);
    // Set, so that no list of the system's takes its place (Windows).
    std::env::set_var("NO_PROXY", "nothing.invalid");
    let url = format!("http://127.0.0.1:{port}/");
    let through = || heads.lock().unwrap().len();

    let net = Net::default();
    assert_eq!(
        get(&net, &url).await.unwrap(),
        "through the system's proxy",
        "none chosen"
    );
    assert_eq!(through(), 1);

    net.save(NetworkSettings {
        proxy: Some(ProxyMode::Direct),
        ..NetworkSettings::default()
    })
    .unwrap();
    assert_eq!(get(&net, &url).await.unwrap(), "directly", "no proxy");
    let download = net.builder().await.build().unwrap();
    let body = download.get(&url).send().await.unwrap().text().await;
    assert_eq!(body.unwrap(), "directly", "a download's client too");
    assert_eq!(through(), 1);
    assert_eq!(connections.load(Ordering::SeqCst), 2);

    let system_proxy = NetworkSettings {
        proxy: Some(ProxyMode::System),
        cloud_hosts: true,
        browser: true,
        ..NetworkSettings::default()
    };
    net.save(system_proxy.clone()).unwrap();
    assert_eq!(get(&net, &url).await.unwrap(), "through the system's proxy");
    assert_eq!(through(), 2);
    assert_eq!(
        cloud_hop(&net.lock(), "build.example.com", 22),
        Ok(Hop::Http {
            host: "127.0.0.1".into(),
            port: proxy_port,
            auth: None,
        })
    );
    assert_eq!(net.browser_proxy().await.map(|p| p.server), Some(system));

    // No proxy again: none of it, though the boxes are kept.
    net.save(NetworkSettings {
        proxy: Some(ProxyMode::Direct),
        ..system_proxy
    })
    .unwrap();
    assert_eq!(get(&net, &url).await.unwrap(), "directly");
    assert_eq!(through(), 2);
    assert_eq!(
        cloud_hop(&net.lock(), "build.example.com", 22),
        Ok(Hop::Direct)
    );
    assert_eq!(net.browser_proxy().await, None);
}

/// Another window's save is followed, and the password read again when
/// it says it changed; this window's own changes need no reading.
#[tokio::test]
async fn another_windows_change_is_followed() {
    let dir = crate::testing::temp_dir();
    let file = dir.0.join("network-settings.json");
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let net = Net::default();
    net.init(
        file.clone(),
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Secret("from the keychain".into())))
        }),
    );
    let settings = custom(
        proxy(ProxyKind::Http, "proxy.example.com", 3128, Some("alice")),
        true,
    );
    net.save(settings.clone()).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(written["custom"]["host"], "proxy.example.com");

    // Read once a proxy that signs in is in use, then kept.
    net.read_password().await;
    net.read_password().await;
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(net.lock().password, Some(secret("from the keychain")));

    // Another window: a longer timeout, and a password changed.
    let other = serde_json::json!({
        "timeoutSecs": 90,
        "proxy": "custom",
        "custom": {"kind": "http", "host": "proxy.example.com", "port": 3128, "username": "alice"},
        "cloudHosts": true,
        "passwordSaved": true,
        "passwordChanged": 12,
    });
    std::fs::write(&file, serde_json::to_vec(&other).unwrap()).unwrap();
    net.follow(true);
    assert_eq!(net.lock().settings.timeout_secs, Some(90));
    assert!(net.view().password_saved);
    net.read_password().await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "read again after its change"
    );

    // This window's own: no read, and the file says so for the others.
    net.password_saved(None).unwrap();
    net.follow(true);
    net.read_password().await;
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(net.lock().password, None);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(written.get("passwordSaved").is_none());
    assert!(written["passwordChanged"].as_u64().unwrap() > 12);

    // A file that isn't settings changes nothing.
    std::fs::write(&file, b"{ not json").unwrap();
    net.follow(true);
    assert_eq!(net.lock().settings.timeout_secs, Some(90));
}

#[test]
fn cloud_hosts_go_through_the_proxy_only_when_set_to() {
    let net = Net::default();
    let hop = |net: &Net, host: &str| cloud_hop(&net.lock(), host, 22);
    assert_eq!(hop(&net, "build.example.com"), Ok(Hop::Direct));

    net.save(custom(
        proxy(ProxyKind::Http, "proxy.example.com", 3128, Some("alice")),
        false,
    ))
    .unwrap();
    net.password_saved(Some(secret("pw"))).unwrap();
    assert_eq!(
        hop(&net, "build.example.com"),
        Ok(Hop::Direct),
        "not for cloud hosts"
    );

    let mut settings = net.lock().settings.clone();
    settings.cloud_hosts = true;
    net.save(settings.clone()).unwrap();
    assert_eq!(
        hop(&net, "build.example.com"),
        Ok(Hop::Http {
            host: "proxy.example.com".into(),
            port: 3128,
            auth: Some(tunnel::basic("alice", "pw")),
        })
    );
    assert_eq!(hop(&net, "127.0.0.1"), Ok(Hop::Direct), "this computer");

    settings.custom = Some(proxy(ProxyKind::Socks5, "socks.example.com", 1080, None));
    net.save(settings).unwrap();
    assert_eq!(
        hop(&net, "build.example.com"),
        Ok(Hop::Socks5 {
            host: "socks.example.com".into(),
            port: 1080,
            auth: None,
            remote_names: true,
        })
    );
}

/// "Use proxy for browser": the browser here, the cloud hosts', or both,
/// go through the proxy in use; without it, Chrome's own settings.
#[tokio::test]
async fn the_browsers_get_the_proxy_where_it_is_set_for() {
    let net = Net::default();
    let mut settings = custom(
        proxy(ProxyKind::Http, "proxy.example.com", 3128, Some("alice")),
        false,
    );
    net.save(settings.clone()).unwrap();
    net.password_saved(Some(secret("s3cret"))).unwrap();
    assert_eq!(net.browser_proxy().await, None, "not for the browser");
    assert_eq!(net.cloud_browser_proxy().await, None);

    let theirs = Some(BrowserProxy {
        server: "http://proxy.example.com:3128".into(),
        username: Some("alice".into()),
        password: Some(secret("s3cret")),
    });
    settings.browser = true;
    for (places, here, cloud) in [
        (BrowserPlaces::Local, true, false),
        (BrowserPlaces::Cloud, false, true),
        (BrowserPlaces::Both, true, true),
    ] {
        settings.browser_places = places;
        net.save(settings.clone()).unwrap();
        let expect = |yes: bool| if yes { theirs.clone() } else { None };
        assert_eq!(net.browser_proxy().await, expect(here), "{places:?} here");
        assert_eq!(
            net.cloud_browser_proxy().await,
            expect(cloud),
            "{places:?} in the cloud"
        );
    }
    let shown = format!("{:?}", net.browser_proxy().await);
    assert!(!shown.contains("s3cret"), "no password in a log: {shown}");

    settings.custom = Some(proxy(ProxyKind::Socks5, "::1", 1080, None));
    net.save(settings).unwrap();
    let socks = net.browser_proxy().await.unwrap();
    assert_eq!(socks.server, "socks5://[::1]:1080");
    assert_eq!((socks.username, socks.password), (None, None));

    // On a cloud host, the one the app sent, whatever its own settings.
    let sent = BrowserProxy {
        server: "http://egress.example.com:8080".into(),
        username: None,
        password: None,
    };
    net.lock().hosted_browser = Some(Some(sent.clone()));
    assert_eq!(net.browser_proxy().await, Some(sent));
    net.lock().hosted_browser = Some(None);
    assert_eq!(net.browser_proxy().await, None);
}

/// "No proxy": the proxy kept for later isn't used, by cloud hosts and the
/// browsers neither, whatever their boxes say; back to it, all of it is.
#[tokio::test]
async fn no_proxy_keeps_the_rest_for_later() {
    let net = Net::default();
    let mut settings = custom(
        proxy(ProxyKind::Http, "proxy.example.com", 3128, None),
        true,
    );
    settings.browser = true;
    settings.browser_places = BrowserPlaces::Both;
    settings.proxy = Some(ProxyMode::Direct);
    net.save(settings.clone()).unwrap();
    assert!(matches!(effective(&net.lock()).route, Route::Direct));
    assert_eq!(
        cloud_hop(&net.lock(), "build.example.com", 22),
        Ok(Hop::Direct)
    );
    assert_eq!(net.browser_proxy().await, None);
    assert_eq!(net.cloud_browser_proxy().await, None);

    settings.proxy = Some(ProxyMode::Custom);
    net.save(settings).unwrap();
    assert!(matches!(effective(&net.lock()).route, Route::Custom(..)));
    assert!(matches!(
        cloud_hop(&net.lock(), "build.example.com", 22),
        Ok(Hop::Http { .. })
    ));
    let server = Some("http://proxy.example.com:3128".to_string());
    assert_eq!(net.browser_proxy().await.map(|p| p.server), server);
    assert_eq!(net.cloud_browser_proxy().await.map(|p| p.server), server);
}

/// The system's proxy goes to the browser as this computer's requests
/// find it, signed in to with what its URL has.
#[test]
fn the_systems_proxy_is_given_to_the_browser_with_its_sign_in() {
    let net = Net::default();
    let system = |url: &str| Matcher::builder().all(url.to_string()).build();
    let state = net.lock();
    assert_eq!(
        proxy_for_browser(&state, &system("http://alice:p%40ss@proxy.corp:3128")),
        Some(BrowserProxy {
            server: "http://proxy.corp:3128".into(),
            username: Some("alice".into()),
            password: Some(secret("p@ss")),
        })
    );
    assert_eq!(
        proxy_for_browser(&state, &system("socks5h://bob:pw@socks.corp:1080")),
        Some(BrowserProxy {
            server: "socks5://socks.corp:1080".into(),
            username: Some("bob".into()),
            password: Some(secret("pw")),
        })
    );
    assert_eq!(
        proxy_for_browser(&state, &system("http://plain.corp:8080"))
            .map(|p| (p.server, p.username, p.password)),
        Some(("http://plain.corp:8080".into(), None, None))
    );
    assert_eq!(proxy_for_browser(&state, &Matcher::builder().build()), None);
}

/// SSH's server talks first: what it says right after the proxy's answer
/// isn't lost.
#[tokio::test]
async fn a_tunnel_keeps_what_the_host_says_first() {
    let (port, heads) = connect_proxy(
        "HTTP/1.1 200 Connection established\r\nVia: test\r\n\r\n",
        "SSH-2.0-host\r\n",
    )
    .await;
    let hop = Hop::http("127.0.0.1", port, Some("alice"), "pw");
    let mut link = tunnel::open("build.example.com", 22, &hop, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(read_n(&mut link, 14).await, "SSH-2.0-host\r\n");
    link.write_all(b"SSH-2.0-client\r\n").await.unwrap();
    assert_eq!(read_n(&mut link, 16).await, "SSH-2.0-client\r\n");

    let head = heads.lock().unwrap()[0].clone();
    assert!(
        head.starts_with("CONNECT build.example.com:22 HTTP/1.1\r\nHost: build.example.com:22\r\n"),
        "{head}"
    );
    assert_eq!(
        header(&head, "proxy-authorization"),
        Some(tunnel::basic("alice", "pw").as_str())
    );
}

#[tokio::test]
async fn a_socks5_tunnel_signs_in_and_names_the_host() {
    let echo = echo_server("SSH-2.0-host\r\n").await;
    let (port, asked) = socks5_proxy(Some(("bob", "pw")), echo, None).await;
    let hop = Hop::Socks5 {
        host: "127.0.0.1".into(),
        port,
        auth: Some(("bob".into(), "pw".into())),
        remote_names: true,
    };
    let mut link = tunnel::open("build.example.com", 22, &hop, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(read_n(&mut link, 14).await, "SSH-2.0-host\r\n");
    link.write_all(b"hello").await.unwrap();
    assert_eq!(read_n(&mut link, 5).await, "hello");
    assert_eq!(asked.lock().unwrap().targets, ["build.example.com:22"]);

    // An address is sent as one.
    let (port, asked) = socks5_proxy(None, echo, None).await;
    let hop = Hop::Socks5 {
        host: "127.0.0.1".into(),
        port,
        auth: None,
        remote_names: true,
    };
    let _ = tunnel::open("10.1.2.3", 2222, &hop, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(asked.lock().unwrap().targets, ["10.1.2.3:2222"]);
}

#[tokio::test]
async fn a_proxy_that_refuses_says_why() {
    let open = |hop: Hop| async move {
        tunnel::open("build.example.com", 22, &hop, Duration::from_secs(5))
            .await
            .map(|_| ())
            .unwrap_err()
    };
    let (port, _) = connect_proxy("HTTP/1.1 407 Proxy Authentication Required\r\n\r\n", "").await;
    assert_eq!(
        open(Hop::http("127.0.0.1", port, None, "")).await,
        "The proxy asks for a username and a password."
    );
    assert_eq!(
        open(Hop::http("127.0.0.1", port, Some("alice"), "wrong")).await,
        "The proxy didn't accept its username and password."
    );
    let (port, _) = connect_proxy("HTTP/1.1 403 Forbidden\r\n\r\n", "").await;
    assert_eq!(
        open(Hop::http("127.0.0.1", port, None, "")).await,
        "The proxy refused to connect to build.example.com:22: HTTP/1.1 403 Forbidden"
    );

    let echo = echo_server("").await;
    let (port, _) = socks5_proxy(Some(("bob", "pw")), echo, None).await;
    let socks = |auth: Option<(&str, &str)>, port| Hop::Socks5 {
        host: "127.0.0.1".into(),
        port,
        auth: auth.map(|(u, p)| (u.to_string(), p.to_string())),
        remote_names: true,
    };
    assert_eq!(
        open(socks(Some(("bob", "nope")), port)).await,
        "The proxy didn't accept its username and password."
    );
    assert_eq!(
        open(socks(None, port)).await,
        "The proxy asks for a username and a password."
    );
    let (port, _) = socks5_proxy(None, echo, Some(5)).await;
    assert_eq!(
        open(socks(None, port)).await,
        "build.example.com:22 refused the proxy's connection."
    );

    let nobody = closed_port().await;
    let unreachable = open(Hop::http("127.0.0.1", nobody, None, "")).await;
    assert!(
        unreachable.starts_with(&format!(
            "The proxy at 127.0.0.1:{nobody} can't be reached: "
        )),
        "{unreachable}"
    );
}

/// The connection and the proxy's handshake together get the timeout.
#[tokio::test]
async fn a_proxy_that_never_answers_runs_out_the_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let started = Instant::now();
    let error = tunnel::open(
        "build.example.com",
        22,
        &Hop::http("127.0.0.1", port, None, ""),
        Duration::from_secs(1),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert_eq!(
        error,
        format!("The proxy at 127.0.0.1:{port} didn't connect to build.example.com:22 within 1 s.")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}
