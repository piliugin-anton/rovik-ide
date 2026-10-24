//! What a command on a team server is told about its user's private network
//! (D20) when its output shows trouble with a port: a server that couldn't
//! take a busy port is told which of this user's processes holds it, and a
//! connection refused at a local address is told that nothing listens
//! there, and why that may be. Every port in that network is this user's
//! own: another user's servers are never what holds or answers it.

use crate::overlay::confine::network::Listener;
use regex::Regex;
use std::sync::LazyLock;

/// The most ports a note names.
const MOST_PORTS: usize = 3;

/// A line saying a server couldn't take its port.
static BUSY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)EADDRINUSE|address already in use|\bport\s+\d{1,5}\s+is\s+(?:already\s+)?in\s+use|:\d{1,5}/?\s+is\s+already\s+used|that port is already in use",
    )
    .unwrap()
});

/// A port named on a busy line: after an address and a colon
/// (`127.0.0.1:3000`, `:::3000`, `[::1]:3000`, `tcp :8080`), or after
/// "port" (`port 3000`, `port=3000`).
static BUSY_PORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\b\d{1,3}(?:\.\d{1,3}){3}|\blocalhost|\]|::|\*|\s):(\d{2,5})\b|\bport\s*[=:]?\s*(\d{2,5})\b",
    )
    .unwrap()
});

/// A line saying a connection was refused.
static REFUSED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)ECONNREFUSED|connection refused|couldn't connect to server").unwrap()
});

/// A local address's port on a refused line: `127.0.0.1:3000`,
/// `localhost port 3000` (curl), `|127.0.0.1|:3000` (wget),
/// `host='localhost', port=3000` (Python's urllib3).
static REFUSED_PORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:localhost|\b127\.\d{1,3}\.\d{1,3}\.\d{1,3}|\[::1\]|::1)(?::|\s+port\s+|\|:)(\d{1,5})\b|host='(?:localhost|127\.\d{1,3}\.\d{1,3}\.\d{1,3}|::1)',\s*port=(\d{1,5})",
    )
    .unwrap()
});

/// Whether `text`, a command's output, shows trouble with a port: worth
/// looking at what listens in the network for.
pub(super) fn troubled(text: &str) -> bool {
    BUSY.is_match(text) || REFUSED.is_match(text)
}

/// The note for a command's output `text`, given what listens in the
/// user's network now, and whether what a command leaves running ends with
/// it (`ends_with_command`, bubblewrap's sandbox): None when it shows no
/// trouble with a port.
pub(super) fn note(text: &str, listening: &[Listener], ends_with_command: bool) -> Option<String> {
    let mut notes = Vec::new();
    let (busy, busy_ports) = ports_on(text, &BUSY, &BUSY_PORT);
    if busy {
        notes.push(busy_note(&busy_ports, listening));
    }
    let (_, refused_ports) = ports_on(text, &REFUSED, &REFUSED_PORT);
    for port in refused_ports {
        if busy_ports.contains(&port) {
            continue;
        }
        notes.push(refused_note(port, listening, ends_with_command));
    }
    (!notes.is_empty()).then(|| {
        notes
            .iter()
            .map(|note| format!("\n({note})"))
            .collect::<String>()
    })
}

/// Whether a line of `text` matches `line`, and the ports `port` finds on
/// such lines: the first few, once each.
fn ports_on(text: &str, line: &Regex, port: &Regex) -> (bool, Vec<u16>) {
    let mut found = false;
    let mut ports = Vec::new();
    for matching in text.lines().filter(|l| line.is_match(l)) {
        found = true;
        for captures in port.captures_iter(matching) {
            let number = captures.iter().skip(1).flatten().next();
            let Some(port) = number.and_then(|m| m.as_str().parse::<u16>().ok()) else {
                continue;
            };
            if port != 0 && !ports.contains(&port) && ports.len() < MOST_PORTS {
                ports.push(port);
            }
        }
    }
    (found, ports)
}

/// Who holds `port`, in words: each process with its command line.
fn holders(port: u16, listening: &[Listener]) -> Vec<String> {
    listening
        .iter()
        .filter(|l| l.port == port)
        .map(|l| match &l.command {
            Some(command) => format!("`{}` (pid {})", short(command), l.pid),
            None => format!("pid {}", l.pid),
        })
        .collect()
}

fn busy_note(ports: &[u16], listening: &[Listener]) -> String {
    let named: Vec<String> = ports
        .iter()
        .map(|&port| match holders(port, listening).as_slice() {
            [] => format!(
                "nothing holds port {port} now: what held it has ended, maybe with this command, \
                 so run it again"
            ),
            held => format!(
                "port {port} is held by {}, a process of this user's (another chat's command, or \
                 one this chat left running): use another port, or stop that process if it's this \
                 chat's own",
                held.join(" and ")
            ),
        })
        .collect();
    if !named.is_empty() {
        return format!(
            "A server found its port busy. In this user's private network on the server, {}. \
             Another user's servers are never on this network's ports.",
            named.join("; ")
        );
    }
    // No port in the output: what listens, to pick another.
    let mut all: Vec<&Listener> = listening.iter().collect();
    all.sort_by_key(|l| l.port);
    all.dedup_by_key(|l| l.port);
    match all.as_slice() {
        [] => "A server found its port busy, but nothing listens in this user's private network \
               on the server now: what held it has ended, maybe with this command, so run it \
               again."
            .to_string(),
        all => format!(
            "A server found its port busy. In this user's private network on the server these \
             listen now, all this user's: {}. Use a port none of them holds.",
            all.iter()
                .map(|l| format!("{} {}", l.port, holders(l.port, listening).join(", ")))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn refused_note(port: u16, listening: &[Listener], ends_with_command: bool) -> String {
    match holders(port, listening).as_slice() {
        [] => {
            // Where what a command leaves running ends with it, a server
            // started by an earlier command is gone.
            let ended = if ends_with_command {
                format!(
                    " A server a command starts in the background ends with that command, so \
                     start it and test it in the same command, for example `npm run dev > \
                     dev.log 2>&1 & sleep 5; curl -s localhost:{port}`."
                )
            } else {
                String::new()
            };
            format!(
                "Nothing listens on port {port} in this user's private network on the server \
                 now, so the connection was refused. If this command started the server, it may \
                 have needed longer to come up: wait until it answers before testing it.{ended} \
                 Other users' servers are never on this network's localhost."
            )
        }
        held => format!(
            "Port {port} listens now in this user's private network on the server ({}), but \
             refused the connection when the command made it: the server may have needed longer \
             to come up. Connect again once it answers.",
            held.join(" and ")
        ),
    }
}

/// `command`, cut to a line's worth.
fn short(command: &str) -> String {
    const MOST: usize = 80;
    match command.char_indices().nth(MOST) {
        Some((at, _)) => format!("{}…", &command[..at]),
        None => command.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(port: u16, pid: u32, command: &str) -> Listener {
        Listener {
            port,
            pid,
            command: Some(command.to_string()),
        }
    }

    #[test]
    fn the_busy_port_is_read_from_what_servers_say() {
        for (said, port) in [
            ("Error: listen EADDRINUSE: address already in use :::3000", 3000),
            ("Error: listen EADDRINUSE: address already in use 127.0.0.1:5173", 5173),
            ("listen tcp :8080: bind: address already in use", 8080),
            ("listen tcp 127.0.0.1:9000: bind: address already in use", 9000),
            ("Error: Port 5173 is already in use", 5173),
            ("Port 3000 is in use, trying another one...", 3000),
            ("Address already in use - bind(2) for \"127.0.0.1\" port 3001 (Errno::EADDRINUSE)", 3001),
            ("Error: http://localhost:3000 is already used, make sure that nothing is running on the port/url", 3000),
            ("[12:34:56] Error: listen EADDRINUSE: address already in use [::1]:4200", 4200),
        ] {
            assert_eq!(ports_on(said, &BUSY, &BUSY_PORT), (true, vec![port]), "{said}");
        }
        // Python names no port: it's busy all the same.
        assert_eq!(
            ports_on(
                "OSError: [Errno 98] Address already in use",
                &BUSY,
                &BUSY_PORT
            ),
            (true, vec![])
        );
        assert_eq!(ports_on("all good", &BUSY, &BUSY_PORT), (false, vec![]));
    }

    #[test]
    fn a_refused_local_port_is_read_from_what_clients_say() {
        for (said, port) in [
            ("Error: connect ECONNREFUSED 127.0.0.1:3000", 3000),
            ("curl: (7) Failed to connect to localhost port 5173 after 0 ms: Couldn't connect to server", 5173),
            ("curl: (7) Failed to connect to 127.0.0.1 port 8000: Connection refused", 8000),
            ("Connecting to localhost (localhost)|127.0.0.1|:8080... failed: Connection refused.", 8080),
            ("requests.exceptions.ConnectionError: HTTPConnectionPool(host='localhost', port=9000): Max retries exceeded with url: / (Caused by NewConnectionError('Failed to establish a new connection: [Errno 111] Connection refused'))", 9000),
            ("Get \"http://127.0.0.1:4000/health\": dial tcp 127.0.0.1:4000: connect: connection refused", 4000),
            ("error sending request for url (http://[::1]:3333/): Connection refused (os error 111)", 3333),
        ] {
            assert_eq!(ports_on(said, &REFUSED, &REFUSED_PORT), (true, vec![port]), "{said}");
        }
        // A remote host refusing isn't the network's business.
        let remote = "curl: (7) Failed to connect to example.com port 443: Connection refused";
        assert_eq!(ports_on(remote, &REFUSED, &REFUSED_PORT), (true, vec![]));
    }

    #[test]
    fn a_busy_port_names_the_users_process_holding_it() {
        let listening = [listener(3000, 4242, "node server.js")];
        let said = note(
            "Error: listen EADDRINUSE: address already in use :::3000",
            &listening,
            true,
        )
        .unwrap();
        assert!(
            said.starts_with("\n(A server found its port busy."),
            "{said}"
        );
        assert!(
            said.contains("port 3000 is held by `node server.js` (pid 4242)"),
            "{said}"
        );
        assert!(
            said.contains("Another user's servers are never on this network's ports"),
            "{said}"
        );

        let gone = note("Error: Port 5173 is already in use", &[], true).unwrap();
        assert!(gone.contains("nothing holds port 5173 now"), "{gone}");
    }

    #[test]
    fn a_busy_port_without_a_number_lists_what_listens() {
        let listening = [listener(5173, 7, "vite"), listener(3000, 8, "node a.js")];
        let said = note(
            "OSError: [Errno 98] Address already in use",
            &listening,
            true,
        )
        .unwrap();
        assert!(
            said.contains(
                "these listen now, all this user's: 3000 `node a.js` (pid 8); 5173 `vite` (pid 7)"
            ),
            "{said}"
        );
    }

    #[test]
    fn a_refused_local_connection_says_nothing_listens_and_why() {
        let refused = "curl: (7) Failed to connect to localhost port 3000 after 0 ms: Couldn't connect to server";
        let said = note(refused, &[], true).unwrap();
        assert!(said.contains("Nothing listens on port 3000"), "{said}");
        assert!(said.contains("ends with that command"), "{said}");
        assert!(said.contains("curl -s localhost:3000"), "{said}");
        // Where what a command leaves running goes on (Landlock), it isn't
        // said to end.
        let lasting = note(refused, &[], false).unwrap();
        assert!(
            lasting.contains("Nothing listens on port 3000"),
            "{lasting}"
        );
        assert!(!lasting.contains("ends with that command"), "{lasting}");

        let late = note(
            "Error: connect ECONNREFUSED 127.0.0.1:3000",
            &[listener(3000, 9, "next dev")],
            true,
        )
        .unwrap();
        assert!(late.contains("Port 3000 listens now"), "{late}");
    }

    #[test]
    fn output_without_port_trouble_gets_no_note() {
        assert!(!troubled("Compiled successfully in 1.2s"));
        assert_eq!(note("Compiled successfully in 1.2s", &[], true), None);
        assert!(troubled("connect ECONNREFUSED 127.0.0.1:1"));
    }
}
