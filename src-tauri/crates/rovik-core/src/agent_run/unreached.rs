//! What a command's output says it couldn't reach: the local ports a
//! connection was refused at ([`refused_ports`]), the host names it
//! couldn't look up ([`unresolved_hosts`]) and a container engine
//! ([`engine_unreachable`]), in the words each kind of client says them. On any machine they tell which of the project's
//! services a command needed (services/, environment.rs); on a team
//! server, what its user's private network has on such a port (ports.rs).

use regex::Regex;
use std::sync::LazyLock;

/// The most ports or names read from one output.
const MOST_PORTS: usize = 3;

/// A line saying a connection was refused, or that a server wasn't there:
/// the database clients say it their own ways (Prisma's "Can't reach
/// database server", libpq's "Is the server running on host", MySQL's,
/// JDBC's "Connection to localhost:5432 refused", Kafka's "could not be
/// established").
pub(super) static REFUSED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)ECONNREFUSED|connection refused|couldn't connect to server|can't reach database server|is the server running on (?:that )?host|can't connect to mysql server on|connection to \S+ refused|could not be established",
    )
    .unwrap()
});

/// A local address's port on a refused line: `127.0.0.1:3000`,
/// `localhost port 3000` (curl), `|127.0.0.1|:3000` (wget),
/// `host='localhost', port=3000` (Python's urllib3), `` `localhost`:`5432` ``
/// (Prisma), `"localhost" (127.0.0.1), port 5432` (libpq).
static REFUSED_PORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:localhost|\b127\.\d{1,3}\.\d{1,3}\.\d{1,3}|\[::1\]|::1)(?::|\s+port\s+|\|:)(\d{1,5})\b|host='(?:localhost|127\.\d{1,3}\.\d{1,3}\.\d{1,3}|::1)',\s*port=(\d{1,5})|`(?:localhost|127\.\d{1,3}\.\d{1,3}\.\d{1,3}|::1)`:`(\d{1,5})`|"(?:localhost|127\.\d{1,3}\.\d{1,3}\.\d{1,3}|::1)"\s*(?:\([^)]*\))?,\s*port\s+(\d{1,5})"#,
    )
    .unwrap()
});

/// libpq's older message, whose port is on the line after the host's:
/// `Is the server running on host "localhost" (127.0.0.1) and accepting` /
/// `TCP/IP connections on port 5432?`
static LIBPQ_PORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)running on host "(?:localhost|127\.\d{1,3}\.\d{1,3}\.\d{1,3}|::1)"[^\n]*\n\s*TCP/IP connections on port (\d{1,5})"#,
    )
    .unwrap()
});

/// MySQL's message, which names the port only when it isn't MySQL's own:
/// `Can't connect to MySQL server on 'localhost:3307' (111)`, or on
/// `'127.0.0.1' (111)`, port 3306.
static MYSQL_LOCAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)can't connect to mysql server on '(?:localhost|127\.0\.0\.1|::1)(?::(\d{1,5}))?'",
    )
    .unwrap()
});

/// A host name a client couldn't look up: Node's `getaddrinfo ENOTFOUND db`,
/// libpq's `could not translate host name "db"`, Go's `lookup db on
/// 127.0.0.53:53: no such host`, Java's `UnknownHostException: db`, MySQL's
/// `Unknown MySQL server host 'db'`, urllib3's `Failed to resolve 'db'`, and
/// Prisma's `Can't reach database server at `db``.
static UNRESOLVED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)getaddrinfo (?:ENOTFOUND|EAI_AGAIN) ([a-z0-9][a-z0-9_.-]*)|could not translate host name "([^"\s]+)"|lookup ([a-z0-9][a-z0-9_.-]*)(?: on [^:\s]+:\d+)?: no such host|UnknownHostException: ([a-z0-9][a-z0-9_.-]*)|Unknown MySQL server host '([^'\s]+)'|Failed to resolve '([^'\s]+)'|Can't reach database server at `([a-z][a-z0-9_.-]*)`"#,
    )
    .unwrap()
});

/// A container engine that wasn't there or couldn't be reached, as its
/// clients and the shell say it: `docker: command not found`, `Cannot
/// connect to the Docker daemon`, its socket missing or refused, Podman's
/// `Cannot connect to Podman`, or rootless Podman unable to map its users
/// (`newuidmap`) or to write its storage (`…/containers/storage…: read-only
/// file system`), as in the sandbox.
static ENGINE_UNREACHABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)cannot connect to the docker daemon|docker\.sock|podman\.sock|cannot connect to podman|\b(?:docker|podman|docker-compose|podman-compose):? (?:command )?not found|command not found: (?:docker|podman)|newuidmap|uid_map|(?:containers|libpod)[^\n]*read-only file system",
    )
    .unwrap()
});

/// Whether `text`, a command's output, says a container engine wasn't
/// there or couldn't be reached.
pub(crate) fn engine_unreachable(text: &str) -> bool {
    ENGINE_UNREACHABLE.is_match(text)
}

/// The local ports `text`, a command's output, says a connection was
/// refused at: the first few, once each.
pub(crate) fn refused_ports(text: &str) -> Vec<u16> {
    let (_, mut ports) = ports_on(text, &REFUSED, &REFUSED_PORT);
    let more = LIBPQ_PORT
        .captures_iter(text)
        .filter_map(|c| c.get(1)?.as_str().parse::<u16>().ok())
        .chain(MYSQL_LOCAL.captures_iter(text).map(|c| {
            c.get(1)
                .and_then(|port| port.as_str().parse::<u16>().ok())
                .unwrap_or(3306)
        }));
    for port in more {
        if port != 0 && !ports.contains(&port) && ports.len() < MOST_PORTS {
            ports.push(port);
        }
    }
    ports
}

/// The host names `text`, a command's output, says couldn't be looked up,
/// lowercase, once each: a service a compose file names, perhaps, which
/// only the project's other containers find by its name.
pub(crate) fn unresolved_hosts(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for captures in UNRESOLVED.captures_iter(text) {
        let Some(name) = captures.iter().skip(1).flatten().next() else {
            continue;
        };
        let name = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        if name != "localhost" && !names.contains(&name) && names.len() < MOST_PORTS {
            names.push(name);
        }
    }
    names
}

/// Whether a line of `text` matches `line`, and the ports `port` finds on
/// such lines: the first few, once each.
pub(super) fn ports_on(text: &str, line: &Regex, port: &Regex) -> (bool, Vec<u16>) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn database_clients_say_their_refused_port_their_own_ways() {
        for (said, port) in [
            ("Error: P1001: Can't reach database server at `localhost`:`5432`", 5432),
            ("psycopg2.OperationalError: connection to server at \"localhost\" (127.0.0.1), port 5432 failed: Connection refused", 5432),
            ("could not connect to server: Connection refused\n\tIs the server running on host \"localhost\" (127.0.0.1) and accepting\n\tTCP/IP connections on port 5433?", 5433),
            ("ERROR 2003 (HY000): Can't connect to MySQL server on '127.0.0.1' (111)", 3306),
            ("ERROR 2003 (HY000): Can't connect to MySQL server on 'localhost:3307' (111)", 3307),
            ("org.postgresql.util.PSQLException: Connection to localhost:5432 refused. Check that the hostname and port are correct", 5432),
            ("[Producer clientId=producer-1] Connection to node -1 (localhost/127.0.0.1:9092) could not be established. Broker may not be available.", 9092),
            ("redis.exceptions.ConnectionError: Error 111 connecting to localhost:6379. Connection refused.", 6379),
            ("MongoServerSelectionError: connect ECONNREFUSED 127.0.0.1:27017", 27017),
        ] {
            assert_eq!(refused_ports(said), [port], "{said}");
        }
        // A database on another host isn't a local service's.
        let remote = "could not connect to server: Connection refused\n\tIs the server running on host \"db.example.com\" (10.0.0.5) and accepting\n\tTCP/IP connections on port 5432?";
        assert_eq!(refused_ports(remote), [] as [u16; 0]);
    }

    #[test]
    fn a_host_a_client_could_not_look_up_is_read_from_what_it_says() {
        for (said, name) in [
            ("Error: getaddrinfo ENOTFOUND db", "db"),
            ("Error: getaddrinfo EAI_AGAIN redis", "redis"),
            ("psycopg2.OperationalError: could not translate host name \"postgres\" to address: Name or service not known", "postgres"),
            ("dial tcp: lookup cache on 127.0.0.53:53: no such host", "cache"),
            ("dial tcp: lookup mq: no such host", "mq"),
            ("java.net.UnknownHostException: kafka", "kafka"),
            ("ERROR 2005 (HY000): Unknown MySQL server host 'mysql' (-2)", "mysql"),
            ("NameResolutionError: Failed to resolve 'search' ([Errno -2] Name or service not known)", "search"),
            ("Error: P1001: Can't reach database server at `db`:`5432`", "db"),
        ] {
            assert_eq!(unresolved_hosts(said), [name], "{said}");
        }
        assert_eq!(
            unresolved_hosts("Error: P1001: Can't reach database server at `localhost`:`5432`"),
            [] as [&str; 0]
        );
        assert_eq!(unresolved_hosts("all good"), [] as [&str; 0]);
    }

    #[test]
    fn an_engine_that_isnt_there_or_cant_be_reached_is_read_from_what_it_says() {
        for said in [
            "bash: line 1: docker: command not found",
            "sh: 1: docker-compose: not found",
            "zsh: command not found: podman",
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?",
            "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock",
            "ls: cannot access '/var/run/docker.sock': No such file or directory",
            "Cannot connect to Podman. Please verify your connection to the Linux system",
            "newuidmap: write to uid_map failed: Operation not permitted",
            "Error: creating runtime static files directory \"/home/dev/.local/share/containers/storage/libpod\": mkdir /home/dev/.local: read-only file system",
        ] {
            assert!(engine_unreachable(said), "{said}");
        }
        for said in [
            "ERROR: failed to solve: dockerfile parse error on line 3",
            "Error response from daemon: No such container: web",
            "npm ERR! missing script: docker",
            "touch: cannot touch 'dist/app.js': Read-only file system",
        ] {
            assert!(!engine_unreachable(said), "{said}");
        }
    }
}
