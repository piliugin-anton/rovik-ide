//! Which chats are on a host. Besides moving there (M06), the developer
//! setting `ROVIK_REMOTE_CHATS=<chat>=<socket>,…` puts chats on the daemons
//! at those sockets, with no server, as the database keeps them from then
//! on (M01): the app connects to each as `ROVIK_COMPUTER` (default
//! "desktop"), with the token in `ROVIK_REMOTE_TOKEN` that the daemon's
//! config.toml names it by. The hosts' summaries list each by its socket's
//! file name (`HostsPage::summaries`), so its chats show its connection,
//! and others can move there.

use rovik_client::{Identity, Remote};
use rovik_protocol::Secret;
use std::collections::BTreeSet;

/// The chats the developer setting names, on their daemons.
pub fn from_env(remote: &Remote) {
    let Ok(setting) = std::env::var("ROVIK_REMOTE_CHATS") else {
        return;
    };
    let token = std::env::var("ROVIK_REMOTE_TOKEN").unwrap_or_default();
    let computer = std::env::var("ROVIK_COMPUTER").unwrap_or_else(|_| "desktop".into());
    let chats = parse(&setting);
    let sockets: BTreeSet<&str> = chats.iter().map(|(_, socket)| socket.as_str()).collect();
    for socket in sockets {
        let identity = Identity {
            host: socket.to_string(),
            computer: computer.clone(),
            token: Secret(token.clone()),
            client: format!("Rovik {}", env!("CARGO_PKG_VERSION")),
        };
        connect(remote, socket, identity);
    }
    for (chat, socket) in &chats {
        remote.place(chat, socket);
    }
}

#[cfg(unix)]
fn connect(remote: &Remote, socket: &str, identity: Identity) {
    let connector = rovik_client::UnixConnector(socket.into());
    remote.connect(socket, connector, identity);
}

#[cfg(not(unix))]
fn connect(_: &Remote, socket: &str, _: Identity) {
    eprintln!("ROVIK_REMOTE_CHATS: {socket}: a daemon's socket needs Linux or macOS");
}

/// `<chat>=<socket>` pairs, separated by commas.
fn parse(setting: &str) -> Vec<(String, String)> {
    setting
        .split(',')
        .filter_map(|pair| {
            let (chat, socket) = pair.split_once('=')?;
            let (chat, socket) = (chat.trim(), socket.trim());
            (!chat.is_empty() && !socket.is_empty()).then(|| (chat.into(), socket.into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_pairs_chats_with_sockets() {
        let pairs = parse(" a=/run/one.sock, b = /run/two.sock ,broken,=x, c=/run/one.sock ");
        let expected = [
            ("a", "/run/one.sock"),
            ("b", "/run/two.sock"),
            ("c", "/run/one.sock"),
        ]
        .map(|(chat, socket)| (chat.to_string(), socket.to_string()));
        assert_eq!(pairs, expected);
    }
}
