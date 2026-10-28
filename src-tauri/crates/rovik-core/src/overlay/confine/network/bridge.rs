//! A desktop's sealed network, its loopback bridged with this computer's
//! (docs/design/allowed-hosts-plan.md, J06).
//!
//! On a desktop, and in WSL, the agent's processes start in a sealed network
//! of their own while the allowed hosts are on (J05): its loopback isn't
//! this computer's. The user still expects the agent's dev server at
//! `localhost:5173` in the editor's browser tab and their own browser, and
//! the agent's tests expect the database the user runs at `localhost:5432`.
//! So Rovik bridges the two loopbacks itself, looking every second:
//!
//! - a port something in the network listens on is opened on this
//!   computer's 127.0.0.1 and ::1, each connection carried in
//!   ([`Network::connect`]);
//! - a port this computer listens on (127.0.0.1, ::1, or every address)
//!   is opened on the network's loopback ([`Network::listen_on`]), each
//!   connection carried to it;
//! - but never a port this process holds (the bridge's own, the filter's,
//!   an MCP server's), DNS's (53), or a proxy's (`closed`): each would be a
//!   way around the list.
//!
//! pasta's own forwarding would open the network's ports on every address
//! of the computer, putting the agent's servers on the LAN: here they're
//! reached from this computer alone. A port taken on one side stays taken
//! on the other, as on one loopback.

use super::Network;
use crate::listening::Listening;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Weak;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

/// How often the bridge looks at what listens on each side.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// DNS's port: a name looked up through it could carry data out.
const DNS: u16 = 53;

/// A network's bridge, until it's dropped or the network is gone: every
/// port it opened closes with it.
pub(super) struct Bridge(JoinHandle<()>);

impl Bridge {
    /// Bridges `network` with this computer's loopback but for the ports in
    /// `closed`, the proxies' here.
    pub(super) fn start(network: Weak<Network>, closed: Vec<u16>) -> Bridge {
        Bridge(crate::rt::spawn(bridge(network, closed)))
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The listeners one side's port was opened with on the other, serving
/// until dropped.
struct Opened(Vec<JoinHandle<()>>);

impl Drop for Opened {
    fn drop(&mut self) {
        for serving in &self.0 {
            serving.abort();
        }
    }
}

/// What a look found, and opened for what's new.
#[derive(Default)]
struct Looked {
    /// The ports something in the network listens on.
    inside: HashSet<u16>,
    /// The ports this computer listens on, each with where to reach it.
    here: HashMap<u16, SocketAddr>,
    /// Listeners opened here for new ports of the network's.
    outward: Vec<(u16, Vec<TcpListener>)>,
    /// Listeners opened in the network for new ports of this computer's.
    inward: Vec<(u16, SocketAddr, Vec<TcpListener>)>,
}

/// The ports each side has opened for the other, and the ones it couldn't
/// open (taken there), which aren't tried again while they stay.
#[derive(Default)]
struct Bridged {
    outward: HashMap<u16, Opened>,
    inward: HashMap<u16, Opened>,
    blocked_out: HashSet<u16>,
    blocked_in: HashSet<u16>,
}

async fn bridge(network: Weak<Network>, closed: Vec<u16>) {
    let mut bridged = Bridged::default();
    loop {
        let Some(net) = network.upgrade() else {
            return;
        };
        let known = (bridged.ports_out(), bridged.ports_in(), closed.clone());
        let looked = crate::rt::spawn_blocking(move || look(&net, known)).await;
        let Ok(looked) = looked else {
            return;
        };
        bridged.follow(looked, &network);
        tokio::time::sleep(LOOK_EVERY).await;
    }
}

impl Bridged {
    /// The ports opened or blocked outward: not to be tried again.
    fn ports_out(&self) -> HashSet<u16> {
        self.outward
            .keys()
            .chain(&self.blocked_out)
            .copied()
            .collect()
    }

    fn ports_in(&self) -> HashSet<u16> {
        self.inward
            .keys()
            .chain(&self.blocked_in)
            .copied()
            .collect()
    }

    /// What's gone closed, what's new served.
    fn follow(&mut self, looked: Looked, network: &Weak<Network>) {
        self.outward.retain(|port, _| looked.inside.contains(port));
        self.blocked_out.retain(|port| looked.inside.contains(port));
        self.inward.retain(|port, _| looked.here.contains_key(port));
        self.blocked_in
            .retain(|port| looked.here.contains_key(port));
        for (port, listeners) in looked.outward {
            if listeners.is_empty() {
                self.blocked_out.insert(port);
                continue;
            }
            let serving = listeners
                .into_iter()
                .filter_map(|listener| {
                    let network = network.clone();
                    serve(listener, move |here| {
                        into_network(here, network.clone(), port)
                    })
                })
                .collect();
            self.outward.insert(port, Opened(serving));
        }
        for (port, to, listeners) in looked.inward {
            if listeners.is_empty() {
                self.blocked_in.insert(port);
                continue;
            }
            let serving = listeners
                .into_iter()
                .filter_map(|listener| serve(listener, move |inside| out_of_network(inside, to)))
                .collect();
            self.inward.insert(port, Opened(serving));
        }
    }
}

/// What listens on each side now, but this process's own, and listeners
/// opened for each port new to the bridge (`known`: those opened or
/// blocked out, then in, and the ports never opened in).
fn look(network: &Network, known: (HashSet<u16>, HashSet<u16>, Vec<u16>)) -> Looked {
    let (known_out, known_in, closed) = known;
    let ours = crate::listening::own_sockets();
    let not_ours = |all: Vec<Listening>| -> Vec<Listening> {
        all.into_iter()
            .filter(|l| !ours.contains(&l.link))
            .collect()
    };
    let holder = network.holder.load(Ordering::Relaxed);
    let inside_net = PathBuf::from(format!("/proc/{holder}/net"));
    let inside = not_ours(crate::listening::listening(&inside_net).unwrap_or_default());
    let here =
        not_ours(crate::listening::listening(Path::new("/proc/self/net")).unwrap_or_default());
    let mut looked = Looked {
        inside: inside.iter().map(|l| l.port).collect(),
        here: reachable_here(&here),
        ..Looked::default()
    };
    for port in looked
        .inside
        .iter()
        .copied()
        .filter(|p| !known_out.contains(p))
    {
        looked.outward.push((port, listen_here(port)));
    }
    for (port, to) in to_open_inside(&looked.here, &known_in, &closed) {
        let listeners = network.listen_on(port).unwrap_or_default();
        looked.inward.push((port, to, listeners));
    }
    looked
}

/// Which of the ports this computer listens on (`here`) to open in the
/// network now: those not opened or blocked there already (`known`), but
/// never DNS's or one of `closed`.
fn to_open_inside(
    here: &HashMap<u16, SocketAddr>,
    known: &HashSet<u16>,
    closed: &[u16],
) -> Vec<(u16, SocketAddr)> {
    here.iter()
        .filter(|(port, _)| !known.contains(port) && **port != DNS && !closed.contains(port))
        .map(|(port, to)| (*port, *to))
        .collect()
}

/// Where each port this computer listens on for its loopback is reached:
/// 127.0.0.1 for one on 127.0.0.1 or every IPv4 address, ::1 for one on
/// IPv6's alone. One on another address (a resolver's 127.0.0.53, the
/// LAN's) isn't the loopback's.
fn reachable_here(listening: &[Listening]) -> HashMap<u16, SocketAddr> {
    let mut here: HashMap<u16, SocketAddr> = HashMap::new();
    for l in listening {
        let to = match l.address {
            IpAddr::V4(v4) if v4 == Ipv4Addr::LOCALHOST || v4.is_unspecified() => {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            }
            IpAddr::V6(v6) if v6 == Ipv6Addr::LOCALHOST || v6.is_unspecified() => {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            }
            _ => continue,
        };
        // IPv4 first, where both are.
        let entry = here.entry(l.port).or_insert(SocketAddr::new(to, l.port));
        if to.is_ipv4() {
            *entry = SocketAddr::new(to, l.port);
        }
    }
    here
}

/// Listeners on this computer's 127.0.0.1 and ::1 for `port`; none when
/// 127.0.0.1's is taken here.
fn listen_here(port: u16) -> Vec<TcpListener> {
    let Ok(v4) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
        return Vec::new();
    };
    let mut listeners = vec![v4];
    listeners.extend(TcpListener::bind((Ipv6Addr::LOCALHOST, port)).ok());
    listeners
}

/// Serves `listener`, each connection it takes carried by `carry`: None
/// when it can't be served.
fn serve<F, Fut>(listener: TcpListener, carry: F) -> Option<JoinHandle<()>>
where
    F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    listener.set_nonblocking(true).ok()?;
    let listener = {
        let _runtime = crate::rt::handle().enter();
        tokio::net::TcpListener::from_std(listener).ok()?
    };
    Some(crate::rt::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            crate::rt::spawn(carry(stream));
        }
    }))
}

/// A connection from this computer to `port` of the network's, carried in.
async fn into_network(mut here: TcpStream, network: Weak<Network>, port: u16) {
    let Some(network) = network.upgrade() else {
        return;
    };
    let inside = crate::rt::spawn_blocking(move || network.connect(port)).await;
    let Ok(Ok(inside)) = inside else {
        return;
    };
    if inside.set_nonblocking(true).is_err() {
        return;
    }
    let Ok(mut inside) = TcpStream::from_std(inside) else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut here, &mut inside).await;
}

/// A connection from the network to this computer's `to`, carried out.
async fn out_of_network(mut inside: TcpStream, to: SocketAddr) {
    let Ok(mut here) = TcpStream::connect(to).await else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut inside, &mut here).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listening(address: &str, port: u16) -> Listening {
        Listening {
            link: format!("socket:[{port}]"),
            address: address.parse().unwrap(),
            port,
        }
    }

    #[test]
    fn only_this_computers_loopback_servers_are_bridged_in_at_their_address() {
        let here = reachable_here(&[
            listening("127.0.0.1", 5432),
            listening("0.0.0.0", 3000),
            listening("::1", 8080),
            listening("::", 9000),
            // A server on both: reached on IPv4.
            listening("::1", 5432),
            // Not the loopback's: a resolver's, the LAN's.
            listening("127.0.0.53", 53),
            listening("192.168.1.5", 8443),
        ]);
        let at = |address: &str, port: u16| SocketAddr::new(address.parse().unwrap(), port);
        let expected: HashMap<u16, SocketAddr> = [
            (5432, at("127.0.0.1", 5432)),
            (3000, at("127.0.0.1", 3000)),
            (8080, at("::1", 8080)),
            (9000, at("::1", 9000)),
        ]
        .into();
        assert_eq!(here, expected);
    }

    #[test]
    fn dns_a_proxys_port_and_a_known_one_are_never_opened_inside() {
        let at = |port: u16| (port, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let here: HashMap<u16, SocketAddr> = [at(53), at(3128), at(5432), at(6379)].into();
        let known: HashSet<u16> = [6379].into();
        assert_eq!(to_open_inside(&here, &known, &[3128]), vec![at(5432)]);
    }
}
