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
    /// The connectors whose program is installed there, where a chat's run
    /// finds it when the connector's settings name none, by id
    /// (`claude-code`). Ids rather than `ConnectorKind`s, so an app that
    /// doesn't know a newer daemon's kind still reads the rest. None from a
    /// daemon that can't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectors: Option<Vec<String>>,
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

/// A provider whose key the host keeps (`KeysList`): never the key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    /// The part of a model's id before `::`.
    pub provider: String,
    /// The only base URL the key goes to.
    pub base_url: String,
}

/// What Rovik's folders take on the host, in bytes (`Storage`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    /// The projects' bare repositories.
    pub projects: u64,
    /// The chats' worktrees.
    pub work: u64,
    /// The chat database, attachments and the agent's log.
    pub data: u64,
    /// The agent's temporary files.
    pub tmp: u64,
}

impl Storage {
    /// What the folders take together.
    pub fn total(&self) -> u64 {
        self.projects + self.work + self.data + self.tmp
    }
}

/// What `rovik-agentd doctor --json` says of the account and the server
/// (R03): its checks, a line each, and what the connection test reads once
/// the daemon is deployed (H04).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Doctor {
    /// The daemon's version, and the protocols it speaks and serves.
    pub version: String,
    pub protocol: u32,
    pub min_protocol: u32,
    /// git's version, where it's installed: the repositories' cleanup runs
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    pub findings: Vec<Finding>,
    /// The file systems Rovik's folders are on.
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// Rovik's install for the whole server, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallLimits>,
    /// The heavy commands the daemon runs at once (5.5): the number the
    /// user set, or one while other users' daemons run; none when the
    /// machine decides. Never more than the administrators' cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_cap: Option<usize>,
    /// Other users' daemons running here.
    pub other_daemons: u32,
    pub storage: Storage,
    /// What the daemon's logs take, rotated as they grow.
    pub logs: u64,
}

/// One of `doctor`'s checks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

/// How a check went: a problem stops the daemon from starting, a warning
/// names what won't work.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Ok,
    Warning,
    Problem,
}

/// A folder of Rovik's, and the free and total bytes of its file system.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub folder: String,
    pub free: u64,
    pub total: u64,
}

/// Rovik's install for the whole server: its administrators, and each
/// limit with its value, `none` when it isn't set (5.5).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstallLimits {
    pub admins: Vec<String>,
    pub limits: Vec<(String, String)>,
}
