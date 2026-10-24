use super::*;
use std::io::{Read, Write};

#[test]
fn a_resolver_on_the_loopback_is_replaced_by_the_forwarder_and_the_rest_kept() {
    let ubuntu = "# This is /run/systemd/resolve/stub-resolv.conf\n\
                  nameserver 127.0.0.53\n\
                  options edns0 trust-ad\n\
                  search example.internal\n";
    assert_eq!(
        through_forwarder(ubuntu).as_deref(),
        Some(
            "# This is /run/systemd/resolve/stub-resolv.conf\n\
             nameserver 169.254.1.1\n\
             options edns0 trust-ad\n\
             search example.internal\n"
        )
    );
    // A local cache with fallbacks: the cache's place goes to the
    // forwarder, once, and the fallbacks stay.
    assert_eq!(
        through_forwarder("nameserver 127.0.0.1\nnameserver ::1\nnameserver 9.9.9.9\n").as_deref(),
        Some("nameserver 169.254.1.1\nnameserver 9.9.9.9\n")
    );
    // Resolvers the namespace reaches through pasta need nothing.
    assert_eq!(
        through_forwarder("nameserver 10.0.0.2\nnameserver 8.8.8.8\n"),
        None
    );
    assert_eq!(through_forwarder(""), None);
}

#[test]
fn only_proxies_on_the_loopback_have_their_ports_reached() {
    let vars = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    };
    assert_eq!(
        loopback_proxy_ports(vars(&[
            ("HTTPS_PROXY", "http://127.0.0.1:3128"),
            ("http_proxy", "localhost:8080"),
            ("ALL_PROXY", "socks5h://[::1]:1080"),
            ("all_proxy", "socks5://localhost"),
            ("https_proxy", "http://user:secret@127.0.0.1:3128/"),
            ("NO_PROXY", "127.0.0.1:9999"),
            ("HTTPS_PROXY", "http://proxy.corp:3128"),
            ("PATH", "/usr/bin"),
        ])),
        vec![1080, 3128, 8080]
    );
    assert_eq!(loopback_port("http://10.1.2.3:3128"), None);
    assert_eq!(loopback_port("http://localhost"), Some(80));
}

#[test]
fn pasta_forwards_nothing_in_and_reaches_only_the_proxies_ports() {
    let args = pasta_args(4242, &[3128, 8080]);
    let pair = |flag: &str| {
        let at = args.iter().position(|a| a == flag).unwrap();
        args[at + 1].clone()
    };
    assert_eq!(pair("--tcp-ports"), "none");
    assert_eq!(pair("--udp-ports"), "none");
    assert_eq!(pair("--tcp-ns"), "3128,8080");
    assert_eq!(pair("--udp-ns"), "none");
    assert_eq!(pair("--dns-forward"), DNS_FORWARDER);
    assert!(args.contains(&"--foreground".to_string()));
    assert_eq!(args.last().map(String::as_str), Some("4242"));
    assert_eq!(pair_of(&pasta_args(1, &[]), "--tcp-ns"), "none");
}

fn pair_of(args: &[String], flag: &str) -> String {
    let at = args.iter().position(|a| a == flag).unwrap();
    args[at + 1].clone()
}

#[test]
fn the_shims_ask_rovik_with_the_opener() {
    let dir = crate::testing::temp_dir();
    write_shims(&dir.0, Path::new("/opt/rovik/bin/rovik-agentd")).unwrap();
    for name in BROWSERS {
        let path = dir.0.join("bin").join(name);
        let script = std::fs::read_to_string(&path).unwrap();
        assert!(
            script.contains("exec '/opt/rovik/bin/rovik-agentd' open \"$@\""),
            "{script}"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "{name}");
    }
}

/*
 * ---------------------------------------------------------------------------
 * A real network: where pasta is installed
 * ---------------------------------------------------------------------------
 */

/// A network for one test, reaching `ports` of the host's loopback, its
/// host resolv.conf `resolv` (the real one when None): see [`for_test`].
fn network(ports: &[u16], resolv: Option<&Path>) -> Option<(crate::testing::TempDir, Network)> {
    for_test(Path::new("/bin/true"), ports, resolv)
}

/// `script` run by sh in `network`: its output.
fn run_in(network: &Network, script: &str) -> std::process::Output {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", script]);
    network.join(&mut cmd);
    cmd.output().unwrap()
}

/// A socket listening on `port` of `network`'s loopback.
fn listen_on(network: &Network, port: u16) -> io::Result<std::net::TcpListener> {
    network
        .socket_inside(Inside::Listen(raw_address(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            port,
        )))))
        .map(std::net::TcpListener::from)
}

#[test]
fn two_networks_take_the_same_port_at_once_and_the_host_keeps_its_own() {
    let Some((_a_dir, a)) = network(&[], None) else {
        return;
    };
    let Some((_b_dir, b)) = network(&[], None) else {
        return;
    };
    // Taken on the host first, as another user's server would have it.
    let host = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = host.local_addr().unwrap().port();
    let in_a = listen_on(&a, port).expect("the port is free in a's network");
    let in_b = listen_on(&b, port).expect("and in b's");
    // Within one network the port is that user's: a second server is
    // refused it, as on a computer.
    let busy = listen_on(&a, port).unwrap_err();
    assert_eq!(busy.raw_os_error(), Some(libc::EADDRINUSE), "{busy}");
    drop((in_a, in_b, host));
}

#[test]
fn a_server_in_the_network_is_reached_only_through_it() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let listener = network.listen().unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello from inside").unwrap();
        // Stopped, not only dropped: a child another test forks meanwhile
        // holds a copy of the descriptor until it execs, and the port would
        // still listen until then.
        // SAFETY: shutdown on a socket this thread owns.
        unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RDWR) };
    });
    // The host's loopback is another one: nothing there (unless a host
    // process happens to listen on the same number, which this checks).
    let on_host = crate::listening::sockets(Path::new("/proc/net")).unwrap();
    if !on_host.values().any(|&p| p == port) {
        let outside = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap_err();
        assert_eq!(
            outside.kind(),
            io::ErrorKind::ConnectionRefused,
            "{outside}"
        );
    }
    let mut inside = network.connect(port).unwrap();
    let mut said = String::new();
    inside.read_to_string(&mut said).unwrap();
    assert_eq!(said, "hello from inside");
    server.join().unwrap();

    // Nothing listening: refused, as the agent's note says.
    let refused = network.connect(port).unwrap_err();
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::ECONNREFUSED),
        "{refused}"
    );
}

#[test]
fn a_joined_command_is_this_user_and_sees_the_networks_resolv_conf() {
    let dir = crate::testing::temp_dir();
    let resolv = dir.0.join("resolv.conf");
    std::fs::write(&resolv, "nameserver 127.0.0.53\nsearch corp\n").unwrap();
    let Some((_net_dir, network)) = network(&[], Some(&resolv)) else {
        return;
    };
    let out = run_in(
        &network,
        &format!("id -u; cat '{}'; ls /proc/self/ns/net -l", resolv.display()),
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // SAFETY: geteuid can't fail.
    let uid = unsafe { libc::geteuid() };
    assert!(said.starts_with(&format!("{uid}\n")), "{said}");
    assert!(
        said.contains("nameserver 169.254.1.1\nsearch corp"),
        "{said}"
    );
    // The host's file is as it was.
    assert_eq!(
        std::fs::read_to_string(&resolv).unwrap(),
        "nameserver 127.0.0.53\nsearch corp\n"
    );
    let ours = std::fs::read_link("/proc/self/ns/net").unwrap();
    assert!(
        !said.contains(&ours.to_string_lossy().into_owned()),
        "{said}"
    );
}

#[test]
fn a_command_in_the_network_reaches_the_hosts_proxy_at_the_same_address() {
    // The proxy on the host's loopback, as HTTPS_PROXY would name it.
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = proxy.local_addr().unwrap().port();
    let Some((_dir, network)) = network(&[port], None) else {
        return;
    };
    let answered = std::thread::spawn(move || {
        let (mut stream, _) = proxy.accept().unwrap();
        stream.write_all(b"proxy here").unwrap();
    });
    let mut through = std::net::TcpStream::from(
        network
            .socket_inside(Inside::Connect(
                [
                    SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
                ]
                .map(raw_address),
            ))
            .expect("the proxy's port is reached from inside"),
    );
    let mut said = String::new();
    through.read_to_string(&mut said).unwrap();
    assert_eq!(said, "proxy here");
    answered.join().unwrap();
}

#[test]
fn the_process_holding_a_busy_port_is_named() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let listener = network.listen().unwrap();
    let port = listener.local_addr().unwrap().port();
    // This process holds the socket made inside.
    let holders = network.listening_on(port);
    assert!(
        holders.iter().any(|(pid, _)| *pid == std::process::id()),
        "{holders:?}"
    );
    assert_eq!(network.listening_on(port.wrapping_add(1)), Vec::new());
}

/// The processes of this test's network: its holder's and pasta's pids.
fn kept_going(network: &Network) -> (u32, Option<u32>) {
    let holder = network.holder.load(Ordering::Relaxed);
    let pasta = crate::listening::process_tree(std::process::id())
        .unwrap()
        .into_iter()
        .find(|pid| {
            crate::listening::command_line(*pid)
                .is_some_and(|line| line.contains(PASTA) && line.ends_with(&format!(" {holder}")))
        });
    (holder, pasta)
}

#[test]
fn the_holder_and_pasta_are_started_again_when_they_end() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let ns = |pid: u32| std::fs::read_link(format!("/proc/{pid}/ns/net")).unwrap();
    let (holder, pasta) = kept_going(&network);
    let before = ns(holder);
    let pasta = pasta.expect("pasta runs for the network");
    // SAFETY: a signal to a child of this process's network.
    unsafe { libc::kill(pasta as i32, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, again) = kept_going(&network);
        if again.is_some_and(|p| p != pasta) && has_default_route(holder) {
            break;
        }
        assert!(Instant::now() < deadline, "pasta wasn't started again");
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: as above, for the holder.
    unsafe { libc::kill(holder as i32, libc::SIGKILL) };
    loop {
        let (now, pasta_now) = kept_going(&network);
        if now != holder && pasta_now.is_some() && has_default_route(now) {
            // The same namespace, joined by the new holder.
            assert_eq!(ns(now), before);
            break;
        }
        assert!(Instant::now() < deadline, "the holder wasn't started again");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_dropped_network_leaves_no_process_and_no_folder() {
    let Some((dir, network)) = network(&[], None) else {
        return;
    };
    let (holder, pasta) = kept_going(&network);
    let folder = network.dir().to_path_buf();
    assert!(folder.join("bin/xdg-open").is_file());
    drop(network);
    for pid in [Some(holder), pasta].into_iter().flatten() {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "{pid} still runs"
        );
    }
    assert!(!folder.exists());
    drop(dir);
}

/// The host's own IPv4 addresses, loopback and interfaces alike.
fn host_ipv4_addresses() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    // SAFETY: getifaddrs' list is read while it's held, then freed once.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return found;
        }
        let mut entry = list;
        while !entry.is_null() {
            let address = (*entry).ifa_addr;
            if !address.is_null() && i32::from((*address).sa_family) == libc::AF_INET {
                let v4 = &*(address as *const libc::sockaddr_in);
                found.push(Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr)));
            }
            entry = (*entry).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    found
}

#[test]
fn a_server_on_every_address_is_still_reached_only_from_inside() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    // Bound to 0.0.0.0, as Express and Next.js do by default.
    let listener = std::net::TcpListener::from(
        network
            .socket_inside(Inside::Listen(raw_address(SocketAddr::from((
                Ipv4Addr::UNSPECIFIED,
                0,
            )))))
            .unwrap(),
    );
    let port = listener.local_addr().unwrap().port();
    let on_host = crate::listening::sockets(Path::new("/proc/net")).unwrap();
    if on_host.values().any(|&p| p == port) {
        eprintln!("a host process listens on {port} too: skipped");
        return;
    }
    let addresses = host_ipv4_addresses();
    assert!(addresses.contains(&Ipv4Addr::LOCALHOST), "{addresses:?}");
    for address in addresses {
        let reached = std::net::TcpStream::connect_timeout(
            &SocketAddr::from((address, port)),
            Duration::from_secs(2),
        );
        assert!(
            reached.is_err(),
            "{address}:{port} reached a server in the network"
        );
    }
    assert!(network.connect(port).is_ok(), "inside, it answers");
}
