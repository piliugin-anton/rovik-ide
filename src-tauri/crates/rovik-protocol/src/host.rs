//! What a host is and has (the Host group of 4.2), for the hosts page,
//! deploy and upgrade.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    /// The daemon's version.
    pub version: String,
    pub protocol: u32,
    pub min_protocol: u32,
    /// The server's system and processor ("linux", "x86_64").
    pub os: String,
    pub arch: String,
    /// The account the daemon runs as.
    pub user: String,
    /// The virtual files are mounted, so CLM chats can use them (K15).
    pub virtual_files: bool,
    /// Why they aren't, when they aren't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_files_problem: Option<String>,
    /// The Chrome the browser tools would start, when one is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome: Option<String>,
    /// Unprivileged user namespaces are allowed (Chrome's sandbox needs them).
    pub user_namespaces: bool,
    /// Rovik as installed for the whole server (D17), when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<Install>,
    /// Daemons of other users running on the server (5.5).
    pub other_daemons: u32,
}

/// Rovik's install on a server (/etc/rovik/agentd.toml).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Install {
    /// The accounts that can change its limits and upgrade it.
    pub admins: Vec<String>,
}
