//! What Settings → Network sets: how long connecting may take, the proxy
//! connections go through (none, the system's or a custom one), and
//! whether cloud hosts' SSH connections and the agent's browsers do too.
//! Kept in network-settings.json (`net::init`); the custom proxy's
//! password isn't, it's in the app's keychain.

use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::net::Ipv6Addr;
use std::time::Duration;

/// How long connecting may take when no timeout is set, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u32 = 30;
/// The timeouts that can be set, in seconds.
pub const MIN_TIMEOUT_SECS: u32 = 5;
pub const MAX_TIMEOUT_SECS: u32 = 300;

/// The longest username and password a SOCKS5 proxy takes (RFC 1929).
const MAX_CREDENTIAL_BYTES: usize = 255;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkSettings {
    /// How long connecting to a server, a proxy or a cloud host may take,
    /// in seconds. None: [`DEFAULT_TIMEOUT_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    pub proxy: ProxyMode,
    /// The custom proxy. Kept while another choice is used, so going back
    /// to it needs nothing typed again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomProxy>,
    /// Cloud hosts' SSH connections go through the proxy too, when there's
    /// one (`proxied`). Kept as it is while there's none, as is `browser`.
    pub cloud_hosts: bool,
    /// The agent's browser goes through the proxy too, where
    /// `browser_places` says.
    pub browser: bool,
    pub browser_places: BrowserPlaces,
}

/// Which of the agent's browsers go through the proxy.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum BrowserPlaces {
    /// The one on this computer.
    #[default]
    Local,
    /// The ones cloud hosts start for the chats on them.
    Cloud,
    /// Both.
    Both,
}

impl BrowserPlaces {
    pub fn local(self) -> bool {
        matches!(self, BrowserPlaces::Local | BrowserPlaces::Both)
    }

    pub fn cloud(self) -> bool {
        matches!(self, BrowserPlaces::Cloud | BrowserPlaces::Both)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ProxyMode {
    /// No proxy: connections go directly, even when this computer has a
    /// proxy set. What the app's settings start with.
    #[default]
    Direct,
    /// This computer's proxy: on macOS and Windows its network settings,
    /// and everywhere the HTTPS_PROXY, HTTP_PROXY, ALL_PROXY and NO_PROXY
    /// variables. None set: connections go directly.
    System,
    /// The proxy set here, `NetworkSettings::custom`.
    Custom,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProxyKind {
    /// An HTTP proxy: HTTPS and SSH go through it by CONNECT.
    Http,
    /// A SOCKS5 proxy, which looks up the names it's given itself.
    Socks5,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomProxy {
    pub kind: ProxyKind,
    /// Its name or address, without a scheme or a port.
    pub host: String,
    pub port: u16,
    /// Signs in to it with this name and the password in the keychain.
    /// None: it's used without signing in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl NetworkSettings {
    /// How long connecting may take.
    pub fn timeout(&self) -> Duration {
        let secs = self
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        Duration::from_secs(u64::from(secs))
    }

    /// Spaces around the host and the username dropped, and an empty
    /// username taken for none.
    pub fn normalized(mut self) -> Self {
        if let Some(custom) = &mut self.custom {
            custom.host = custom.host.trim().to_string();
            custom.username = custom
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
        if let Some(custom) = &self.custom {
            custom.check()?;
        } else if self.proxy == ProxyMode::Custom {
            return Err("Enter the proxy's host and port.".into());
        }
        Ok(())
    }

    /// The custom proxy, when it's the one in use.
    pub fn custom_in_use(&self) -> Option<&CustomProxy> {
        match self.proxy {
            ProxyMode::Custom => self.custom.as_ref(),
            ProxyMode::Direct | ProxyMode::System => None,
        }
    }

    /// A proxy is used, the system's or the custom one: not with "No
    /// proxy", which nothing goes through a proxy with, cloud hosts and
    /// browsers neither.
    pub fn proxied(&self) -> bool {
        self.proxy != ProxyMode::Direct
    }
}

impl CustomProxy {
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
        self.url(None).map(|_| ())
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

    /// Its URL for reqwest, signed in with `password` when it has a
    /// username: `http://` for HTTP, `socks5h://` for SOCKS5 (the proxy
    /// looks the names up).
    pub(crate) fn url(&self, password: Option<&Secret>) -> Result<reqwest::Url, String> {
        let scheme = match self.kind {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5h",
        };
        let address = authority(self.host.trim(), self.port);
        let mut url = reqwest::Url::parse(&format!("{scheme}://{address}"))
            .map_err(|_| format!("{address} isn't an address a proxy can have."))?;
        if let Some(user) = self.username.as_deref() {
            if let Some(password) = password {
                self.check_password(password)?;
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
}

/// `host:port`, an IPv6 address in brackets.
pub fn authority(host: &str, port: u16) -> String {
    match bare_ipv6(host) {
        Some(ip) => format!("[{ip}]:{port}"),
        None => format!("{host}:{port}"),
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
