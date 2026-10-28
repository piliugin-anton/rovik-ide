//! The hosts the agent's work may reach (Settings → Permissions & approval
//! → Network): an entry as the settings keep it, and whether a host is on
//! the list (docs/design/allowed-hosts-plan.md, J01).
//!
//! An entry is a name (`docs.rs`, that name alone), a name's subdomains
//! (`*.github.com`: `api.github.com`, not `github.com`) or an address
//! (`10.0.0.5`, `[fd00::1]`). Entries and the hosts checked against them
//! are read as a URL's host is (lower case, punycode, IPv4 in dotted form),
//! so `Docs.RS.` and `docs.rs` are one host. This computer's own services
//! (loopback) are never on the list: they're always reached directly.
//!
//! The frontend keeps a copy of [`normalize`] for the settings page
//! (src/lib/permissions.ts): the cases in test-fixtures/allowed-hosts.json
//! hold both to the same answers.
//!
//! The list the agent's work follows is the process's ([`configure`]): the
//! app's settings, or on a cloud host the last computer's to configure it
//! (J12). While it's on, the agent's browser and commands reach the network
//! through the filter ([`filter`], J03), one listening for each client where
//! that client reaches it ([`filter_port`]). A command's sandbox leaves it
//! no other way out ([`hold`]): on Linux the agent's network is sealed but
//! for the filter (J04-J06), on macOS Seatbelt keeps it to the loopback
//! (J07).

use crate::net::BrowserProxy;
use crate::overlay::confine::network::{self, Network};
use crate::overlay::confine::Confine;
use filter::{Client, Denials, Filter, Routes};
use rovik_protocol::permissions::Permissions;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, LazyLock, Mutex, RwLock, Weak};
use std::time::Instant;

pub mod filter;

/// The longest name a host has.
const MAX_NAME: usize = 253;

/// The list the agent's work follows, as the settings last set it
/// ([`configure`]): None, any host.
static LIST: RwLock<Option<Arc<AllowedHosts>>> = RwLock::new(None);

/// What the filters refused, for the model's notes.
static DENIALS: LazyLock<Arc<Denials>> = LazyLock::new(Default::default);

/// The filters listening now.
static LISTENING: Mutex<Vec<Listening>> = Mutex::new(Vec::new());

/// Taken while the agent's network follows the list ([`follow`]).
static FOLLOWING: Mutex<()> = Mutex::new(());

/// What a held command's `NO_PROXY` holds: this computer's own services,
/// which it reaches directly.
const NO_PROXY: &str = "localhost,127.0.0.1,::1";

/// Variables for a program's environment, as name and value.
pub type Env = Vec<(String, String)>;

/// A filter listening for a client.
struct Listening {
    client: Client,
    /// Where: None, on this computer's 127.0.0.1; else in that network,
    /// whose filters go with it.
    network: Option<Weak<Network>>,
    port: u16,
    serving: tokio::task::JoinHandle<()>,
}

/// The settings the agent's work follows from now on: the app's, or on a
/// cloud host the last computer's to configure it (J12). The agent's
/// network follows the list as it's turned on or off ([`follow`]).
pub fn configure(permissions: &Permissions) {
    let list = AllowedHosts::of(permissions).map(Arc::new);
    let on = list.is_some();
    let was_on =
        std::mem::replace(&mut *LIST.write().unwrap_or_else(|e| e.into_inner()), list).is_some();
    if on != was_on {
        if let Err(why) = follow(true) {
            log::error!("the agent's network couldn't follow the allowed hosts: {why}");
        }
    }
}

/// The agent's network held to the filter while the list is on, let go
/// while it's off (J05): a team server's changes its mode, a desktop's is
/// made or dropped. The list is read as it is now, one change at a time, so
/// the last to run leaves the network as the list is. Where it was turned
/// on or off (`changed`) the agent's background commands stop, but on a
/// team server, whose network stays the same: they'd keep the network they
/// started in. Why the network can't follow, when it can't.
fn follow(changed: bool) -> Result<(), String> {
    let _turn = FOLLOWING.lock().unwrap_or_else(|e| e.into_inner());
    let followed = network::seal_agents(current().is_some(), crate::net::proxies_here());
    if changed && network::current().is_none() {
        crate::command_tool::background::stop_running();
    }
    followed
}

/// The list the agent's work follows now: None while any host may be
/// reached.
pub fn current() -> Option<Arc<AllowedHosts>> {
    LIST.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The hosts the filters refused `client` since `since`, each once: for
/// `chat`, those it named as its own and those no client named.
pub fn refused_since(since: Instant, client: Client, chat: Option<&str>) -> Vec<String> {
    DENIALS.since(since, client, chat)
}

/// The port of the filter for `client` in `network` (None: on this
/// computer's 127.0.0.1), started the first time it's asked for. A
/// network's filters end once the network is gone.
pub fn filter_port(client: Client, network: Option<&Arc<Network>>) -> Result<u16, String> {
    let mut listening = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
    listening.retain(|l| {
        let gone = l.network.as_ref().is_some_and(|n| n.strong_count() == 0);
        if gone {
            l.serving.abort();
        }
        !gone
    });
    let same_place = |l: &Listening| match (&l.network, network) {
        (None, None) => true,
        (Some(theirs), Some(ours)) => std::ptr::eq(theirs.as_ptr(), Arc::as_ptr(ours)),
        _ => false,
    };
    if let Some(found) = listening
        .iter()
        .find(|l| l.client == client && same_place(l))
    {
        return Ok(found.port);
    }
    let couldnt = |e: std::io::Error| format!("The allowed hosts' filter couldn't listen: {e}");
    let socket = match network {
        Some(network) => network.listen(),
        None => std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)),
    }
    .map_err(couldnt)?;
    let port = socket.local_addr().map_err(couldnt)?.port();
    socket.set_nonblocking(true).map_err(couldnt)?;
    let listener = {
        let _runtime = crate::rt::handle().enter();
        tokio::net::TcpListener::from_std(socket).map_err(couldnt)?
    };
    let filter = Arc::new(Filter {
        client,
        list: Arc::new(current),
        upstream: Arc::new(Routes(client)),
        denials: DENIALS.clone(),
    });
    let serving = crate::rt::spawn(filter.serve(listener));
    listening.push(Listening {
        client,
        network: network.map(Arc::downgrade),
        port,
        serving,
    });
    Ok(port)
}

/// A confined command's sandbox and environment (`confine`, for `chat`):
/// while the list is on, held to it, the filter its proxy (J03) and its
/// sandbox leaving it no other way out (J04); as they are while any host
/// may be reached. Why not, when the list is on but the command can't be
/// held to it here.
pub fn hold(confine: Option<Confine>, chat: &str) -> Result<(Option<Confine>, Env), String> {
    let Some(list) = current() else {
        return Ok((confine, Vec::new()));
    };
    let Some(confine) = confine else {
        return Err(format!(
            "The user's allowed hosts are on ({}), and the agent's commands reach the network only \
             through them, from the sandbox that holds them to the list, which doesn't run here: \
             no command can run.",
            list.shown()
        ));
    };
    let (confine, network) = held(confine)?;
    let port = filter_port(Client::Commands, network.as_ref())?;
    Ok((Some(confine), proxy_env(chat, port)))
}

/// `confine` in the agent's sealed network (J04), and that network.
#[cfg(target_os = "linux")]
fn held(confine: Confine) -> Result<(Confine, Option<Arc<Network>>), String> {
    let network = sealed()?;
    Ok((confine.with_network(network.clone()), Some(network)))
}

/// The agent's sealed network (J04), made again if it couldn't be as the
/// list was turned on.
#[cfg(target_os = "linux")]
fn sealed() -> Result<Arc<Network>, String> {
    follow(false).map_err(|why| {
        format!(
            "The user's allowed hosts are on, and the agent's work reaches the network only \
             through them, from a network of its own, which couldn't be made here: {why}. Ask \
             the user to fix that, or to let the agent reach any host in Settings → Permissions \
             & approval."
        )
    })?;
    network::agents().ok_or_else(|| {
        "The user's allowed hosts were turned off as it started: run it again.".to_string()
    })
}

/// `confine` under Seatbelt's rules for the loopback alone, the proxies'
/// ports there closed too (J07): the filter listens on 127.0.0.1.
#[cfg(target_os = "macos")]
fn held(confine: Confine) -> Result<(Confine, Option<Arc<Network>>), String> {
    Ok((confine.to_loopback(crate::net::proxies_here()), None))
}

/// Nowhere else is a command held to the list: Windows runs the agent's
/// commands in WSL, under Linux's (J08).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn held(_confine: Confine) -> Result<(Confine, Option<Arc<Network>>), String> {
    Err(
        "The user's allowed hosts are on, and the agent's commands can't be held to them on this \
         system."
            .into(),
    )
}

/// Where a harness's program (Claude Code, Codex) starts, and what it's
/// given ([`hold_program`]).
#[derive(Default)]
pub struct Held {
    /// The network it starts in: the agent's ([`network::agents`]).
    pub network: Option<Arc<Network>>,
    /// Its proxy variables, while the list is on: the filter.
    pub env: Env,
    /// While the list is on, on macOS, the Seatbelt profile it runs under:
    /// connections out to the loopback alone.
    pub seatbelt: Option<String>,
}

/// Where a harness's program starts for `chat` (J10): in the agent's
/// network, and while the list is on, held to it as a command is, the
/// filter its proxy. It gets no sandbox of a command's, which would keep it
/// from its own files. Why not, when the list is on but it can't be held
/// here.
pub fn hold_program(chat: &str) -> Result<Held, String> {
    if current().is_none() {
        return Ok(Held {
            network: network::agents(),
            ..Held::default()
        });
    }
    program_held(chat)
}

/// In the agent's sealed network (J04).
#[cfg(target_os = "linux")]
fn program_held(chat: &str) -> Result<Held, String> {
    let network = sealed()?;
    let port = filter_port(Client::Commands, Some(&network))?;
    Ok(Held {
        network: Some(network),
        env: proxy_env(chat, port),
        seatbelt: None,
    })
}

/// Under Seatbelt's rules for the loopback alone (J07).
#[cfg(target_os = "macos")]
fn program_held(chat: &str) -> Result<Held, String> {
    let port = filter_port(Client::Commands, None)?;
    let closed = crate::net::proxies_here();
    Ok(Held {
        network: None,
        env: proxy_env(chat, port),
        seatbelt: Some(crate::overlay::confine::loopback_profile(&closed)),
    })
}

/// On Windows itself nothing holds a program to the list (J08).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn program_held(_chat: &str) -> Result<Held, String> {
    Err(
        "The user's allowed hosts are on, and a harness's program on Windows itself can't be \
         held to them: open the project in WSL, where it is, or ask the user to let the agent \
         reach any host in Settings → Permissions & approval."
            .into(),
    )
}

/// The variables that send a command's connections through the filter at
/// `port` of its loopback, as `chat`'s (J03): the chat goes as the proxy's
/// user name, which most tools send.
fn proxy_env(chat: &str, port: u16) -> Env {
    let user: String = chat
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    let proxy = match user.is_empty() {
        true => format!("http://127.0.0.1:{port}"),
        false => format!("http://{user}@127.0.0.1:{port}"),
    };
    let mut env = Vec::new();
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        env.push((name.to_string(), proxy.clone()));
        env.push((name.to_ascii_lowercase(), proxy.clone()));
    }
    env.push(("NO_PROXY".into(), NO_PROXY.into()));
    env.push(("no_proxy".into(), NO_PROXY.into()));
    // Node's own fetch and http follow the variables only when asked.
    env.push(("NODE_USE_ENV_PROXY".into(), "1".into()));
    env
}

/// The proxy the agent's browser starts with while the list is on: its
/// filter, where Chrome reaches it (in `network`, the agent's network it
/// joins, else on 127.0.0.1). None while any host may be reached: Chrome
/// goes the way Settings → Network says (J09).
pub fn browser_proxy(network: Option<&Arc<Network>>) -> Result<Option<BrowserProxy>, String> {
    if current().is_none() {
        return Ok(None);
    }
    let port = filter_port(Client::Browser, network)?;
    Ok(Some(BrowserProxy {
        server: format!("http://127.0.0.1:{port}"),
        username: None,
        password: None,
    }))
}

/// The list `Permissions::allowed_hosts` names, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedHosts {
    entries: Vec<Entry>,
    /// The entries as the settings show them, for messages.
    shown: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Name(String),
    /// A name's subdomains, at any depth.
    Subdomains(String),
    Address(IpAddr),
}

impl AllowedHosts {
    /// The list `permissions` sets: None when any host may be reached. An
    /// entry that can't be read is left out (the settings page saves none).
    pub fn of(permissions: &Permissions) -> Option<AllowedHosts> {
        if permissions.allow_any_host {
            return None;
        }
        let shown: Vec<String> = permissions
            .allowed_hosts
            .iter()
            .filter_map(|entry| normalize(entry))
            .collect();
        let entries = shown.iter().filter_map(|entry| read_entry(entry)).collect();
        Some(AllowedHosts { entries, shown })
    }

    /// Whether `host` may be reached: a URL's host or a CONNECT's (a name,
    /// an IPv4 address, an IPv6 address with or without its brackets).
    /// This computer's own aren't on any list ([`is_loopback`]).
    pub fn allows(&self, host: &str) -> bool {
        let Some(host) = url_host(host) else {
            return false;
        };
        let address = address_of(&host);
        self.entries.iter().any(|entry| match (entry, address) {
            (Entry::Address(allowed), Some(ip)) => *allowed == ip,
            (Entry::Name(name), None) => *name == host,
            (Entry::Subdomains(name), None) => host
                .strip_suffix(name.as_str())
                .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
            _ => false,
        })
    }

    /// The entries, for a message: "docs.rs, *.github.com", or "none".
    pub fn shown(&self) -> String {
        match self.shown.is_empty() {
            true => "none".to_string(),
            false => self.shown.join(", "),
        }
    }
}

/// `raw` as the settings keep an entry, or None when it can't be one: a
/// URL's scheme, sign-in, port and path are dropped (`https://docs.rs/x`
/// is `docs.rs`); `*.` may lead a name, nothing else holds a `*`.
pub fn normalize(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    if let Some((_, rest)) = text.split_once("://") {
        text = rest;
    }
    let text = text.split(['/', '?', '#']).next().unwrap_or_default();
    let text = text.rsplit_once('@').map_or(text, |(_, host)| host);
    let (subdomains, text) = match text.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let host = url_host(without_port(text))?;
    match (subdomains, address_of(&host)) {
        (false, _) => Some(host),
        (true, None) => Some(format!("*.{host}")),
        // `*.10.0.0.5` names nothing.
        (true, Some(_)) => None,
    }
}

/// Whether `host` (a URL's or a CONNECT's) is this computer's own:
/// `localhost` and its subdomains, the loopback addresses.
pub fn is_loopback(host: &str) -> bool {
    let Some(host) = url_host(host) else {
        return false;
    };
    match address_of(&host) {
        Some(ip) => ip.is_loopback(),
        None => host == "localhost" || host.ends_with(".localhost"),
    }
}

/// `text` without a port after it: `example.com:8443`, `[::1]:443`. A bare
/// IPv6 address (`fd00::1`) has colons of its own and keeps them.
fn without_port(text: &str) -> &str {
    if text.starts_with('[') {
        return match text.find(']') {
            Some(end) => &text[..=end],
            None => text,
        };
    }
    match text.split_once(':') {
        Some((host, port)) if !port.contains(':') => host,
        _ => text,
    }
}

/// `text`, a host, as a URL has it (lower case, punycode, IPv4 in dotted
/// form, IPv6 in brackets), without a trailing dot: None when it isn't
/// one, or holds what no host's name does.
fn url_host(text: &str) -> Option<String> {
    let text = text.trim();
    // An IPv6 address without its brackets.
    let bracketed = match text.contains(':') && !text.starts_with('[') {
        true => format!("[{text}]"),
        false => text.to_string(),
    };
    let url = reqwest::Url::parse(&format!("http://{bracketed}/")).ok()?;
    let host = url.host_str()?.trim_end_matches('.').to_string();
    let valid = host.starts_with('[')
        || (host.len() <= MAX_NAME
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            }));
    valid.then_some(host)
}

/// The address `host` ([`url_host`]'s) is, if it's one; an IPv4-mapped
/// IPv6 address is its IPv4 address.
fn address_of(host: &str) -> Option<IpAddr> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>().ok()? {
        IpAddr::V6(v6) => Some(v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)),
        v4 => Some(v4),
    }
}

/// An entry [`normalize`] made, read.
fn read_entry(entry: &str) -> Option<Entry> {
    if let Some(name) = entry.strip_prefix("*.") {
        return Some(Entry::Subdomains(name.to_string()));
    }
    Some(match address_of(entry) {
        Some(ip) => Entry::Address(ip),
        None => Entry::Name(entry.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(entries: &[&str]) -> AllowedHosts {
        AllowedHosts::of(&Permissions {
            allow_any_host: false,
            allowed_hosts: entries.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        })
        .expect("a list")
    }

    /// The cases the settings page normalises entries with too
    /// (src/lib/permissions.test.ts): both must agree on every one.
    #[test]
    fn entries_are_normalised_as_the_settings_page_does() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../../test-fixtures/allowed-hosts.json"))
                .unwrap();
        for case in fixture["normalize"].as_array().unwrap() {
            let raw = case[0].as_str().unwrap();
            let want = case[1].as_str();
            assert_eq!(normalize(raw).as_deref(), want, "{raw:?}");
        }
    }

    #[test]
    fn a_name_matches_itself_alone() {
        let list = listing(&["docs.rs"]);
        assert!(list.allows("docs.rs"));
        assert!(list.allows("DOCS.RS."));
        assert!(!list.allows("www.docs.rs"));
        assert!(!list.allows("docs.rs.evil.com"));
        assert!(!list.allows("notdocs.rs"));
    }

    #[test]
    fn a_wildcard_matches_the_subdomains_alone() {
        let list = listing(&["*.github.com"]);
        assert!(list.allows("api.github.com"));
        assert!(list.allows("a.b.github.com"));
        assert!(!list.allows("github.com"));
        assert!(!list.allows("evilgithub.com"));
        assert!(!list.allows(".github.com"));
    }

    #[test]
    fn an_address_matches_itself_however_it_is_written() {
        let list = listing(&["10.0.0.5", "[FD00:0::1]"]);
        assert!(list.allows("10.0.0.5"));
        assert!(list.allows("::ffff:10.0.0.5"));
        assert!(list.allows("[::ffff:10.0.0.5]"));
        assert!(list.allows("fd00::1"));
        assert!(list.allows("[fd00:0:0::1]"));
        assert!(!list.allows("10.0.0.6"));
        // A name that happens to read as one is no address.
        assert!(!listing(&["docs.rs"]).allows("10.0.0.5"));
    }

    #[test]
    fn an_international_name_matches_its_punycode() {
        let list = listing(&["bücher.de"]);
        assert!(list.allows("xn--bcher-kva.de"));
        assert!(list.allows("BÜCHER.de"));
    }

    #[test]
    fn any_host_is_no_list_and_an_empty_list_allows_none() {
        assert_eq!(AllowedHosts::of(&Permissions::default()), None);
        let empty = listing(&[]);
        assert!(!empty.allows("docs.rs"));
        assert_eq!(empty.shown(), "none");
        // An entry the settings couldn't have saved is left out.
        let odd = listing(&["*", "docs.rs", "exa mple.com"]);
        assert_eq!(odd.shown(), "docs.rs");
        assert!(!odd.allows("example.com"));
    }

    /// The list the process follows, and the browser's filter: in a process
    /// of their own, as the list is the process's.
    #[test]
    fn the_browsers_filter_follows_the_processs_list() {
        crate::testing::in_a_process_of_its_own(
            "allowed_hosts::tests::the_browsers_filter_in_a_process_of_its_own",
        );
    }

    #[test]
    #[ignore = "run in a process of its own by the_browsers_filter_follows_the_processs_list"]
    fn the_browsers_filter_in_a_process_of_its_own() {
        use crate::agent_run::{agent_configure, AgentConfig};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        assert_eq!(browser_proxy(None), Ok(None), "any host: Chrome's own way");
        // As the app's settings come.
        let test = crate::testing::TestCore::new();
        let mut config = AgentConfig::default();
        config.permissions.allow_any_host = false;
        config.permissions.allowed_hosts = vec!["docs.rs".into()];
        agent_configure(&test.core, config);
        assert_eq!(
            current().map(|list| list.shown()).as_deref(),
            Some("docs.rs")
        );
        let proxy = browser_proxy(None).unwrap().expect("the filter");
        let port: u16 = proxy
            .server
            .strip_prefix("http://127.0.0.1:")
            .and_then(|port| port.parse().ok())
            .unwrap_or_else(|| panic!("{}", proxy.server));
        assert_eq!(browser_proxy(None).unwrap(), Some(proxy), "one, kept");
        let started = Instant::now();
        let answer = crate::rt::block_on(async move {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(b"CONNECT evil.example:443 HTTP/1.1\r\n\r\n")
                .await
                .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).await.unwrap();
            answer
        });
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert_eq!(
            refused_since(started, Client::Browser, None),
            ["evil.example"]
        );
        assert!(refused_since(started, Client::Commands, None).is_empty());
        // Any host again: Chrome goes its own way.
        agent_configure(&test.core, AgentConfig::default());
        assert_eq!(current(), None);
        assert_eq!(browser_proxy(None), Ok(None));
    }

    #[test]
    fn a_held_commands_proxy_names_its_chat() {
        let env = proxy_env("chat 7/é", 4242);
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            assert_eq!(
                get(name),
                Some("http://chat%207%2F%C3%A9@127.0.0.1:4242"),
                "{name}"
            );
        }
        for name in ["NO_PROXY", "no_proxy"] {
            assert_eq!(get(name), Some("localhost,127.0.0.1,::1"), "{name}");
        }
        assert_eq!(get("NODE_USE_ENV_PROXY"), Some("1"));
        assert_eq!(proxy_env("", 4242)[0].1, "http://127.0.0.1:4242");
    }

    #[test]
    fn while_any_host_may_be_reached_a_command_is_as_it_was() {
        assert_eq!(current(), None);
        let root = crate::testing::temp_dir();
        let confine = crate::overlay::confine::hiding_if_available(&root.0);
        let had = confine.as_ref().map(|c| c.network.is_some());
        let (confine, env) = hold(confine, "chat").unwrap();
        assert_eq!(confine.map(|c| c.network.is_some()), had);
        assert!(env.is_empty(), "{env:?}");
        // A harness's program, as before: no proxy, no Seatbelt.
        let program = hold_program("chat").unwrap();
        assert!(program.env.is_empty() && program.seatbelt.is_none());
        assert_eq!(program.network.is_some(), network::agents().is_some());
    }

    /// While the list is on, a command's only way out is the filter: a host
    /// off the list is refused and named for the model, an address isn't
    /// reached directly, and a server left running from before is stopped
    /// (J03-J05, J07). In a process of its own, as the list is the
    /// process's.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_command_reaches_the_network_only_through_the_filter() {
        crate::testing::in_a_process_of_its_own(
            "allowed_hosts::tests::a_held_command_in_a_process_of_its_own",
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "run in a process of its own by a_command_reaches_the_network_only_through_the_filter"]
    fn a_held_command_in_a_process_of_its_own() {
        use crate::agent_run::tools::{execute, CallContext};
        use crate::agent_run::StopSignal;
        let dir = crate::testing::temp_dir();
        let sandboxed = crate::overlay::confine::hiding_if_available(&dir.0).is_some();
        // Linux seals the network with pasta.
        #[cfg(target_os = "linux")]
        let sealable = crate::distro::which("pasta").is_some();
        #[cfg(not(target_os = "linux"))]
        let sealable = true;
        if !sealable || !crate::process::on_path("curl") || !sandboxed {
            eprintln!("pasta, curl or the sandbox is missing here: skipped");
            return;
        }
        let root = dir.0.to_string_lossy().into_owned();
        let test = crate::testing::TestCore::new();
        let ctx = CallContext {
            seen: crate::file_guard::Seen::new("held"),
            restricted: Arc::default(),
            owner: "held".into(),
            checks_owner: "held".into(),
            hidden: Vec::new(),
            note: Arc::new(|_| {}),
            background_chat: Some("held".into()),
            folders: None,
            reply_cut_off: false,
        };
        let stop = Arc::new(StopSignal::new());
        let bash = |args: serde_json::Value| {
            let args = args.to_string();
            crate::rt::block_on(execute(&test.core, Some(&root), "bash", &args, &stop, &ctx))
                .content
        };
        // A server left running from before the list.
        let started = bash(serde_json::json!({
            "command": crate::testing::beating("up"),
            "background": true,
        }));
        assert!(crate::testing::beats(&dir.0), "{started}");

        configure(&Permissions {
            allow_any_host: false,
            allowed_hosts: vec!["docs.rs".into()],
            ..Permissions::default()
        });
        assert!(
            !crate::testing::beats(&dir.0),
            "stopped as the list was turned on"
        );
        let result = bash(serde_json::json!({
            "command": "curl -sS -o /dev/null -w '%{http_code}' http://unlisted.example/; \
                        echo \" [$?]\"; \
                        curl -sS --noproxy '*' --max-time 5 http://192.0.2.1/; \
                        echo \"direct [$?]\"",
        }));
        assert!(result.contains("403 [0]"), "{result}");
        // 7: it couldn't connect.
        assert!(result.contains("direct [7]"), "{result}");
        assert!(
            result.contains("(The user's allowed hosts kept these out: unlisted.example."),
            "{result}"
        );
        // A harness's program is held as a command is (J10).
        let program = hold_program("held").unwrap();
        let proxy = program
            .env
            .iter()
            .find(|(name, _)| name == "HTTPS_PROXY")
            .map(|(_, value)| value.as_str());
        assert!(
            proxy.is_some_and(|proxy| proxy.starts_with("http://held@127.0.0.1:")),
            "{proxy:?}"
        );
        #[cfg(target_os = "linux")]
        assert!(program.network.is_some() && program.seatbelt.is_none());
        #[cfg(target_os = "macos")]
        assert!(program
            .seatbelt
            .is_some_and(|profile| profile.contains("(remote ip \"localhost:*\")")));

        // Any host again: the network is let go.
        configure(&Permissions::default());
        assert!(crate::overlay::confine::network::agents().is_none());
        assert!(hold_program("held").unwrap().env.is_empty());
    }

    #[test]
    fn this_computers_own_hosts_are_loopback() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "app.localhost",
            "127.0.0.1",
            "127.1.2.3",
        ] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["::1", "[::1]", "::ffff:127.0.0.1"] {
            assert!(is_loopback(host), "{host}");
        }
        for host in [
            "localhost.com",
            "10.0.0.1",
            "example.com",
            "0.0.0.0",
            "[fd00::1]",
        ] {
            assert!(!is_loopback(host), "{host}");
        }
    }
}
