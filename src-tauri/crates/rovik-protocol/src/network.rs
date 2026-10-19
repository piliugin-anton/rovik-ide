//! What a cloud host is given for its own connections (Settings → Network,
//! "Cloud hosts"): how long connecting may take, and where its requests to
//! model providers and its agent's browser go. It comes with the settings
//! runs follow (`AgentConfig::network`), each time a computer connects and
//! when they change; a host keeps the proxies' passwords in memory only.

use crate::Secret;
use serde::{Deserialize, Serialize};

/// A proxy's kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProxyKind {
    /// An HTTP proxy: HTTPS and SSH go through it by CONNECT.
    Http,
    /// A SOCKS5 proxy, which looks up the names it's given itself.
    Socks5,
}

/// A proxy to go through, and what signs in to it. Its `Debug` leaves the
/// password out (`Secret`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEndpoint {
    pub kind: ProxyKind,
    /// Its name or address, without a scheme or a port.
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<Secret>,
}

/// Where a kind of connection goes when not the way the host has it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProxyRoute {
    /// Directly, even when the host has a proxy set.
    Direct,
    /// Through this proxy.
    Proxy(ProxyEndpoint),
}

/// Settings → Network's rows for cloud hosts, as a host is given them.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct HostNetwork {
    /// How long connecting may take, in seconds; none: the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    /// Its requests to model providers, its downloads and Autopilot's
    /// notices. None: the host's own settings, its proxy variables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<ProxyRoute>,
    /// The agent's browser there. None: Chrome's own settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<ProxyRoute>,
}

impl HostNetwork {
    /// Without the proxies' passwords: what a host may write down.
    pub fn without_passwords(&self) -> HostNetwork {
        let forget = |route: &Option<ProxyRoute>| match route {
            Some(ProxyRoute::Proxy(proxy)) => Some(ProxyRoute::Proxy(ProxyEndpoint {
                password: None,
                ..proxy.clone()
            })),
            other => other.clone(),
        };
        HostNetwork {
            timeout_secs: self.timeout_secs,
            models: forget(&self.models),
            browser: forget(&self.browser),
        }
    }
}
