//! What a command on a team server is told about its user's private network
//! (D20) when its output shows trouble with a port: a server that couldn't
//! take a busy port is told which of this user's processes holds it, and a
//! connection refused at a local address is told that nothing listens
//! there, and why that may be. Every port in that network is this user's
//! own: another user's servers are never what holds or answers it.
//!
//! What clients say they couldn't reach is read in unreached.rs, on any
//! machine.

use super::unreached::{ports_on, refused_ports, REFUSED};
use crate::overlay::confine::network::Listener;
use regex::Regex;
use std::sync::LazyLock;

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

/// Whether `text`, a command's output, shows trouble with a port: worth
/// looking at what listens in the network for.
pub(super) fn troubled(text: &str) -> bool {
    BUSY.is_match(text) || REFUSED.is_match(text)
}

/// The note for a command's output `text`, given what listens in the
/// user's network now: None when it shows no trouble with a port. A
/// refused port of `services`, the ports the project's services publish,
/// is theirs to explain (environment.rs).
pub(super) fn note(text: &str, listening: &[Listener], services: &[u16]) -> Option<String> {
    let mut notes = Vec::new();
    let (busy, busy_ports) = ports_on(text, &BUSY, &BUSY_PORT);
    if busy {
        notes.push(busy_note(&busy_ports, listening));
    }
    for port in refused_ports(text) {
        if busy_ports.contains(&port) || services.contains(&port) {
            continue;
        }
        notes.push(refused_note(port, listening));
    }
    (!notes.is_empty()).then(|| {
        notes
            .iter()
            .map(|note| format!("\n({note})"))
            .collect::<String>()
    })
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

fn refused_note(port: u16, listening: &[Listener]) -> String {
    match holders(port, listening).as_slice() {
        // A server started by an earlier command is gone with it.
        [] => format!(
            "Nothing listens on port {port} in this user's private network on the server now, \
             so the connection was refused. If this command started the server, it may have \
             needed longer to come up: wait until it answers before testing it. A server a \
             command starts with `&` ends with that command: leave one running with bash's \
             background: true, or start it and test it in the same command, for example `npm \
             run dev > dev.log 2>&1 & sleep 5; curl -s localhost:{port}`. Other users' servers \
             are never on this network's localhost."
        ),
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
    fn a_busy_port_names_the_users_process_holding_it() {
        let listening = [listener(3000, 4242, "node server.js")];
        let said = note(
            "Error: listen EADDRINUSE: address already in use :::3000",
            &listening,
            &[],
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

        let gone = note("Error: Port 5173 is already in use", &[], &[]).unwrap();
        assert!(gone.contains("nothing holds port 5173 now"), "{gone}");
    }

    #[test]
    fn a_busy_port_without_a_number_lists_what_listens() {
        let listening = [listener(5173, 7, "vite"), listener(3000, 8, "node a.js")];
        let said = note(
            "OSError: [Errno 98] Address already in use",
            &listening,
            &[],
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
        let said = note(refused, &[], &[]).unwrap();
        assert!(said.contains("Nothing listens on port 3000"), "{said}");
        assert!(said.contains("ends with that command"), "{said}");
        assert!(said.contains("background: true"), "{said}");
        assert!(said.contains("curl -s localhost:3000"), "{said}");

        let late = note(
            "Error: connect ECONNREFUSED 127.0.0.1:3000",
            &[listener(3000, 9, "next dev")],
            &[],
        )
        .unwrap();
        assert!(late.contains("Port 3000 listens now"), "{late}");
    }

    #[test]
    fn a_port_a_service_publishes_is_left_to_the_service_to_explain() {
        let refused = "Error: connect ECONNREFUSED 127.0.0.1:5432\nError: connect ECONNREFUSED 127.0.0.1:3000";
        let said = note(refused, &[], &[5432]).unwrap();
        assert!(said.contains("Nothing listens on port 3000"), "{said}");
        assert!(!said.contains("5432"), "{said}");
        assert_eq!(
            note("Error: connect ECONNREFUSED 127.0.0.1:5432", &[], &[5432]),
            None
        );
    }

    #[test]
    fn output_without_port_trouble_gets_no_note() {
        assert!(!troubled("Compiled successfully in 1.2s"));
        assert_eq!(note("Compiled successfully in 1.2s", &[], &[]), None);
        assert!(troubled("connect ECONNREFUSED 127.0.0.1:1"));
    }
}
