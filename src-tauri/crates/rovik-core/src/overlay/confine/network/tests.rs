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
fn pasta_forwards_nothing_in_and_reaches_only_the_proxies_ports() {
    let args = pasta_args(4242, &[3128, 8080], false);
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
    assert_eq!(pair_of(&pasta_args(1, &[], false), "--tcp-ns"), "none");
}

#[test]
fn a_sealed_pasta_sets_up_the_loopback_alone() {
    let args = pasta_args(4242, &[3128], true);
    for flag in ["--tcp-ports", "--udp-ports", "--tcp-ns", "--udp-ns"] {
        assert_eq!(pair_of(&args, flag), "none", "{flag}");
    }
    for flag in ["--config-net", "--dns-forward"] {
        assert!(!args.iter().any(|a| a == flag), "{flag}: {args:?}");
    }
    assert!(args.contains(&"--foreground".to_string()));
    assert_eq!(args.last().map(String::as_str), Some("4242"));
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

/// A socket listening on `port` of `network`'s loopback, 127.0.0.1's.
fn listen_on(network: &Network, port: u16) -> io::Result<std::net::TcpListener> {
    network
        .listen_on(port)
        .map(|mut listening| listening.remove(0))
}

/// A port nothing listens on in `network` now.
fn free_port(network: &Network) -> u16 {
    let listener = network.listen().unwrap();
    listener.local_addr().unwrap().port()
}

/// A service's port listens inside on 127.0.0.1 and, where the network has
/// IPv6, on ::1: a client that looks `localhost` up as either reaches it.
#[test]
fn a_services_port_listens_inside_on_both_loopbacks() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let port = free_port(&network);
    let listening = network.listen_on(port).expect("a free port listens");
    let addresses: Vec<SocketAddr> = listening.iter().map(|l| l.local_addr().unwrap()).collect();
    assert_eq!(addresses[0], SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    assert!(
        addresses[1..]
            .iter()
            .all(|a| *a == SocketAddr::from((Ipv6Addr::LOCALHOST, port))),
        "{addresses:?}"
    );
    let first = listening.into_iter().next().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = first.accept().unwrap();
        stream.write_all(b"the service").unwrap();
    });
    let mut client = network.connect(port).unwrap();
    let mut said = String::new();
    client.read_to_string(&mut said).unwrap();
    assert_eq!(said, "the service");
    server.join().unwrap();
}

/// A process's own network is reached in its namespaces, entered from
/// outside: as the daemon reaches a service in one of its user's rootless
/// containers. The network's holder stands in for the container: its user
/// namespace is this user's, as Podman's is.
#[test]
fn a_port_in_another_processs_network_is_reached_in_its_namespaces() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let listener = network.listen().unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"inside the container").unwrap();
        // Stopped, not only dropped: a child another test forks meanwhile
        // holds a copy of the descriptor until it execs, and the port would
        // still listen until then.
        // SAFETY: shutdown on a socket this thread owns.
        unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RDWR) };
    });
    let holder = network.holder.load(Ordering::Relaxed);
    let container = Namespaces::of(holder).expect("the holder's namespaces");
    let mut client = container.connect(port).expect("its network is entered");
    let mut said = String::new();
    client.read_to_string(&mut said).unwrap();
    assert_eq!(said, "inside the container");
    server.join().unwrap();
    // Nothing listening there now: refused, as a stopped service is.
    let refused = container.connect(port).unwrap_err();
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::ECONNREFUSED),
        "{refused}"
    );
}

/// A process in the daemon's own network is in no container: a connection
/// made there would reach the server's loopback, which every user shares.
#[test]
fn a_process_in_the_daemons_own_network_is_no_container() {
    let refused = Namespaces::of(std::process::id())
        .err()
        .expect("this process's own network refused");
    assert!(
        refused.to_string().contains("the daemon's own network"),
        "{refused}"
    );
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

/*
 * ---------------------------------------------------------------------------
 * Sealed: while the user's allowed hosts are on (allowed-hosts-plan.md)
 * ---------------------------------------------------------------------------
 */

/// A connection to `address` made in `network`: why it failed.
fn unreached(network: &Network, address: IpAddr) -> io::Error {
    let to = SocketAddr::new(address, 80);
    match network.socket_inside(Inside::Connect([to, to].map(raw_address))) {
        Ok(_) => panic!("{to} was reached"),
        Err(error) => error,
    }
}

#[test]
fn a_sealed_network_reaches_its_loopback_and_no_other_address() {
    let Some((_dir, network)) = sealed_for_test() else {
        return;
    };
    let listener = network.listen().unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(network.connect(port).is_ok(), "its loopback is up");
    assert!(!has_default_route(network.holder.load(Ordering::Relaxed)));
    // The computer's own addresses, and any outside: unreachable at once.
    let mut elsewhere: Vec<IpAddr> = host_ipv4_addresses()
        .into_iter()
        .filter(|address| !address.is_loopback())
        .map(IpAddr::V4)
        .collect();
    elsewhere.push(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)));
    elsewhere.push("2001:db8::1".parse().unwrap());
    for address in elsewhere {
        let started = Instant::now();
        let error = unreached(&network, address);
        // A system without IPv6 has no IPv6 socket to try with.
        let failed = [libc::ENETUNREACH, libc::EAFNOSUPPORT];
        assert!(
            error
                .raw_os_error()
                .is_some_and(|code| failed.contains(&code)),
            "{address}: {error}"
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{address}");
    }
}

#[test]
fn an_open_network_is_sealed_and_opened_again_its_servers_kept() {
    let Some((_dir, network)) = network(&[], None) else {
        return;
    };
    let holder = network.holder.load(Ordering::Relaxed);
    assert!(has_default_route(holder));
    let listener = network.listen().unwrap();
    let port = listener.local_addr().unwrap().port();

    network.seal(true).unwrap();
    assert!(!has_default_route(holder));
    let outside = unreached(&network, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)));
    assert_eq!(outside.raw_os_error(), Some(libc::ENETUNREACH), "{outside}");
    assert!(network.connect(port).is_ok(), "its server stays");
    // Asked again, nothing changes.
    let (_, pasta) = kept_going(&network);
    network.seal(true).unwrap();
    assert_eq!(kept_going(&network).1, pasta);

    network.seal(false).unwrap();
    assert!(has_default_route(holder));
    assert!(network.connect(port).is_ok(), "its server stays");
}

#[test]
fn a_sealed_networks_pasta_is_started_again_sealed() {
    let Some((_dir, network)) = sealed_for_test() else {
        return;
    };
    let (holder, pasta) = kept_going(&network);
    let pasta = pasta.expect("pasta runs for the network");
    // SAFETY: a signal to a child of this process's network.
    unsafe { libc::kill(pasta as i32, libc::SIGKILL) };
    let again = eventually(|| kept_going(&network).1.filter(|again| *again != pasta))
        .expect("pasta was started again");
    let line = crate::listening::command_line(again).unwrap_or_default();
    assert!(line.ends_with(&format!(" {holder}")), "{line}");
    assert!(!line.contains("--config-net"), "{line}");
    assert!(!has_default_route(holder));
}

/// What `found` gives within ten seconds, looking every tenth of one.
fn eventually<T>(mut found: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = found() {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A python3 server on 127.0.0.1, at a port of the system's choosing, that
/// says what it's given to each connection and closes it.
const SERVER: &str = "import socket, sys
s = socket.socket()
s.bind(('127.0.0.1', 0))
s.listen()
print(s.getsockname()[1], flush=True)
while True:
    c, _ = s.accept()
    c.sendall(sys.argv[1].encode())
    c.close()
";

/// [`SERVER`] running, in a network or on this computer: ended when it's
/// dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// One saying `said`, in `network` (None: on this computer). None
    /// where python3 isn't installed, and the test is then skipped.
    fn start(network: Option<&Network>, said: &str) -> Option<Server> {
        if !crate::process::on_path("python3") {
            eprintln!("python3 isn't installed: skipped");
            return None;
        }
        let mut cmd = Command::new("python3");
        cmd.args(["-c", SERVER, said])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        if let Some(network) = network {
            network.join(&mut cmd);
        }
        let mut child = cmd.spawn().unwrap();
        let mut line = String::new();
        io::BufRead::read_line(
            &mut io::BufReader::new(child.stdout.as_mut().unwrap()),
            &mut line,
        )
        .unwrap();
        let port = line.trim().parse().expect("the server's port");
        Some(Server { child, port })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a connection said before it closed; None when there was none.
fn said(stream: io::Result<std::net::TcpStream>) -> Option<String> {
    let mut said = String::new();
    stream.ok()?.read_to_string(&mut said).ok()?;
    (!said.is_empty()).then_some(said)
}

/// The ports something on this computer listens on now.
fn listened_here() -> Vec<u16> {
    crate::listening::sockets(Path::new("/proc/self/net"))
        .unwrap()
        .into_values()
        .collect()
}

#[test]
fn a_bridge_carries_each_loopbacks_servers_to_the_other_while_they_listen() {
    let Some((_dir, network)) = sealed_for_test() else {
        return;
    };
    let network = Arc::new(network);
    let (Some(inside), Some(here)) = (
        Server::start(Some(&network), "inside"),
        Server::start(None, "here"),
    ) else {
        return;
    };
    if listened_here().contains(&inside.port) || network.listening_ports().contains(&here.port) {
        eprintln!("a port is busy on both sides: skipped");
        return;
    }
    let _bridge = Bridge::start(Arc::downgrade(&network), Vec::new());
    let reached_here = || said(std::net::TcpStream::connect(("127.0.0.1", inside.port)));
    assert_eq!(eventually(reached_here).as_deref(), Some("inside"));
    let reached_inside = || said(network.connect(here.port));
    assert_eq!(eventually(reached_inside).as_deref(), Some("here"));

    // Gone on one side, gone on the other.
    let (inside_port, here_port) = (inside.port, here.port);
    drop((inside, here));
    let refused =
        |error: io::Error| (error.kind() == io::ErrorKind::ConnectionRefused).then_some(());
    let closed_here = || {
        std::net::TcpStream::connect(("127.0.0.1", inside_port))
            .err()
            .and_then(refused)
    };
    assert!(
        eventually(closed_here).is_some(),
        "{inside_port} still listens here"
    );
    let closed_inside = || network.connect(here_port).err().and_then(refused);
    assert!(
        eventually(closed_inside).is_some(),
        "{here_port} still listens inside"
    );
}

#[test]
fn a_bridge_never_opens_a_proxys_port_or_this_processs_own() {
    let Some((_dir, network)) = sealed_for_test() else {
        return;
    };
    let network = Arc::new(network);
    let (Some(proxy), Some(here)) = (Server::start(None, "proxy"), Server::start(None, "here"))
    else {
        return;
    };
    // This process's own: the filter's inside, an MCP server's here.
    let filter = network.listen().unwrap();
    let filter = filter.local_addr().unwrap().port();
    let mcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mcp = mcp.local_addr().unwrap().port();
    if listened_here().contains(&filter)
        || network
            .listening_ports()
            .iter()
            .any(|port| [proxy.port, here.port, mcp].contains(port))
    {
        eprintln!("a port is busy on both sides: skipped");
        return;
    }
    let _bridge = Bridge::start(Arc::downgrade(&network), vec![proxy.port]);
    // Once a look has opened what it opens.
    let reached_inside = || said(network.connect(here.port));
    assert_eq!(eventually(reached_inside).as_deref(), Some("here"));
    for port in [proxy.port, mcp] {
        let refused = network.connect(port).unwrap_err();
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::ECONNREFUSED),
            "{port}: {refused}"
        );
    }
    let refused = std::net::TcpStream::connect(("127.0.0.1", filter)).unwrap_err();
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ConnectionRefused,
        "{refused}"
    );
}
