//! What Settings → Network sets: how long connecting may take, the proxies
//! Rovik can use, and where each kind of connection goes (`Routes`): this
//! computer's requests to model providers and its downloads, its SSH
//! connections to cloud hosts, its agent's browser, and the cloud hosts'
//! requests and browsers. Kept in network-settings.json (`net::init`); the
//! proxies' passwords aren't, they're in the app's keychain.

use rovik_protocol::network::{ProxyEndpoint, ProxyKind};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::Ipv6Addr;
use std::time::Duration;

/// How long connecting may take when no timeout is set, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u32 = 30;
/// The timeouts that can be set, in seconds.
pub const MIN_TIMEOUT_SECS: u32 = 5;
pub const MAX_TIMEOUT_SECS: u32 = 300;

/// The longest username and password a SOCKS5 proxy takes (RFC 1929).
const MAX_CREDENTIAL_BYTES: usize = 255;
/// The longest id a proxy has.
const MAX_ID_CHARS: usize = 40;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkSettings {
    /// How long connecting to a server, a proxy or a cloud host may take,
    /// in seconds, here and on cloud hosts. None: [`DEFAULT_TIMEOUT_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    /// The proxies the routes can name.
    pub proxies: Vec<SavedProxy>,
    pub routes: Routes,
}

/// A proxy the routes can name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedProxy {
    /// What the routes name it by, and its password's in the keychain.
    pub id: String,
    pub kind: ProxyKind,
    /// Its name or address, without a scheme or a port.
    pub host: String,
    pub port: u16,
    /// Signs in to it with this name and its password in the keychain.
    /// None: it's used without signing in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// A password for it is in the keychain. The backend keeps it: what the
    /// page sends is taken from what's saved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password_saved: bool,
}

/// Where one kind of connection goes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    /// This computer's requests to model providers only: the system's
    /// proxy, which is none while the system has none set.
    Auto,
    /// The way it goes on its own: the browser's own settings, or on a
    /// cloud host the host's.
    Own,
    /// No proxy: directly, even when the computer has a proxy set.
    Direct,
    /// This computer's proxy: on macOS and Windows its network settings,
    /// and everywhere the HTTPS_PROXY, HTTP_PROXY, ALL_PROXY and NO_PROXY
    /// variables. None set: directly.
    System,
    /// One of the saved proxies, by its id.
    Proxy(String),
}

/// A kind of connection: a row of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// This computer's requests to model providers, and its downloads.
    Models,
    /// This computer's SSH connections to cloud hosts.
    Ssh,
    /// The agent's browser on this computer.
    Browser,
    /// Cloud hosts' requests to model providers, their downloads and
    /// Autopilot's notices.
    HostModels,
    /// The agent's browser on cloud hosts.
    HostBrowser,
}

impl Row {
    pub const ALL: [Row; 5] = [
        Row::Models,
        Row::Ssh,
        Row::Browser,
        Row::HostModels,
        Row::HostBrowser,
    ];

    /// As the page names it.
    pub fn name(self) -> &'static str {
        match self {
            Row::Models => "Model providers and downloads on this computer",
            Row::Ssh => "SSH to cloud hosts",
            Row::Browser => "The agent's browser on this computer",
            Row::HostModels => "Model providers and downloads on cloud hosts",
            Row::HostBrowser => "The agent's browser on cloud hosts",
        }
    }

    /// Whether it can go `route`'s way: "Automatic" is for this computer's
    /// requests, "System proxy" this computer's alone, and "Its own" for
    /// what has a way of its own.
    pub fn takes(self, route: &Route) -> bool {
        match route {
            Route::Direct | Route::Proxy(_) => true,
            Route::Auto => self == Row::Models,
            Route::System => matches!(self, Row::Models | Row::Ssh | Row::Browser),
            Route::Own => matches!(self, Row::Browser | Row::HostModels | Row::HostBrowser),
        }
    }

    /// Where it goes until a way is chosen.
    fn default_route(self) -> Route {
        match self {
            Row::Models => Route::Auto,
            Row::Ssh => Route::Direct,
            Row::Browser | Row::HostModels | Row::HostBrowser => Route::Own,
        }
    }
}

/// Where each kind of connection goes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Routes {
    pub models: Route,
    pub ssh: Route,
    pub browser: Route,
    pub host_models: Route,
    pub host_browser: Route,
}

impl Default for Routes {
    fn default() -> Self {
        Routes {
            models: Row::Models.default_route(),
            ssh: Row::Ssh.default_route(),
            browser: Row::Browser.default_route(),
            host_models: Row::HostModels.default_route(),
            host_browser: Row::HostBrowser.default_route(),
        }
    }
}

impl Routes {
    pub fn get(&self, row: Row) -> &Route {
        match row {
            Row::Models => &self.models,
            Row::Ssh => &self.ssh,
            Row::Browser => &self.browser,
            Row::HostModels => &self.host_models,
            Row::HostBrowser => &self.host_browser,
        }
    }
}

impl NetworkSettings {
    /// How long connecting may take.
    pub fn timeout(&self) -> Duration {
        timeout_of(self.timeout_secs)
    }

    /// Spaces around the hosts and the usernames dropped, and an empty
    /// username taken for none.
    pub fn normalized(mut self) -> Self {
        for proxy in &mut self.proxies {
            proxy.host = proxy.host.trim().to_string();
            proxy.username = proxy
                .username
                .take()
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty());
        }
        self
    }

    /// Why these settings can't be used, when they can't.
    pub fn check(&self) -> Result<(), String> {
        if let Some(secs) = self.timeout_secs {
            if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&secs) {
                return Err(format!(
                    "The timeout can be from {MIN_TIMEOUT_SECS} to {MAX_TIMEOUT_SECS} seconds."
                ));
            }
        }
        let mut ids = HashSet::new();
        for proxy in &self.proxies {
            let id = proxy.id.as_str();
            let plain = id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if id.is_empty() || id.chars().count() > MAX_ID_CHARS || !plain {
                return Err(format!("{id:?} isn't a proxy's id."));
            }
            if !ids.insert(id) {
                return Err(format!("Two proxies have the id {id:?}."));
            }
            proxy.check()?;
        }
        for row in Row::ALL {
            let route = self.routes.get(row);
            if !row.takes(route) {
                return Err(format!("{} can't go that way.", row.name()));
            }
            if let Route::Proxy(id) = route {
                if !ids.contains(id.as_str()) {
                    return Err(format!(
                        "{} goes through a proxy that isn't saved.",
                        row.name()
                    ));
                }
            }
        }
        Ok(())
    }

    /// The saved proxy with this id.
    pub fn proxy(&self, id: &str) -> Option<&SavedProxy> {
        self.proxies.iter().find(|p| p.id == id)
    }

    /// The saved proxy `row` goes through, when it goes through one.
    pub fn proxy_for(&self, row: Row) -> Option<&SavedProxy> {
        match self.routes.get(row) {
            Route::Proxy(id) => self.proxy(id),
            _ => None,
        }
    }

    /// The rows that go through the proxy `id`.
    pub fn uses(&self, id: &str) -> Vec<Row> {
        Row::ALL
            .into_iter()
            .filter(|&row| matches!(self.routes.get(row), Route::Proxy(p) if p == id))
            .collect()
    }
}

/// How long connecting may take, `secs` or the default, within the range.
pub(crate) fn timeout_of(secs: Option<u32>) -> Duration {
    let secs = secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
    Duration::from_secs(u64::from(secs))
}

impl SavedProxy {
    /// Why it can't be used, when it can't.
    pub fn check(&self) -> Result<(), String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Enter the proxy's host.".into());
        }
        if host.contains("://") || host.contains(['/', '@', '?', '#']) {
            return Err(
                "Enter only the proxy's name or address, such as proxy.example.com.".into(),
            );
        }
        if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("The proxy's host can't have spaces in it.".into());
        }
        if host.contains(':') && bare_ipv6(host).is_none() {
            return Err("Enter the proxy's port in its own field.".into());
        }
        if host.len() > 253 {
            return Err("The proxy's host name is too long.".into());
        }
        if self.port == 0 {
            return Err("Enter the proxy's port.".into());
        }
        if self.kind == ProxyKind::Socks5
            && self
                .username
                .as_ref()
                .is_some_and(|u| u.len() > MAX_CREDENTIAL_BYTES)
        {
            return Err(format!(
                "A SOCKS5 proxy takes a username of {MAX_CREDENTIAL_BYTES} bytes at most."
            ));
        }
        // What the clients are given has to be a URL.
        proxy_url(&self.endpoint(None)).map(|_| ())
    }

    /// Why `password` can't sign in to it, when it can't: a SOCKS5 proxy
    /// takes 255 bytes at most.
    pub fn check_password(&self, password: &Secret) -> Result<(), String> {
        if self.kind == ProxyKind::Socks5 && password.0.len() > MAX_CREDENTIAL_BYTES {
            return Err(format!(
                "A SOCKS5 proxy takes a password of {MAX_CREDENTIAL_BYTES} bytes at most."
            ));
        }
        Ok(())
    }

    /// `host:port`, an IPv6 address in brackets.
    pub fn address(&self) -> String {
        authority(&self.host, self.port)
    }

    /// It, signed in to with `password` when it has a username.
    pub fn endpoint(&self, password: Option<Secret>) -> ProxyEndpoint {
        ProxyEndpoint {
            kind: self.kind,
            host: self.host.trim().to_string(),
            port: self.port,
            username: self.username.clone(),
            password: self.username.as_ref().and(password),
        }
    }
}

/// A proxy as a message says it: "the HTTP proxy at proxy.example.com:3128".
pub fn described(proxy: &ProxyEndpoint) -> String {
    let kind = match proxy.kind {
        ProxyKind::Http => "HTTP",
        ProxyKind::Socks5 => "SOCKS5",
    };
    format!("the {kind} proxy at {}", authority(&proxy.host, proxy.port))
}

/// A proxy's URL for reqwest, signed in to when it has a username:
/// `http://` for HTTP, `socks5h://` for SOCKS5 (the proxy looks the names
/// up).
pub(crate) fn proxy_url(proxy: &ProxyEndpoint) -> Result<reqwest::Url, String> {
    let scheme = match proxy.kind {
        ProxyKind::Http => "http",
        ProxyKind::Socks5 => "socks5h",
    };
    let address = authority(proxy.host.trim(), proxy.port);
    let mut url = reqwest::Url::parse(&format!("{scheme}://{address}"))
        .map_err(|_| format!("{address} isn't an address a proxy can have."))?;
    if let Some(user) = proxy.username.as_deref() {
        let password = proxy.password.as_ref();
        if proxy.kind == ProxyKind::Socks5
            && password.is_some_and(|p| p.0.len() > MAX_CREDENTIAL_BYTES)
        {
            return Err(format!(
                "A SOCKS5 proxy takes a password of {MAX_CREDENTIAL_BYTES} bytes at most."
            ));
        }
        // Percent-encoded here, decoded where it's sent.
        let signed_in = url
            .set_username(user)
            .and_then(|()| url.set_password(Some(password.map_or("", |p| p.0.as_str()))));
        if signed_in.is_err() {
            return Err(format!("{address} can't be signed in to."));
        }
    }
    Ok(url)
}

/// `host:port`, an IPv6 address in brackets.
pub fn authority(host: &str, port: u16) -> String {
    match bare_ipv6(host) {
        Some(ip) => format!("[{ip}]:{port}"),
        None => format!("{host}:{port}"),
    }
}

/// `host` as a connection takes it: an IPv6 address without the brackets
/// a URL puts around it, or the host as given.
pub fn bare_host(host: &str) -> String {
    match bare_ipv6(host) {
        Some(ip) => ip.to_string(),
        None => host.to_string(),
    }
}

/// An IPv6 address, with or without its brackets.
fn bare_ipv6(host: &str) -> Option<Ipv6Addr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse().ok()
}

/// A host on this computer, which no proxy is asked for: `localhost` and
/// its names, and the loopback addresses.
pub fn on_this_computer(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// The same hosts, as reqwest's `NoProxy` reads them.
pub(crate) const ON_THIS_COMPUTER: &str = "localhost,127.0.0.0/8,::1";
