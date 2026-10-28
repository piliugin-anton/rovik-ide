//! The app's connections out, as Settings → Network sets them.
//!
//! - One HTTP client is shared by the requests to model providers
//!   (`client`), so their connections are kept open and reused, and on
//!   HTTP/2 requests at once share one. Downloads build their own clients
//!   from `builder`, or have a builder of their own set the same way by
//!   `configure` (the app's updater); cloud hosts' SSH connections start at
//!   `open_cloud_host`; the agent's browser starts with `browser_proxy`.
//! - Each kind of connection goes its own way (`Routes`): directly, through
//!   the system's proxy, as reqwest finds it, or through one of the saved
//!   proxies, HTTP or SOCKS5, which nothing on this computer (localhost)
//!   goes through. This computer's requests start at "Automatic": the
//!   system's proxy, which is none while the system has none set.
//! - A cloud host connects as the app tells it (`host_network`, sent with
//!   the settings runs follow): there, `set_host_network` takes it, and the
//!   shared client, the downloads and the browser follow it.
//! - The shared client, and its connections with it, is replaced when the
//!   settings change; after an attempt that may have failed for its
//!   connection (`refresh`); after the computer slept, as a connection kept
//!   from before is likely dead and a request on it would wait out its
//!   first-byte limit; and every ten minutes, which follows a change of the
//!   system's proxy. A request keeps the client it started with. While one
//!   is under way on HTTP/2, pings find a dead connection.
//!
//! The settings are network-settings.json, named by `init`. Another of
//! Rovik's windows may save it: it's read again when it changed. The
//! proxies' passwords are in the app's keychain: the file says which have
//! one and when one last changed, and `init`'s `password` reads them, off
//! the async threads, as a keychain may ask the user first, once a route
//! needs one. A process that never calls `init` (a cloud host's daemon, the
//! tests) has the settings' defaults until a computer tells it more.

mod settings;
#[cfg(test)]
mod tests;
mod tunnel;

pub use rovik_protocol::network::{HostNetwork, ProxyEndpoint, ProxyKind, ProxyRoute};
pub use settings::{
    authority, described, on_this_computer, NetworkSettings, Route, Routes, Row, SavedProxy,
    DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS,
};
pub use tunnel::Link;

use base64::Engine;
use hyper_util::client::proxy::matcher::Matcher;
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub(crate) use tunnel::{open as open_tunnel, Hop};

/// How long a connection the shared client keeps may wait for its next
/// request: under the minute load balancers commonly give one.
const POOL_IDLE: Duration = Duration::from_secs(55);
/// While a request is under way on an HTTP/2 connection, it's pinged this
/// often, and dropped when a ping isn't answered within `PING_WAIT`.
const PING_EVERY: Duration = Duration::from_secs(30);
const PING_WAIT: Duration = Duration::from_secs(20);
/// The shared client is replaced once it's this old.
const MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// The wall clock moved this much further than the process's own between
/// two uses: the computer slept.
const SLEPT_BY: Duration = Duration::from_secs(30);
/// How often the settings file is looked at for another window's change.
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// After a password couldn't be read, how long before it's tried again.
const PASSWORD_RETRY: Duration = Duration::from_secs(30);
/// Where what has to go through a proxy that can't be used goes: nowhere,
/// so nothing goes around the proxy chosen.
const NOWHERE: &str = "http://proxy.invalid:9";

/// Reads a saved proxy's password, by the proxy's id: none when none is
/// saved.
pub type PasswordSource = Arc<dyn Fn(&str) -> Result<Option<Secret>, String> + Send + Sync>;

/// The agent's browser's proxy (eoka): Chrome's `--proxy-server`, where
/// `direct://` goes directly, and what it answers the proxy's sign-in
/// with. Its `Debug` leaves the password out (`Secret`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserProxy {
    pub server: String,
    pub username: Option<String>,
    pub password: Option<Secret>,
}

impl BrowserProxy {
    /// Directly, whatever the browser's own settings say.
    pub fn direct() -> BrowserProxy {
        BrowserProxy {
            server: "direct://".into(),
            username: None,
            password: None,
        }
    }

    /// Through `proxy`, signed in to as it says.
    fn of(proxy: &ProxyEndpoint) -> BrowserProxy {
        let scheme = match proxy.kind {
            ProxyKind::Http => "http",
            // Chrome's SOCKS5 looks the names up through the proxy anyway.
            ProxyKind::Socks5 => "socks5",
        };
        BrowserProxy {
            server: format!("{scheme}://{}", authority(&proxy.host, proxy.port)),
            username: proxy.username.clone(),
            password: proxy.username.as_ref().and(proxy.password.clone()),
        }
    }

    fn of_route(route: &ProxyRoute) -> BrowserProxy {
        match route {
            ProxyRoute::Direct => BrowserProxy::direct(),
            ProxyRoute::Proxy(proxy) => BrowserProxy::of(proxy),
        }
    }

    /// Through a proxy that can't be used: nowhere.
    fn nowhere() -> BrowserProxy {
        BrowserProxy {
            server: NOWHERE.into(),
            username: None,
            password: None,
        }
    }
}

/// network-settings.json.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Saved {
    #[serde(flatten)]
    settings: NetworkSettings,
    /// When a proxy's password last changed, in ms since 1970: another
    /// window reads them again then.
    #[serde(skip_serializing_if = "Option::is_none")]
    passwords_changed: Option<u64>,
}

/// Settings → Network's page.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetworkView {
    pub settings: NetworkSettings,
    /// This computer's proxy now, which "System proxy" and "Automatic"
    /// use.
    pub system: SystemProxy,
    pub default_timeout_secs: u32,
    pub min_timeout_secs: u32,
    pub max_timeout_secs: u32,
}

/// The system's proxy for HTTPS and for HTTP, as `scheme://host:port`
/// without credentials; none: connections go directly.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemProxy {
    pub https: Option<String>,
    pub http: Option<String>,
}

#[derive(Default)]
struct State {
    file: Option<PathBuf>,
    source: Option<PasswordSource>,
    settings: NetworkSettings,
    /// The saved proxies' passwords as read, by id: none when none is
    /// saved. One that isn't here is read once a route needs it.
    passwords: HashMap<String, Option<Secret>>,
    /// When a password last changed, as the file says.
    passwords_changed: Option<u64>,
    /// No read of the passwords before then: one is under way, or the last
    /// failed.
    password_not_before: Option<Instant>,
    /// The file as last read, and when it was last looked at.
    seen: Option<Vec<u8>>,
    looked: Option<Instant>,
    /// Bumped by every change of what the clients are built from.
    generation: u64,
    shared: Option<Shared>,
    /// The shared client is replaced at its next use.
    stale: bool,
    /// The clocks at the shared client's last use, to tell a sleep.
    clocks: Option<(SystemTime, Instant)>,
    /// On a cloud host: how the app said to connect (`AgentConfig`), the
    /// last computer to say having it. Not set in the app, whose settings
    /// say.
    hosted: Option<HostNetwork>,
}

struct Shared {
    client: reqwest::Client,
    generation: u64,
    built: Instant,
}

/// What the clients are built from now.
struct Effective {
    timeout: Duration,
    way: Way,
}

/// How a kind of connection gets to its server.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Way {
    /// Directly, whatever proxy the system has.
    Direct,
    /// Through the system's proxy, as reqwest finds it.
    System,
    /// Through this proxy, signed in to.
    Proxy(ProxyEndpoint),
    /// Through a proxy that isn't there: nothing goes around it, it fails
    /// to connect. Not reached, as the settings are checked.
    Nowhere,
}

#[derive(Default)]
struct Net {
    state: Mutex<State>,
}

static NET: LazyLock<Net> = LazyLock::new(Net::default);

/// Reads the settings from `file`, and the proxies' passwords, when
/// they're needed, with `password`.
pub fn init(file: PathBuf, password: PasswordSource) {
    NET.init(file, password);
}

/// The settings, for their page.
pub fn view() -> NetworkView {
    NET.view()
}

/// Why `settings` can't be kept with these passwords, the new ones (some)
/// and those to forget (none), by proxy, when they can't.
pub fn check(
    settings: &NetworkSettings,
    passwords: &HashMap<String, Option<Secret>>,
) -> Result<(), String> {
    let settings = settings.clone().normalized();
    settings.check()?;
    for (id, password) in passwords {
        let proxy = settings
            .proxy(id)
            .ok_or_else(|| "A password came for a proxy that isn't saved.".to_string())?;
        if let Some(password) = password {
            proxy.check_password(password)?;
        }
    }
    Ok(())
}

/// Keeps `settings` and applies them, with the passwords the app saved
/// (some) or forgot (none) for these proxies, noted for the other windows.
/// Refused when they can't be used (`check`).
pub fn save(
    settings: NetworkSettings,
    passwords: &HashMap<String, Option<Secret>>,
) -> Result<NetworkView, String> {
    NET.save(settings, passwords)?;
    Ok(NET.view())
}

/// The client the requests to model providers share.
pub async fn client() -> reqwest::Client {
    NET.client().await
}

/// An attempt failed in a way its connection may be to blame for: the
/// next request connects afresh.
pub fn refresh() {
    NET.refresh();
}

/// A builder with the settings' timeout and the way the requests to model
/// providers go, for a client of its own (a download).
pub async fn builder() -> reqwest::ClientBuilder {
    NET.builder().await
}

/// The settings as another window may have saved them, and the proxies'
/// passwords their route needs, read for `configure`, which can't wait.
pub async fn ready() {
    NET.current().await;
}

/// A builder another crate made (the app's updater makes its own) given
/// what `builder` gives one: the settings' timeout and the way the
/// downloads go. For a closure that can't wait: `ready` first.
pub fn configure(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    NET.configure(builder)
}

/// A TCP connection to a cloud host's SSH server, the way the settings say
/// SSH to cloud hosts goes.
pub async fn open_cloud_host(host: &str, port: u16) -> Result<Link, String> {
    NET.open_cloud_host(host, port).await
}

/// The proxy a cloud host's connection goes through, as a page says it
/// ("the HTTP proxy at proxy.example.com:3128"); none: directly.
pub async fn cloud_host_proxy(host: &str, port: u16) -> Result<Option<String>, String> {
    NET.current().await;
    let hop = cloud_hop(&NET.lock(), host, port)?;
    Ok(match hop {
        Hop::Direct => None,
        Hop::Http { host, port, .. } => {
            Some(format!("the HTTP proxy at {}", authority(&host, port)))
        }
        Hop::Socks5 { host, port, .. } => {
            Some(format!("the SOCKS5 proxy at {}", authority(&host, port)))
        }
    })
}

/// The proxy the agent's browser in this process starts with: on a cloud
/// host the way the app said (`set_host_network`), in the app the way its
/// settings say. None: Chrome's own settings.
pub async fn browser_proxy() -> Option<BrowserProxy> {
    NET.browser_proxy().await
}

/// How the agent's connection to `host:port` goes on from the allowed
/// hosts' filter (docs/design/allowed-hosts-plan.md, J03): the browser's
/// (`browser`) as Settings → Network's browser row says, on a cloud host as
/// the app said, its own settings being the system's proxy; the commands'
/// through the system's proxy, as their environment would have them go.
pub(crate) async fn agent_hop(browser: bool, host: &str, port: u16) -> Result<Hop, String> {
    NET.agent_hop(browser, host, port).await
}

/// How long connecting may take, as the settings say (on a cloud host, as
/// the app said).
pub(crate) async fn connect_timeout() -> Duration {
    NET.current().await;
    effective(&NET.lock()).timeout
}

/// How cloud hosts are to connect, as the settings say, the proxies'
/// passwords with them: sent with the settings runs follow
/// (`AgentConfig::network`).
pub async fn host_network() -> HostNetwork {
    NET.host_network().await
}

/// On a cloud host: how the app said to connect, for what this process
/// connects to from now on. None: as the host has it.
pub fn set_host_network(network: Option<HostNetwork>) {
    NET.set_host_network(network);
}

/// The ports of the proxies on this computer's loopback that its
/// connections may go through: those the environment's variables and the
/// system's settings name, and Settings → Network's (on a cloud host, those
/// the app sent). While the user's allowed hosts are on, the agent's
/// commands are kept from them: each would be a way around the list
/// (docs/design/allowed-hosts-plan.md, J06, J07).
pub fn proxies_here() -> Vec<u16> {
    let mut ports = loopback_proxy_ports(std::env::vars_os());
    let system = system_proxy();
    ports.extend(
        [system.https, system.http]
            .iter()
            .flatten()
            .filter_map(|proxy| loopback_port(proxy)),
    );
    ports.extend(saved_here(&NET.lock()));
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The ports of the proxies `vars` (an environment) name on this
/// computer's loopback (`HTTPS_PROXY=http://127.0.0.1:3128`), each once.
pub(crate) fn loopback_proxy_ports(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<u16> {
    const PROXIES: &[&str] = &["http_proxy", "https_proxy", "all_proxy", "ftp_proxy"];
    let mut ports: Vec<u16> = vars
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.to_str()?.to_ascii_lowercase();
            PROXIES
                .contains(&name.as_str())
                .then(|| loopback_port(value.to_str()?))?
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The port of the proxy at `address` (`http://127.0.0.1:3128`,
/// `localhost:8080`, `socks5h://[::1]:1080`) when it's on the loopback.
fn loopback_port(address: &str) -> Option<u16> {
    let address = address.trim();
    let url = match address.contains("://") {
        true => reqwest::Url::parse(address),
        false => reqwest::Url::parse(&format!("http://{address}")),
    }
    .ok()?;
    let host = url.host_str()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let local = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !local {
        return None;
    }
    url.port_or_known_default()
        .or_else(|| url.scheme().starts_with("socks").then_some(1080))
}

/// The ports of the proxies `state`'s settings save on this computer's
/// loopback, and on a cloud host those the app sent.
fn saved_here(state: &State) -> Vec<u16> {
    let saved = state
        .settings
        .proxies
        .iter()
        .map(|p| (p.host.as_str(), p.port));
    let sent = state
        .hosted
        .iter()
        .flat_map(|hosted| [&hosted.models, &hosted.browser])
        .filter_map(|route| match route {
            Some(ProxyRoute::Proxy(proxy)) => Some((proxy.host.as_str(), proxy.port)),
            _ => None,
        });
    saved
        .chain(sent)
        .filter(|(host, _)| on_this_computer(host))
        .map(|(_, port)| port)
        .collect()
}

/// The proxy a request to `url` goes through, as a message says it ("the
/// HTTP proxy at proxy.example.com:3128"); none when it goes directly or
/// through the system's.
pub fn proxy_for_requests(url: &str) -> Option<String> {
    let host = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
    if on_this_computer(&host) {
        return None;
    }
    match effective(&NET.lock()).way {
        Way::Proxy(proxy) => Some(described(&proxy)),
        _ => None,
    }
}

/// An error and what caused it: a request's says only "error sending
/// request", the proxy's or the connection's answer is further down.
pub fn error_text(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        let said = source.to_string();
        if !text.contains(&said) {
            text.push_str(": ");
            text.push_str(&said);
        }
        cause = source.source();
    }
    text
}

impl Net {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn init(&self, file: PathBuf, source: PasswordSource) {
        {
            let mut state = self.lock();
            state.file = Some(file);
            state.source = Some(source);
            state.passwords.clear();
            state.password_not_before = None;
        }
        self.follow(true);
    }

    fn view(&self) -> NetworkView {
        self.follow(true);
        let settings = self.lock().settings.clone();
        NetworkView {
            settings,
            system: system_proxy(),
            default_timeout_secs: DEFAULT_TIMEOUT_SECS,
            min_timeout_secs: MIN_TIMEOUT_SECS,
            max_timeout_secs: MAX_TIMEOUT_SECS,
        }
    }

    fn save(
        &self,
        settings: NetworkSettings,
        passwords: &HashMap<String, Option<Secret>>,
    ) -> Result<(), String> {
        check(&settings, passwords)?;
        let mut settings = settings.normalized();
        let mut state = self.lock();
        // Whether a password is saved is what the app did, not what the
        // page says.
        for proxy in &mut settings.proxies {
            proxy.password_saved = match passwords.get(&proxy.id) {
                Some(password) => password.is_some(),
                None => state
                    .settings
                    .proxy(&proxy.id)
                    .is_some_and(|saved| saved.password_saved),
            };
        }
        let passwords_changed = if passwords.is_empty() {
            state.passwords_changed
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            Some(now.max(state.passwords_changed.map_or(0, |c| c + 1)))
        };
        let saved = Saved {
            settings: settings.clone(),
            passwords_changed,
        };
        write(&mut state, &saved)?;
        state.passwords_changed = passwords_changed;
        let mut changed = state.settings != settings;
        for (id, password) in passwords {
            if state
                .passwords
                .insert(id.clone(), password.clone())
                .as_ref()
                != Some(password)
            {
                changed = true;
            }
        }
        let kept: HashSet<String> = settings.proxies.iter().map(|p| p.id.clone()).collect();
        state.passwords.retain(|id, _| kept.contains(id));
        state.settings = settings;
        if changed {
            state.generation += 1;
        }
        Ok(())
    }

    /// Reads the file again when it's due (or `now`) and changed: another
    /// window saved it.
    fn follow(&self, now: bool) {
        let file = {
            let mut state = self.lock();
            let due = now || state.looked.is_none_or(|at| at.elapsed() >= LOOK_EVERY);
            if !due {
                return;
            }
            state.looked = Some(Instant::now());
            match &state.file {
                Some(file) => file.clone(),
                None => return,
            }
        };
        let bytes = std::fs::read(&file).ok();
        let mut state = self.lock();
        if bytes == state.seen {
            return;
        }
        state.seen = bytes.clone();
        let saved = match bytes.as_deref().map(serde_json::from_slice::<Saved>) {
            None => Saved::default(),
            Some(Ok(saved)) => saved,
            Some(Err(e)) => {
                log::warn!(
                    "network: {} isn't settings, kept as they were: {e}",
                    file.display()
                );
                return;
            }
        };
        let settings = saved.settings.normalized();
        if let Err(e) = settings.check() {
            log::warn!(
                "network: {} can't be used, kept as they were: {e}",
                file.display()
            );
            return;
        }
        if state.settings != settings {
            state.settings = settings;
            state.generation += 1;
        }
        if saved.passwords_changed != state.passwords_changed {
            state.passwords_changed = saved.passwords_changed;
            state.passwords.clear();
            state.password_not_before = None;
        }
    }

    /// Reads the passwords the routes need and aren't read yet: off the
    /// async threads.
    async fn read_passwords(&self) {
        let (source, wanted) = {
            let mut state = self.lock();
            if state
                .password_not_before
                .is_some_and(|t| Instant::now() < t)
            {
                return;
            }
            let wanted = needed(&state);
            if wanted.is_empty() {
                return;
            }
            let Some(source) = state.source.clone() else {
                // Nothing to read them with: none saved.
                for id in wanted {
                    state.passwords.insert(id, None);
                }
                return;
            };
            // The others go on with what there is meanwhile.
            state.password_not_before = Some(Instant::now() + PASSWORD_RETRY);
            (source, wanted)
        };
        let read = crate::rt::spawn_blocking(move || {
            wanted
                .into_iter()
                .map(|id| {
                    let password = source(&id);
                    (id, password)
                })
                .collect::<Vec<_>>()
        })
        .await;
        let mut state = self.lock();
        let read = match read {
            Ok(read) => read,
            Err(e) => {
                log::warn!("network: the proxies' passwords couldn't be read: {e}");
                return;
            }
        };
        let mut failed = false;
        for (id, password) in read {
            match password {
                Ok(password) => {
                    if state.passwords.insert(id, password.clone()) != Some(password) {
                        state.generation += 1;
                    }
                }
                Err(e) => {
                    failed = true;
                    log::warn!("network: the proxy {id}'s password couldn't be read: {e}");
                }
            }
        }
        if !failed {
            state.password_not_before = None;
        }
    }

    async fn current(&self) {
        self.follow(false);
        self.read_passwords().await;
    }

    async fn client(&self) -> reqwest::Client {
        self.current().await;
        self.shared(SystemTime::now(), Instant::now())
    }

    /// The shared client, a new one when it's due one, at these clocks.
    fn shared(&self, wall: SystemTime, monotonic: Instant) -> reqwest::Client {
        let mut state = self.lock();
        let slept = state
            .clocks
            .is_some_and(|before| slept(before, (wall, monotonic)));
        state.clocks = Some((wall, monotonic));
        let keep = state.shared.as_ref().is_some_and(|shared| {
            !state.stale
                && !slept
                && shared.generation == state.generation
                && monotonic.saturating_duration_since(shared.built) < MAX_AGE
        });
        if !keep {
            let client = build_shared(&effective(&state));
            state.shared = Some(Shared {
                client,
                generation: state.generation,
                built: monotonic,
            });
            state.stale = false;
        }
        state
            .shared
            .as_ref()
            .map(|shared| shared.client.clone())
            .expect("built above")
    }

    fn refresh(&self) {
        self.lock().stale = true;
    }

    async fn builder(&self) -> reqwest::ClientBuilder {
        self.current().await;
        let state = self.lock();
        base_builder(&effective(&state))
    }

    fn configure(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        configured(builder, &effective(&self.lock()))
    }

    async fn open_cloud_host(&self, host: &str, port: u16) -> Result<Link, String> {
        self.current().await;
        let (hop, timeout) = {
            let state = self.lock();
            (cloud_hop(&state, host, port)?, state.settings.timeout())
        };
        tunnel::open(host, port, &hop, timeout).await
    }

    async fn browser_proxy(&self) -> Option<BrowserProxy> {
        if let Some(hosted) = self.lock().hosted.clone() {
            return hosted.browser.as_ref().map(BrowserProxy::of_route);
        }
        self.current().await;
        let route = self.lock().settings.routes.browser.clone();
        match route {
            Route::Own | Route::Auto => None,
            Route::Direct => Some(BrowserProxy::direct()),
            // As this computer's requests find it; none set: directly.
            Route::System => Some(
                system_for_browser(&Matcher::from_system()).unwrap_or_else(BrowserProxy::direct),
            ),
            Route::Proxy(id) => {
                let state = self.lock();
                Some(match state.settings.proxy(&id) {
                    Some(proxy) => BrowserProxy::of(&endpoint_of(&state, proxy)),
                    None => BrowserProxy::nowhere(),
                })
            }
        }
    }

    async fn agent_hop(&self, browser: bool, host: &str, port: u16) -> Result<Hop, String> {
        if !browser {
            return system_hop(host, port, FILTER_THROUGH);
        }
        if let Some(hosted) = self.lock().hosted.clone() {
            return match &hosted.browser {
                Some(ProxyRoute::Direct) => Ok(Hop::Direct),
                Some(ProxyRoute::Proxy(proxy)) => Ok(hop_of(proxy)),
                None => system_hop(host, port, FILTER_THROUGH),
            };
        }
        self.current().await;
        let state = self.lock();
        match &state.settings.routes.browser {
            Route::Direct => Ok(Hop::Direct),
            Route::Proxy(id) => match state.settings.proxy(id) {
                Some(proxy) => Ok(hop_of(&endpoint_of(&state, proxy))),
                None => Err(format!(
                    "{} goes through a proxy that isn't saved.",
                    Row::Browser.name()
                )),
            },
            Route::Own | Route::Auto | Route::System => system_hop(host, port, FILTER_THROUGH),
        }
    }

    async fn host_network(&self) -> HostNetwork {
        self.current().await;
        let state = self.lock();
        let settings = &state.settings;
        let route = |row: Row| match settings.routes.get(row) {
            Route::Direct => Some(ProxyRoute::Direct),
            Route::Proxy(id) => Some(match settings.proxy(id) {
                Some(proxy) => ProxyRoute::Proxy(endpoint_of(&state, proxy)),
                None => ProxyRoute::Proxy(nowhere_endpoint()),
            }),
            Route::Own | Route::Auto | Route::System => None,
        };
        HostNetwork {
            timeout_secs: settings.timeout_secs,
            models: route(Row::HostModels),
            browser: route(Row::HostBrowser),
        }
    }

    fn set_host_network(&self, network: Option<HostNetwork>) {
        let mut state = self.lock();
        if state.hosted != network {
            state.hosted = network;
            state.generation += 1;
        }
    }
}

/// The ids of the proxies whose password a route needs and that aren't
/// read yet: those that sign in, with a password saved.
fn needed(state: &State) -> Vec<String> {
    let mut ids: Vec<String> = Row::ALL
        .into_iter()
        .filter_map(|row| state.settings.proxy_for(row))
        .filter(|proxy| {
            proxy.username.is_some()
                && proxy.password_saved
                && !state.passwords.contains_key(&proxy.id)
        })
        .map(|proxy| proxy.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// A saved proxy, signed in to with its password as read.
fn endpoint_of(state: &State, proxy: &SavedProxy) -> ProxyEndpoint {
    let password = state.passwords.get(&proxy.id).cloned().flatten();
    proxy.endpoint(password)
}

/// A proxy that can't be reached, for what has to go through one that
/// isn't there.
fn nowhere_endpoint() -> ProxyEndpoint {
    ProxyEndpoint {
        kind: ProxyKind::Http,
        host: "proxy.invalid".into(),
        port: 9,
        username: None,
        password: None,
    }
}

/// Writes `saved` to the settings file, when there is one, and notes it
/// as read: this window's own change isn't another's.
fn write(state: &mut State, saved: &Saved) -> Result<(), String> {
    let Some(file) = state.file.clone() else {
        return Ok(());
    };
    let mut bytes = serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    crate::atomic_file::write_atomic(&file, &bytes)
        .map_err(|e| format!("The network settings couldn't be saved: {e}"))?;
    state.seen = Some(bytes);
    Ok(())
}

/// Whether the wall clock moved `SLEPT_BY` further than the monotonic
/// one between `before` and `now`: the computer slept between them.
fn slept(before: (SystemTime, Instant), now: (SystemTime, Instant)) -> bool {
    let wall = now.0.duration_since(before.0).unwrap_or_default();
    let monotonic = now.1.saturating_duration_since(before.1);
    wall > monotonic + SLEPT_BY
}

/// What the requests to model providers and the downloads are built from:
/// on a cloud host the way the app said, in the app its settings' way.
fn effective(state: &State) -> Effective {
    if let Some(hosted) = &state.hosted {
        let way = match &hosted.models {
            None => Way::System,
            Some(ProxyRoute::Direct) => Way::Direct,
            Some(ProxyRoute::Proxy(proxy)) => Way::Proxy(proxy.clone()),
        };
        return Effective {
            timeout: settings::timeout_of(hosted.timeout_secs),
            way,
        };
    }
    let settings = &state.settings;
    let way = match &settings.routes.models {
        Route::Direct => Way::Direct,
        Route::Proxy(id) => match settings.proxy(id) {
            Some(proxy) => Way::Proxy(endpoint_of(state, proxy)),
            None => Way::Nowhere,
        },
        Route::Auto | Route::System | Route::Own => Way::System,
    };
    Effective {
        timeout: settings.timeout(),
        way,
    }
}

/// The settings' timeout and way.
fn base_builder(effective: &Effective) -> reqwest::ClientBuilder {
    configured(reqwest::Client::builder(), effective)
}

/// `builder` with the settings' timeout and way. The system's proxy is
/// reqwest's own, found as the client is built.
fn configured(builder: reqwest::ClientBuilder, effective: &Effective) -> reqwest::ClientBuilder {
    let builder = builder.connect_timeout(effective.timeout);
    let on_this_computer = || reqwest::NoProxy::from_string(settings::ON_THIS_COMPUTER);
    let nowhere = || {
        let nowhere = reqwest::Proxy::all(NOWHERE).expect("a proxy's URL");
        nowhere.no_proxy(on_this_computer())
    };
    let proxy = match &effective.way {
        // Not even the system's.
        Way::Direct => return builder.no_proxy(),
        Way::System => return builder,
        Way::Nowhere => return builder.proxy(nowhere()),
        Way::Proxy(proxy) => proxy,
    };
    match settings::proxy_url(proxy)
        .and_then(|url| reqwest::Proxy::all(url).map_err(|e| e.to_string()))
    {
        Ok(proxy) => builder.proxy(proxy.no_proxy(on_this_computer())),
        Err(e) => {
            // Checked as they're saved and read, so not reached: and then
            // nothing goes around the proxy chosen, it fails to connect.
            log::warn!("network: the proxy can't be used: {e}");
            builder.proxy(nowhere())
        }
    }
}

fn build_shared(effective: &Effective) -> reqwest::Client {
    base_builder(effective)
        .pool_idle_timeout(POOL_IDLE)
        .http2_keep_alive_interval(PING_EVERY)
        .http2_keep_alive_timeout(PING_WAIT)
        .http2_keep_alive_while_idle(false)
        .build()
        .unwrap_or_else(|e| {
            log::warn!("network: the shared client couldn't be built, so a plain one is: {e}");
            reqwest::Client::new()
        })
}

/// The system's proxy (`system`) as Chrome takes it, as this computer's
/// requests find it, with the credentials its URL has; none when it has
/// none.
fn system_for_browser(system: &Matcher) -> Option<BrowserProxy> {
    let destination: http::Uri = "https://example.com".parse().expect("a URL");
    let found = system.intercept(&destination)?;
    let uri = found.uri();
    // Chrome's names: its SOCKS5 looks names up through the proxy anyway.
    let scheme = match uri.scheme_str()? {
        "socks5h" => "socks5",
        "socks4a" => "socks4",
        other => other,
    };
    let (username, password) = match (found.raw_auth(), found.basic_auth()) {
        (Some((user, password)), _) => (Some(user.to_string()), Some(password.to_string())),
        (None, Some(basic)) => basic
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Basic "))
            .and_then(|token| base64::engine::general_purpose::STANDARD.decode(token).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|pair| {
                pair.split_once(':')
                    .map(|(user, password)| (Some(user.to_string()), Some(password.to_string())))
            })
            .unwrap_or((None, None)),
        _ => (None, None),
    };
    Some(BrowserProxy {
        server: format!("{scheme}://{}", uri.authority()?),
        username,
        password: password.map(Secret),
    })
}

/// How a cloud host's SSH connection gets there.
fn cloud_hop(state: &State, host: &str, port: u16) -> Result<Hop, String> {
    if on_this_computer(host) {
        return Ok(Hop::Direct);
    }
    let settings = &state.settings;
    match &settings.routes.ssh {
        Route::System => system_hop(host, port, SSH_THROUGH),
        Route::Proxy(id) => match settings.proxy(id) {
            Some(proxy) => Ok(hop_of(&endpoint_of(state, proxy))),
            None => Err(format!(
                "{} goes through a proxy that isn't saved.",
                Row::Ssh.name()
            )),
        },
        Route::Direct | Route::Auto | Route::Own => Ok(Hop::Direct),
    }
}

/// A tunnel through `proxy`, signed in to as it says.
fn hop_of(proxy: &ProxyEndpoint) -> Hop {
    let user = proxy.username.as_deref();
    let password = proxy.password.as_ref().map_or("", |p| p.0.as_str());
    match proxy.kind {
        ProxyKind::Http => Hop::http(&proxy.host, proxy.port, user, password),
        ProxyKind::Socks5 => Hop::Socks5 {
            host: proxy.host.clone(),
            port: proxy.port,
            auth: user.map(|user| (user.to_string(), password.to_string())),
            remote_names: true,
        },
    }
}

/// What goes through a tunnel, and what to choose instead of a system's
/// proxy it can't go through: for [`system_hop`]'s errors.
struct Through {
    what: &'static str,
    instead: &'static str,
}

/// A cloud host's SSH connection.
const SSH_THROUGH: Through = Through {
    what: "an SSH connection",
    instead: "choose a saved HTTP or SOCKS5 proxy for SSH to cloud hosts, or no proxy",
};

/// The allowed hosts' filter going on.
const FILTER_THROUGH: Through = Through {
    what: "Rovik's filter for the allowed hosts",
    instead: "set an HTTP or SOCKS5 proxy instead, or none",
};

/// The system's proxy for an HTTPS connection to `host:port`, which is
/// what a tunnel to it is: an HTTP proxy's CONNECT, or SOCKS5. `through`
/// says what the tunnel is for, in an error.
fn system_hop(host: &str, port: u16, through: Through) -> Result<Hop, String> {
    let destination: http::Uri = format!("https://{}", authority(host, port))
        .parse()
        .map_err(|_| format!("{host} isn't a host's name or address."))?;
    let Some(proxy) = Matcher::from_system().intercept(&destination) else {
        return Ok(Hop::Direct);
    };
    let uri = proxy.uri();
    let shown = shown(uri);
    let proxy_host = uri
        .host()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
        .ok_or_else(|| format!("The system's proxy, {shown}, has no host."))?;
    match uri.scheme_str() {
        Some("http") => Ok(Hop::Http {
            host: proxy_host,
            port: uri.port_u16().unwrap_or(80),
            auth: proxy
                .basic_auth()
                .and_then(|value| value.to_str().ok())
                .map(String::from),
        }),
        Some(scheme @ ("socks5" | "socks5h")) => Ok(Hop::Socks5 {
            host: proxy_host,
            port: uri.port_u16().unwrap_or(1080),
            auth: proxy
                .raw_auth()
                .map(|(user, password)| (user.to_string(), password.to_string())),
            remote_names: scheme == "socks5h",
        }),
        Some("https") => Err(format!(
            "The system's proxy, {shown}, is reached over HTTPS, which {} can't go through: {}.",
            through.what, through.instead
        )),
        _ => Err(format!(
            "The system's proxy, {shown}, isn't one {} can go through: {}.",
            through.what, through.instead
        )),
    }
}

/// The system's proxy, as its page says it.
fn system_proxy() -> SystemProxy {
    let matcher = Matcher::from_system();
    let proxy_for = |url: &str| {
        let destination: http::Uri = url.parse().expect("a URL");
        matcher
            .intercept(&destination)
            .map(|proxy| shown(proxy.uri()))
    };
    SystemProxy {
        https: proxy_for("https://example.com"),
        http: proxy_for("http://example.com"),
    }
}

/// `scheme://host:port`, without the path the matcher gives it.
fn shown(uri: &http::Uri) -> String {
    match (uri.scheme_str(), uri.authority()) {
        (Some(scheme), Some(authority)) => format!("{scheme}://{authority}"),
        _ => uri.to_string(),
    }
}
