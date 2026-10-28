//! Which TCP ports are listened on, and by which processes, from /proc:
//! the browser tools' check that Chrome opened no port (browser_tools.rs),
//! a team server's private network, which names the process holding a
//! port a command found busy (overlay/confine/network.rs), and a desktop's
//! sealed network's bridge (network/bridge.rs). Linux only.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// A TCP socket listening in a network namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listening {
    /// As a descriptor's link names it (`socket:[123]`).
    pub link: String,
    pub address: IpAddr,
    pub port: u16,
}

/// The listening TCP sockets of the network namespace that `net` shows (a
/// `/proc/<pid>/net` folder, or `/proc/self/net` for this process's own).
pub fn listening(net: &Path) -> std::io::Result<Vec<Listening>> {
    let mut listening = Vec::new();
    for table in ["tcp", "tcp6"] {
        let rows = match std::fs::read_to_string(net.join(table)) {
            Ok(rows) => rows,
            // A system without IPv6.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for row in rows.lines().skip(1) {
            let columns: Vec<&str> = row.split_whitespace().collect();
            // The local address (hex ADDRESS:PORT), the state (0A is
            // LISTEN) and the inode.
            if let (Some(local), Some(&"0A"), Some(inode)) =
                (columns.get(1), columns.get(3), columns.get(9))
            {
                if let Some((address, port)) = local_address(local) {
                    listening.push(Listening {
                        link: format!("socket:[{inode}]"),
                        address,
                        port,
                    });
                }
            }
        }
    }
    Ok(listening)
}

/// The listening TCP sockets of the network namespace that `net` shows
/// ([`listening`]): each as a descriptor's link names it, and its port.
pub fn sockets(net: &Path) -> std::io::Result<HashMap<String, u16>> {
    Ok(listening(net)?
        .into_iter()
        .map(|listening| (listening.link, listening.port))
        .collect())
}

/// A /proc/net row's local address (`0100007F:1F90`; IPv6's has 32 hex
/// digits): the kernel writes each 32-bit word in the host's order.
fn local_address(text: &str) -> Option<(IpAddr, u16)> {
    let (address, port) = text.rsplit_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let words: Vec<[u8; 4]> = address
        .as_bytes()
        .chunks(8)
        .map(|word| {
            let word = std::str::from_utf8(word).ok()?;
            u32::from_str_radix(word, 16).ok().map(u32::to_ne_bytes)
        })
        .collect::<Option<_>>()?;
    let address = match words.as_slice() {
        [v4] => IpAddr::V4(Ipv4Addr::from(*v4)),
        [a, b, c, d] => {
            let mut octets = [0u8; 16];
            for (i, word) in [a, b, c, d].into_iter().enumerate() {
                octets[i * 4..i * 4 + 4].copy_from_slice(word);
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some((address, port))
}

/// The sockets this process holds, as their descriptors' links name them.
pub fn own_sockets() -> HashSet<String> {
    let Ok(descriptors) = std::fs::read_dir("/proc/self/fd") else {
        return HashSet::new();
    };
    descriptors
        .flatten()
        .filter_map(|descriptor| std::fs::read_link(descriptor.path()).ok())
        .filter_map(|target| target.to_str().map(str::to_string))
        .filter(|target| target.starts_with("socket:["))
        .collect()
}

/// Those of `pids` that hold one of `sockets`, each with the port: a
/// process that's gone, or whose descriptors this one can't read, holds
/// none it can see.
pub fn holders(
    sockets: &HashMap<String, u16>,
    pids: impl IntoIterator<Item = u32>,
) -> Vec<(u32, u16)> {
    let mut found = Vec::new();
    if sockets.is_empty() {
        return found;
    }
    for pid in pids {
        let Ok(descriptors) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for descriptor in descriptors.flatten() {
            let Ok(target) = std::fs::read_link(descriptor.path()) else {
                continue;
            };
            if let Some(port) = target.to_str().and_then(|target| sockets.get(target)) {
                found.push((pid, *port));
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// Every process /proc lists.
pub fn processes() -> std::io::Result<Vec<u32>> {
    Ok(std::fs::read_dir("/proc")?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect())
}

/// `pid` and every process descended from it.
pub fn process_tree(pid: u32) -> std::io::Result<Vec<u32>> {
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for child in processes()? {
        // The parent is the second field after the command, which ends at
        // the last parenthesis.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) else {
            continue;
        };
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|parent| parent.parse::<u32>().ok());
        if let Some(parent) = parent {
            children.entry(parent).or_default().push(child);
        }
    }
    let mut tree = vec![pid];
    let mut next = 0;
    while next < tree.len() {
        tree.extend(children.get(&tree[next]).into_iter().flatten().copied());
        next += 1;
    }
    Ok(tree)
}

/// Process `pid`'s command line, its arguments joined by spaces: None when
/// it's gone or can't be read.
pub fn command_line(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let words: Vec<String> = raw
        .split(|&byte| byte == 0)
        .filter(|word| !word.is_empty())
        .map(|word| String::from_utf8_lossy(word).into_owned())
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listening_socket_is_found_with_the_process_holding_it() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sockets = sockets(Path::new("/proc/net")).unwrap();
        assert!(sockets.values().any(|&p| p == port), "{sockets:?}");
        let me = std::process::id();
        assert!(holders(&sockets, [me]).contains(&(me, port)));
    }

    #[test]
    fn a_listening_socket_is_found_with_its_address_as_this_processs_own() {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut expected = vec![(IpAddr::V4(Ipv4Addr::LOCALHOST), v4.local_addr().unwrap())];
        // Where the system has IPv6.
        let v6 = std::net::TcpListener::bind("[::1]:0").ok();
        if let Some(v6) = &v6 {
            expected.push((IpAddr::V6(Ipv6Addr::LOCALHOST), v6.local_addr().unwrap()));
        }
        let all = listening(Path::new("/proc/self/net")).unwrap();
        let ours = own_sockets();
        for (address, bound) in expected {
            let found = all
                .iter()
                .find(|l| l.address == address && l.port == bound.port())
                .unwrap_or_else(|| panic!("{bound} in {all:?}"));
            assert!(ours.contains(&found.link), "{found:?} in {ours:?}");
        }
    }

    #[test]
    fn a_child_is_in_its_parents_tree_and_holds_no_socket_of_the_parents() {
        let _listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(process_tree(std::process::id())
            .unwrap()
            .contains(&child.id()));
        let sockets = sockets(Path::new("/proc/net")).unwrap();
        assert_eq!(holders(&sockets, [child.id()]), Vec::new());
        assert_eq!(command_line(child.id()).as_deref(), Some("sleep 30"));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
