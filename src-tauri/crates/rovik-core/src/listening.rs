//! Which TCP ports are listened on, and by which processes, from /proc:
//! the browser tools' check that Chrome opened no port (browser_tools.rs),
//! and a team server's private network, which names the process holding a
//! port a command found busy (overlay/confine/network.rs). Linux only.

use std::collections::HashMap;
use std::path::Path;

/// The listening TCP sockets of the network namespace that `net` shows (a
/// `/proc/<pid>/net` folder, or `/proc/net` for this process's own): each
/// as a descriptor's link names it (`socket:[123]`), and its port.
pub fn sockets(net: &Path) -> std::io::Result<HashMap<String, u16>> {
    let mut listening = HashMap::new();
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
                let port = local.rsplit_once(':').map(|(_, port)| port);
                if let Some(port) = port.and_then(|port| u16::from_str_radix(port, 16).ok()) {
                    listening.insert(format!("socket:[{inode}]"), port);
                }
            }
        }
    }
    Ok(listening)
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
