//! The hosts this computer knows (H01): `cloud-hosts.json` in the app's
//! config folder, with this computer's id (D5), and for each host how to
//! reach it, the host key pinned on first use, its approval policy (6.9)
//! and what was deployed there. No secret goes in it: a host's SSH
//! password, its key's passphrase and this computer's token for it are in
//! the secret store (`secrets`), under the host's id.

use crate::remote::ApprovalPolicy;
use crate::secrets::{SecretError, SecretName, SecretStore};
use rovik_protocol::autopilot::AutonomySettings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file, in the app's config folder.
pub const FILE: &str = "cloud-hosts.json";

/// Its format.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostSettings {
    pub version: u32,
    /// This computer's id, which every daemon knows it by (D5).
    pub computer: String,
    #[serde(default)]
    pub hosts: Vec<Host>,
}

/// A host, as the hosts page sets it up.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// Its id: where its secrets are, and what its chats' location names.
    pub id: String,
    /// What the user calls it.
    pub name: String,
    /// How it's reached: SSH to a server, or `wsl.exe` to a WSL
    /// distribution on this computer, for which `host`, `port`, `user`
    /// and `auth` don't apply.
    #[serde(default, skip_serializing_if = "Transport::is_ssh")]
    pub transport: Transport,
    /// Its DNS name or address, and SSH port.
    pub host: String,
    #[serde(default = "ssh_port")]
    pub port: u16,
    pub user: String,
    pub auth: AuthKind,
    /// The key file, for `AuthKind::Key`: its passphrase, if it has one,
    /// is in the secret store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<PathBuf>,
    /// The host key pinned on first use, as OpenSSH writes it: a
    /// different key is refused (H02).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key: Option<String>,
    #[serde(default)]
    pub approval_policy: ApprovalPolicy,
    /// What its tasks follow while it works on its own (`Autonomous`):
    /// kept when another policy is chosen, for when it's chosen again.
    #[serde(default, skip_serializing_if = "is_default")]
    pub autonomy: AutonomySettings,
    #[serde(default)]
    pub deploy: DeployState,
    /// Commands run on the host after a deploy, and again on request, to
    /// install what the projects' checks need (design §7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_script: Option<String>,
}

/// How the app logs in to a host (H02, H03).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// A private key file, with its passphrase in the secret store.
    Key,
    /// The keys the SSH agent holds.
    Agent,
    /// A password, in the secret store.
    Password,
    /// The server's own questions, answered as they come.
    KeyboardInteractive,
    /// The user's own `ssh`, with their configuration (H03).
    SystemSsh,
}

/// How the app reaches a host.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Transport {
    /// A server, over SSH, as `Host::host`, `port`, `user` and `auth` say.
    #[default]
    Ssh,
    /// A WSL2 distribution on this computer (Windows), as `user` there:
    /// the distribution's default user when none is named.
    Wsl {
        distro: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
    },
}

impl Transport {
    pub fn is_ssh(&self) -> bool {
        *self == Transport::Ssh
    }
}

impl Host {
    /// The distribution, for a host reached through WSL.
    pub fn wsl(&self) -> Option<crate::wsl::Wsl> {
        match &self.transport {
            Transport::Wsl { distro, user } => Some(crate::wsl::Wsl::new(distro, user.clone())),
            Transport::Ssh => None,
        }
    }
}

/// What was deployed on a host for this user (H05).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DeployState {
    #[default]
    NotDeployed,
    /// Rovik runs there for this user, at this version; the build's
    /// SHA-256, which the check on each connection knows (H05).
    Deployed {
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
}

fn ssh_port() -> u16 {
    22
}

fn is_default(autonomy: &AutonomySettings) -> bool {
    *autonomy == AutonomySettings::default()
}

impl HostSettings {
    /// The hosts this computer knows, as `config_dir` keeps them. The first
    /// time there are none, and this computer gets its id, saved at once so
    /// it never changes.
    pub fn load(config_dir: &Path) -> Result<HostSettings, String> {
        let path = config_dir.join(FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let settings: HostSettings = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                if settings.version > VERSION {
                    return Err(format!(
                        "{} was written by a newer Rovik: update the app.",
                        path.display()
                    ));
                }
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let settings = HostSettings {
                    version: VERSION,
                    computer: uuid::Uuid::new_v4().to_string(),
                    hosts: Vec::new(),
                };
                settings.save(config_dir)?;
                Ok(settings)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, config_dir: &Path) -> Result<(), String> {
        let path = config_dir.join(FILE);
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
        rovik_core::atomic_file::write_atomic_private(&path, &bytes)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn host(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.id == id)
    }

    /// The host in place of the one with its id, or added.
    pub fn put(&mut self, host: Host) {
        match self.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(slot) => *slot = host,
            None => self.hosts.push(host),
        }
    }

    /// The host goes, and its secrets with it.
    pub fn remove(&mut self, id: &str, secrets: &SecretStore) -> Result<Option<Host>, SecretError> {
        forget_secrets(secrets, id)?;
        let at = self.hosts.iter().position(|host| host.id == id);
        Ok(at.map(|at| self.hosts.remove(at)))
    }
}

/// The secrets a host had: its password, its key's passphrase, and this
/// computer's token for it.
pub fn forget_secrets(secrets: &SecretStore, id: &str) -> Result<(), SecretError> {
    let host = id.to_string();
    for name in [
        SecretName::Password { host: host.clone() },
        SecretName::KeyPassphrase { host: host.clone() },
        SecretName::Token { host },
    ] {
        secrets.delete(&name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Kdf;
    use rovik_protocol::Secret;

    const QUICK: Kdf = Kdf {
        memory_kib: 256,
        iterations: 1,
        parallelism: 1,
    };

    fn host(id: &str) -> Host {
        Host {
            id: id.into(),
            name: "Build box".into(),
            transport: Transport::Ssh,
            host: "build.example.com".into(),
            port: 2222,
            user: "me".into(),
            auth: AuthKind::Key,
            key_path: Some(PathBuf::from("/home/me/.ssh/id_ed25519")),
            host_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtestkey".into()),
            approval_policy: ApprovalPolicy::AiReviewerThenMe,
            autonomy: AutonomySettings::default(),
            deploy: DeployState::Deployed {
                version: "0.1.0".into(),
                sha256: Some("ab".repeat(32)),
            },
            setup_script: Some("rustup default stable\n".into()),
        }
    }

    #[test]
    fn no_secret_ever_lands_in_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = HostSettings::load(dir.path()).unwrap();
        settings.put(host("h1"));
        settings.save(dir.path()).unwrap();
        let secrets = SecretStore::file(dir.path().join("secrets.json"), QUICK);
        secrets.unlock(&Secret("unlock-phrase".into())).unwrap();
        let values = [
            (SecretName::Password { host: "h1".into() }, "pw-7f3a9"),
            (SecretName::KeyPassphrase { host: "h1".into() }, "pp-2b8c1"),
            (SecretName::Token { host: "h1".into() }, "tok-9d4e6"),
        ];
        for (name, value) in &values {
            secrets.set(name, &Secret(value.to_string())).unwrap();
        }
        settings.save(dir.path()).unwrap();

        for file in [FILE, "secrets.json"] {
            let written = std::fs::read_to_string(dir.path().join(file)).unwrap();
            for (_, value) in &values {
                assert!(!written.contains(value), "{value} is in {file}");
            }
            assert!(
                !written.contains("unlock-phrase"),
                "the passphrase in {file}"
            );
        }
        // A host removed takes its secrets with it.
        settings.remove("h1", &secrets).unwrap();
        for (name, _) in &values {
            assert_eq!(secrets.get(name), Err(SecretError::Missing(name.clone())));
        }
    }

    #[test]
    fn the_settings_round_trip_and_this_computer_keeps_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let first = HostSettings::load(dir.path()).unwrap();
        assert!(first.hosts.is_empty());
        let mut settings = HostSettings::load(dir.path()).unwrap();
        assert_eq!(settings.computer, first.computer, "saved at once");
        settings.put(host("h1"));
        let mut renamed = host("h1");
        renamed.name = "Renamed".into();
        settings.put(renamed.clone());
        settings.save(dir.path()).unwrap();

        let read = HostSettings::load(dir.path()).unwrap();
        assert_eq!(read.hosts, [renamed]);
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        for field in [
            "\"keyPath\"",
            "\"hostKey\"",
            "\"approvalPolicy\": \"aiReviewerThenMe\"",
            "\"auth\": \"key\"",
            "\"state\": \"deployed\"",
        ] {
            assert!(text.contains(field), "{field} in {text}");
        }
    }

    /// H01: a host that works on its own keeps its block; a file from
    /// before it loads with the block's defaults.
    #[test]
    fn a_host_that_works_on_its_own_round_trips_and_an_old_file_loads() {
        use rovik_protocol::autopilot::{Delivery, Effort};
        let dir = tempfile::tempdir().unwrap();
        let mut settings = HostSettings::load(dir.path()).unwrap();
        let mut own = host("h1");
        own.approval_policy = ApprovalPolicy::Autonomous;
        own.autonomy = AutonomySettings {
            effort: Some(Effort::Thorough),
            delivery: Delivery::PushBranch {
                deploy_key: "~/.ssh/rovik_deploy".into(),
                open_pr: true,
            },
            notify_url: Some("https://ntfy.sh/topic".into()),
            ..Default::default()
        };
        settings.put(own.clone());
        settings.save(dir.path()).unwrap();
        let read = HostSettings::load(dir.path()).unwrap();
        assert_eq!(read.hosts, [own]);
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            text.contains("\"approvalPolicy\": \"autonomous\""),
            "{text}"
        );
        assert!(text.contains("\"kind\": \"pushBranch\""), "{text}");

        // Written before the block existed.
        std::fs::write(
            dir.path().join(FILE),
            r#"{"version":1,"computer":"c","hosts":[{"id":"h2","name":"Old","host":"old.example.com","user":"me","auth":"agent","approvalPolicy":"aiReviewerThenMe"}]}"#,
        )
        .unwrap();
        let old = HostSettings::load(dir.path()).unwrap();
        assert_eq!(old.hosts[0].autonomy, AutonomySettings::default());
        assert_eq!(
            old.hosts[0].approval_policy,
            ApprovalPolicy::AiReviewerThenMe
        );
        // Saved again, the default block isn't written.
        old.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!text.contains("autonomy"), "{text}");
    }

    #[test]
    fn a_file_from_a_newer_app_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            r#"{"version":2,"computer":"c","hosts":[]}"#,
        )
        .unwrap();
        let refused = HostSettings::load(dir.path()).unwrap_err();
        assert!(refused.contains("newer Rovik"), "{refused}");
    }
}
