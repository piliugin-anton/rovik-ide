//! The hosts this computer knows (H01): `cloud-hosts.json` in the app's
//! config folder, with this computer's id (D5), and for each host how to
//! reach it, the host key pinned on first use, its approval policy (6.9)
//! and what was deployed there. No secret goes in it: a host's SSH
//! password, its key's passphrase and this computer's token for it are in
//! the secret store (`secrets`), under the host's id.

use crate::remote::ApprovalPolicy;
use crate::secrets::{SecretError, SecretName, SecretStore};
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
            host: "build.example.com".into(),
            port: 2222,
            user: "me".into(),
            auth: AuthKind::Key,
            key_path: Some(PathBuf::from("/home/me/.ssh/id_ed25519")),
            host_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtestkey".into()),
            approval_policy: ApprovalPolicy::AiReviewerThenMe,
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
