//! The app's connections out, as Settings → Network sets them.
//!
//! - One HTTP client is shared by the requests to model providers
//!   (`client`), so their connections are kept open and reused, and on
//!   HTTP/2 requests at once share one. Downloads build their own clients
//!   from `builder`; cloud hosts' SSH connections start at
//!   `open_cloud_host`.
//! - All take the settings' timeout for connecting, and their proxy: none
//!   ("No proxy", the first choice, whatever the system has set), the
//!   system's, as reqwest finds it, or a custom HTTP or SOCKS5 one, which
//!   nothing on this computer (localhost) goes through. Cloud hosts'
//!   connections and the agent's browsers go through it only when the
//!   settings say so.
//! - The shared client, and its connections with it, is replaced when the
//!   settings change; after an attempt that may have failed for its
//!   connection (`refresh`); after the computer slept, as a connection
//!   kept from before is likely dead and a request on it would wait out
//!   its first-byte limit; and every ten minutes, which follows a change of
//!   the system's proxy. A request keeps the client it started with. While
//!   one is under way on HTTP/2, pings find a dead connection.
//!
//! The settings are network-settings.json, named by `init`. Another of
//! Rovik's windows may save it: it's read again when it changed. The custom
//! proxy's password is in the app's keychain: the file says whether one is
//! saved and when it last changed, and `init`'s `password` reads it, off
//! the async threads, as a keychain may ask the user first. A process that
//! never calls `init` (a cloud host's daemon, the tests) follows the
//! system's proxy, as it always did, and has 30 s to connect.

mod settings;
#[cfg(test)]
mod tests;
mod tunnel;

pub use rovik_protocol::agent::BrowserProxy;
pub use settings::{
    authority, on_this_computer, BrowserPlaces, CustomProxy, NetworkSettings, ProxyKind, ProxyMode,
    DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS,
};
pub use tunnel::Link;

use base64::Engine;
use hyper_util::client::proxy::matcher::Matcher;
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tunnel::Hop;

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
/// After the password couldn't be read, how long before it's tried again.
const PASSWORD_RETRY: Duration = Duration::from_secs(30);

/// Reads the custom proxy's password: none when none is saved.
pub type PasswordSource = Arc<dyn Fn() -> Result<Option<Secret>, String> + Send + Sync>;

/// network-settings.json.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Saved {
    #[serde(flatten)]
    settings: NetworkSettings,
    /// A password for the custom proxy is in the keychain.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    password_saved: bool,
    /// When it last changed, in ms since 1970: another window reads it
    /// again then.
    #[serde(skip_serializing_if = "Option::is_none")]
    password_changed: Option<u64>,
}

/// Settings → Network's page.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetworkView {
    pub settings: NetworkSettings,
    /// A password for the custom proxy is saved.
    pub password_saved: bool,
    /// The proxy "System proxy" uses now.
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
    password: Option<Secret>,
    password_saved: bool,
    /// When the password last changed, as the file says.
    password_changed: Option<u64>,
    /// The password is to be read (again).
    password_due: bool,
    /// No read of it before then: one is under way, or the last failed.
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
    /// On a cloud host: the browser proxy the app sent (`AgentConfig`),
    /// none for none. Not set in the app, whose settings say.
    hosted_browser: Option<Option<BrowserProxy>>,
}

struct Shared {
    client: reqwest::Client,
    generation: u64,
    built: Instant,
}

/// What the clients are built from now.
struct Effective {
    timeout: Duration,
    route: Route,
}

/// How the clients' connections get to their servers.
enum Route {
    /// Directly, whatever proxy the system has: "No proxy".
    Direct,
    /// Through the system's proxy, as reqwest finds it.
    System,
    /// Through the custom proxy, signed in to with its password.
    Custom(CustomProxy, Option<Secret>),
}

struct Net {
    state: Mutex<State>,
}

impl Default for Net {
    /// Until `init` reads the app's settings, the system's proxy: what a
    /// process without settings of its own, a cloud host's daemon, follows.
    fn default() -> Self {
        let settings = NetworkSettings {
            proxy: ProxyMode::System,
            ..NetworkSettings::default()
        };
        Net {
            state: Mutex::new(State {
                settings,
                ..State::default()
            }),
        }
    }
}

static NET: LazyLock<Net> = LazyLock::new(Net::default);

/// Reads the settings from `file`, and the custom proxy's password, when
/// it's needed, with `password`.
pub fn init(file: PathBuf, password: PasswordSource) {
    NET.init(file, password);
}

/// The settings, for their page.
pub fn view() -> NetworkView {
    NET.view()
}

/// Keeps `settings` and applies them; refused when they can't be used.
pub fn save(settings: NetworkSettings) -> Result<NetworkView, String> {
    NET.save(settings)?;
    Ok(NET.view())
}

/// The app saved the custom proxy's password, or forgot it (none):
/// applied, and noted for the other windows.
pub fn password_saved(password: Option<Secret>) -> Result<NetworkView, String> {
    NET.password_saved(password)?;
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

/// A builder with the settings' timeout and proxy, for a client of its
/// own (a download).
pub async fn builder() -> reqwest::ClientBuilder {
    NET.builder().await
}

/// A TCP connection to a cloud host's SSH server: through the proxy when
/// the settings say cloud hosts' connections go through it.
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

/// The proxy for the agent's browser in this process: on a cloud host the
/// one the app sent (`set_hosted_browser_proxy`); in the app the settings'
/// when "Use proxy for browser" covers this computer. None, as with "No
/// proxy": Chrome's own settings, which follow the system's proxy.
pub async fn browser_proxy() -> Option<BrowserProxy> {
    NET.browser_proxy().await
}

/// The proxy for the browsers of the cloud hosts, sent to them with the
/// settings runs follow (`AgentConfig`): the settings' when "Use proxy for
/// browser" covers the cloud.
pub async fn cloud_browser_proxy() -> Option<BrowserProxy> {
    NET.cloud_browser_proxy().await
}

/// On a cloud host: the browser proxy the app sent, for the browser this
/// process starts from now on (the last computer to send one has it).
pub fn set_hosted_browser_proxy(proxy: Option<BrowserProxy>) {
    NET.lock().hosted_browser = Some(proxy);
}

/// The custom proxy a request to `url` goes through, as a message says it
/// ("the HTTP proxy at proxy.example.com:3128"); none when it goes
/// directly or through the system's.
pub fn custom_proxy_for(url: &str) -> Option<String> {
    let host = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
    if on_this_computer(&host) {
        return None;
    }
    let state = NET.lock();
    let proxy = state.settings.custom_in_use()?;
    let kind = match proxy.kind {
        ProxyKind::Http => "HTTP",
        ProxyKind::Socks5 => "SOCKS5",
    };
    Some(format!("the {kind} proxy at {}", proxy.address()))
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
            state.password_due = true;
            state.password_not_before = None;
            // The app's own defaults, "No proxy", until the file says more.
            state.settings = NetworkSettings::default();
            state.seen = None;
            state.generation += 1;
        }
        self.follow(true);
    }

    fn view(&self) -> NetworkView {
        self.follow(true);
        let (settings, password_saved) = {
            let state = self.lock();
            (state.settings.clone(), state.password_saved)
        };
        NetworkView {
            settings,
            password_saved,
            system: system_proxy(),
            default_timeout_secs: DEFAULT_TIMEOUT_SECS,
            min_timeout_secs: MIN_TIMEOUT_SECS,
            max_timeout_secs: MAX_TIMEOUT_SECS,
        }
    }

    fn save(&self, settings: NetworkSettings) -> Result<(), String> {
        let settings = settings.normalized();
        settings.check()?;
        let mut state = self.lock();
        let saved = Saved {
            settings: settings.clone(),
            password_saved: state.password_saved,
            password_changed: state.password_changed,
        };
        write(&mut state, &saved)?;
        if state.settings != settings {
            state.settings = settings;
            state.generation += 1;
        }
        Ok(())
    }

    fn password_saved(&self, password: Option<Secret>) -> Result<(), String> {
        let mut state = self.lock();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let changed = now.max(state.password_changed.map_or(0, |c| c + 1));
        let saved = Saved {
            settings: state.settings.clone(),
            password_saved: password.is_some(),
            password_changed: Some(changed),
        };
        write(&mut state, &saved)?;
        state.password_saved = password.is_some();
        state.password_changed = Some(changed);
        state.password_due = false;
        state.password_not_before = None;
        if state.password != password {
            state.password = password;
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
        state.password_saved = saved.password_saved;
        if saved.password_changed != state.password_changed {
            state.password_changed = saved.password_changed;
            state.password_due = true;
            state.password_not_before = None;
        }
    }

    /// Reads the password, when the custom proxy in use signs in and it's
    /// due: off the async threads.
    async fn read_password(&self) {
        let source = {
            let mut state = self.lock();
            let needed = state
                .settings
                .custom_in_use()
                .is_some_and(|p| p.username.is_some());
            if !state.password_due || !needed {
                return;
            }
            if state
                .password_not_before
                .is_some_and(|t| Instant::now() < t)
            {
                return;
            }
            let Some(source) = state.source.clone() else {
                state.password_due = false;
                return;
            };
            // The others go on with what there is meanwhile.
            state.password_not_before = Some(Instant::now() + PASSWORD_RETRY);
            source
        };
        let read = crate::rt::spawn_blocking(move || source())
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let mut state = self.lock();
        match read {
            Ok(password) => {
                state.password_due = false;
                state.password_not_before = None;
                if state.password != password {
                    state.password = password;
                    state.generation += 1;
                }
            }
            Err(e) => log::warn!("network: the proxy's password couldn't be read: {e}"),
        }
    }

    async fn current(&self) {
        self.follow(false);
        self.read_password().await;
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

    async fn open_cloud_host(&self, host: &str, port: u16) -> Result<Link, String> {
        self.current().await;
        let (hop, timeout) = {
            let state = self.lock();
            (cloud_hop(&state, host, port)?, state.settings.timeout())
        };
        tunnel::open(host, port, &hop, timeout).await
    }

    async fn browser_proxy(&self) -> Option<BrowserProxy> {
        if let Some(hosted) = self.lock().hosted_browser.clone() {
            return hosted;
        }
        self.current().await;
        self.proxy_for_browsers(BrowserPlaces::local)
    }

    async fn cloud_browser_proxy(&self) -> Option<BrowserProxy> {
        self.current().await;
        self.proxy_for_browsers(BrowserPlaces::cloud)
    }

    /// The proxy for the browsers `covers` picks, when the settings send
    /// them through one.
    fn proxy_for_browsers(&self, covers: fn(BrowserPlaces) -> bool) -> Option<BrowserProxy> {
        let wanted = |state: &State| {
            let settings = &state.settings;
            settings.proxied() && settings.browser && covers(settings.browser_places)
        };
        if !wanted(&self.lock()) {
            return None;
        }
        // Found before the lock is taken again: it may ask the system.
        let system = Matcher::from_system();
        let state = self.lock();
        wanted(&state)
            .then(|| proxy_for_browser(&state, &system))
            .flatten()
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

fn effective(state: &State) -> Effective {
    let settings = &state.settings;
    let route = match settings.custom_in_use() {
        Some(proxy) => Route::Custom(proxy.clone(), state.password.clone()),
        None if !settings.proxied() => Route::Direct,
        None => Route::System,
    };
    Effective {
        timeout: settings.timeout(),
        route,
    }
}

/// The settings' timeout and proxy. The system's proxy is reqwest's own,
/// found as the client is built.
fn base_builder(effective: &Effective) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().connect_timeout(effective.timeout);
    let (proxy, password) = match &effective.route {
        // Not even the system's.
        Route::Direct => return builder.no_proxy(),
        Route::System => return builder,
        Route::Custom(proxy, password) => (proxy, password),
    };
    let on_this_computer = || reqwest::NoProxy::from_string(settings::ON_THIS_COMPUTER);
    match proxy
        .url(password.as_ref())
        .and_then(|url| reqwest::Proxy::all(url).map_err(|e| e.to_string()))
    {
        Ok(custom) => builder.proxy(custom.no_proxy(on_this_computer())),
        Err(e) => {
            // Checked as they're saved and read, so not reached: and then
            // nothing goes around the proxy chosen, it fails to connect.
            log::warn!("network: the custom proxy can't be used: {e}");
            let nowhere = reqwest::Proxy::all("http://proxy.invalid:9").expect("a proxy's URL");
            builder.proxy(nowhere.no_proxy(on_this_computer()))
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

/// The proxy in use, as Chrome takes it: the custom one, or the system's
/// (`system`) as this computer's requests find it, with the credentials
/// its URL has; none when it has none.
fn proxy_for_browser(state: &State, system: &Matcher) -> Option<BrowserProxy> {
    if let Some(proxy) = state.settings.custom_in_use() {
        let scheme = match proxy.kind {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5",
        };
        return Some(BrowserProxy {
            server: format!("{scheme}://{}", proxy.address()),
            username: proxy.username.clone(),
            password: proxy.username.as_ref().and(state.password.clone()),
        });
    }
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
    let settings = &state.settings;
    if !settings.proxied() || !settings.cloud_hosts || on_this_computer(host) {
        return Ok(Hop::Direct);
    }
    match state.settings.custom_in_use() {
        Some(proxy) => {
            let user = proxy.username.as_deref();
            let password = state.password.as_ref().map_or("", |p| p.0.as_str());
            Ok(match proxy.kind {
                ProxyKind::Http => Hop::http(&proxy.host, proxy.port, user, password),
                ProxyKind::Socks5 => Hop::Socks5 {
                    host: proxy.host.clone(),
                    port: proxy.port,
                    auth: user.map(|user| (user.to_string(), password.to_string())),
                    remote_names: true,
                },
            })
        }
        None => system_hop(host, port),
    }
}

/// The system's proxy for an HTTPS connection to `host:port`, which is
/// what a tunnel to it is: an HTTP proxy's CONNECT, or SOCKS5.
fn system_hop(host: &str, port: u16) -> Result<Hop, String> {
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
            "The system's proxy, {shown}, is reached over HTTPS, which a cloud host's \
             connection can't go through: choose a custom proxy, or don't use the proxy \
             for cloud hosts."
        )),
        _ => Err(format!(
            "The system's proxy, {shown}, isn't one a cloud host's connection can go \
             through: choose a custom HTTP or SOCKS5 proxy, or don't use the proxy for \
             cloud hosts."
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
