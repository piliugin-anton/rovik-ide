//! What Settings → Cloud hosts does (U01). The hosts this computer knows
//! are in `cloud-hosts.json`, with their secrets in the keychain (H01).
//! Each host has its connection test (H04), and its deploy, upgrade and
//! removal (H05). Once connected, its daemon says what it is, which keys it
//! keeps and what its folders take (the Host group). Deployed hosts are
//! connected when the app starts, each through Rovik's SSH or the user's
//! own (H02, H03).
//!
//! The test and the deploy go through Rovik's own SSH, which runs their
//! commands; a host set to "Use my system ssh" is deployed with another
//! way in first.

use crate::client::{HostState, Identity};
use crate::connection_test::{test_connection, Step, TestOptions};
use crate::deploy::{self, Builds, DeployError, Layout, Removal, SetupRun};
use crate::host_settings::{AuthKind, DeployState, Host, HostSettings};
use crate::release::Releases;
use crate::remote::{ApprovalPolicy, Remote};
use crate::secrets::{SecretName, SecretStore, StoreKind};
use crate::ssh::{Answerer, Questions, SshAuth, SshConnector, SshTarget};
use crate::system_ssh::{Local, SystemSsh, SystemSshConnector};
use rovik_protocol::host::{HostInfo, KeyInfo, Storage};
use rovik_protocol::{features, Request, Response, Secret};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Why a host set to the user's own ssh is neither tested nor deployed.
const SYSTEM_SSH: &str = "Testing and deploying go through Rovik's own SSH, which runs their \
                          commands. Log in with a key, your SSH agent or a password to deploy; \
                          \"My system ssh\" connects once Rovik runs there.";

pub struct HostsPage {
    config_dir: PathBuf,
    /// Where the user's own ssh forwards each host's daemon, on Unix (H03).
    sockets: PathBuf,
    secrets: Arc<SecretStore>,
    remote: Arc<Remote>,
    /// The app's version: a daemon older than it can be upgraded.
    version: String,
    /// One change of the settings file at a time.
    file: Mutex<()>,
}

/// The hosts, as the page lists them.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HostsList {
    pub hosts: Vec<HostRow>,
    /// Where secrets are kept, for the page to say (design §6).
    pub store: StoreKind,
    /// What a deploy uploads: the build `ROVIK_AGENTD_BINARY` names, or
    /// this app's release when there's a key to check it with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HostRow {
    #[serde(flatten)]
    pub host: Host,
    /// Its connection, once deployed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<HostState>,
    /// The user's chats there or moving there.
    pub chats: usize,
    /// Whether a password and a key passphrase are saved: never the secrets.
    pub password_saved: bool,
    pub passphrase_saved: bool,
}

/// What the page's form changes of a host's secrets: none leaves one as
/// it is, an empty one forgets it.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct HostSecrets {
    #[serde(default)]
    pub password: Option<Secret>,
    #[serde(default)]
    pub passphrase: Option<Secret>,
}

/// What a connected host's daemon says, for the page.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HostDetails {
    pub info: HostInfo,
    /// The providers whose key it keeps: none from a daemon that can't say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<KeyInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<Storage>,
    /// The user is one of Rovik's administrators there.
    pub admin: bool,
    /// The daemon is older than this app.
    pub upgrade: bool,
    pub app_version: String,
    /// The daemon cleans up when asked ("Clean up now", R01).
    pub clean_up: bool,
}

/// A host as the chats show it: its name, and its connection once it's
/// deployed.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostSummary {
    pub id: String,
    pub name: String,
    pub deployed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<HostState>,
    /// Its mirrored chats pulled since it connected (K23): for a window
    /// that missed `CaughtUp`, as the hosts connect while the app starts.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub caught_up: bool,
}

/// A deploy's, an upgrade's or a removal's failure, as the page takes it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DeployFailure {
    RootLogin {
        message: String,
    },
    Unsupported {
        message: String,
    },
    /// Ask for the user's sudo password, and deploy again with it.
    SudoPassword {
        message: String,
    },
    /// Show `command` for someone with root to run, then deploy again.
    NeedsRoot {
        command: String,
        message: String,
    },
    Tampered {
        message: String,
    },
    AskAdmins {
        admins: Vec<String>,
        message: String,
    },
    Failed {
        message: String,
    },
}

impl From<DeployError> for DeployFailure {
    fn from(e: DeployError) -> DeployFailure {
        let message = e.to_string();
        match e {
            DeployError::RootLogin => DeployFailure::RootLogin { message },
            DeployError::Unsupported(_) => DeployFailure::Unsupported { message },
            DeployError::SudoPassword => DeployFailure::SudoPassword { message },
            DeployError::NeedsRoot { command } => DeployFailure::NeedsRoot { command, message },
            DeployError::Tampered(_) | DeployError::Changed(_) => {
                DeployFailure::Tampered { message }
            }
            DeployError::AskAdmins(admins) => DeployFailure::AskAdmins { admins, message },
            DeployError::Failed(_) => DeployFailure::Failed { message },
        }
    }
}

impl From<String> for DeployFailure {
    fn from(message: String) -> DeployFailure {
        DeployFailure::Failed { message }
    }
}

impl HostsPage {
    /// The page over the settings in `config_dir`; `sockets` is where the
    /// user's own ssh forwards each host's daemon here.
    pub fn new(
        config_dir: PathBuf,
        sockets: PathBuf,
        secrets: Arc<SecretStore>,
        remote: Arc<Remote>,
        version: &str,
    ) -> HostsPage {
        HostsPage {
            config_dir,
            sockets,
            secrets,
            remote,
            version: version.to_string(),
            file: Mutex::new(()),
        }
    }

    fn settings(&self) -> Result<HostSettings, String> {
        HostSettings::load(&self.config_dir)
    }

    /// Changes the settings file, one change at a time.
    fn change<T>(
        &self,
        edit: impl FnOnce(&mut HostSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let _held = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = self.settings()?;
        let done = edit(&mut settings)?;
        settings.save(&self.config_dir)?;
        Ok(done)
    }

    /// Changes the host with id `id`.
    fn change_host(&self, id: &str, edit: impl FnOnce(&mut Host)) -> Result<Host, String> {
        self.change(|settings| {
            let host = settings.hosts.iter_mut().find(|h| h.id == id);
            let host = host.ok_or_else(|| missing(id))?;
            edit(host);
            Ok(host.clone())
        })
    }

    fn host(&self, id: &str) -> Result<Host, String> {
        let settings = self.settings()?;
        settings.host(id).cloned().ok_or_else(|| missing(id))
    }

    /// Connects every deployed host whose token is saved, as the app
    /// starts. On the current Tokio runtime.
    pub fn connect_all(&self) {
        let Ok(settings) = self.settings() else {
            return;
        };
        for host in &settings.hosts {
            if let Err(e) = self.connect(&settings.computer, host) {
                log::warn!("{}: {e}", host.name);
            }
        }
    }

    /// Connects a deployed host in the app, in place of an earlier
    /// connection: through the user's own ssh, or Rovik's, which checks the
    /// install on each new connection (D5).
    fn connect(&self, computer: &str, host: &Host) -> Result<(), String> {
        let DeployState::Deployed { sha256, .. } = &host.deploy else {
            return Ok(());
        };
        let token = self.secret(SecretName::Token {
            host: host.id.clone(),
        });
        let token = token.ok_or("This computer has no token for this host: deploy again.")?;
        let identity = Identity {
            host: host.name.clone(),
            computer: computer.to_string(),
            token,
            client: format!("Rovik {}", self.version),
        };
        self.remote.set_policy(&host.id, host.approval_policy);
        if host.auth == AuthKind::SystemSsh {
            let ssh = SystemSsh {
                destination: format!("{}@{}", host.user, host.host),
                port: (host.port != 22).then_some(host.port),
                program: "ssh".into(),
            };
            let connector = SystemSshConnector::new(ssh, self.local(host)?);
            self.remote.connect(&host.id, connector, identity);
            return Ok(());
        }
        let mut connector = SshConnector::new(self.target(host)?);
        if let Some(sha256) = sha256 {
            connector = connector.with_install_check(vec![sha256.clone()], Some(self.releases()));
        }
        self.remote.connect(&host.id, connector, identity);
        Ok(())
    }

    /// Where the user's own ssh forwards a host's daemon here.
    fn local(&self, host: &Host) -> Result<Local, String> {
        #[cfg(unix)]
        {
            Ok(Local::Socket(
                self.sockets.join(format!("{}.sock", host.id)),
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.sockets, host);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            Ok(Local::Port(port))
        }
    }

    fn secret(&self, name: SecretName) -> Option<Secret> {
        self.secrets.get(&name).ok()
    }

    /// Where and as whom Rovik's SSH logs in, the secrets from the store.
    fn target(&self, host: &Host) -> Result<SshTarget, String> {
        let id = host.id.clone();
        let password = || self.secret(SecretName::Password { host: id.clone() });
        let auth = match host.auth {
            AuthKind::Key => SshAuth::Key {
                path: host
                    .key_path
                    .clone()
                    .ok_or("Choose the key file to log in with.")?,
                passphrase: self.secret(SecretName::KeyPassphrase { host: id.clone() }),
            },
            AuthKind::Agent => SshAuth::Agent { socket: None },
            AuthKind::Password => SshAuth::Password(
                password().ok_or("No SSH password is saved for this host: enter it again.")?,
            ),
            AuthKind::KeyboardInteractive => SshAuth::KeyboardInteractive(answering(password())),
            AuthKind::SystemSsh => return Err(SYSTEM_SSH.into()),
        };
        Ok(SshTarget {
            host: host.host.clone(),
            port: host.port,
            user: host.user.clone(),
            auth,
            host_key: host.host_key.clone(),
        })
    }

    /// The hosts' names and connections, for the chats (U02, U03): no
    /// secret is read for them. After those of the settings come those the
    /// app connects to without settings, the daemons whose sockets the
    /// developer setting `ROVIK_REMOTE_CHATS` names, by the socket's file
    /// name: their chats show their connection, and can move there.
    pub fn summaries(&self) -> Result<Vec<HostSummary>, String> {
        let settings = self.settings()?;
        let states = self.remote.states();
        let state_of = |id: &str| states.iter().find(|(host, _)| host == id).map(|(_, s)| *s);
        let mut summaries: Vec<HostSummary> = settings
            .hosts
            .into_iter()
            .map(|host| HostSummary {
                state: state_of(&host.id),
                deployed: matches!(host.deploy, DeployState::Deployed { .. }),
                caught_up: self.remote.caught_up(&host.id),
                id: host.id,
                name: host.name,
            })
            .collect();
        for (id, state) in &states {
            if summaries.iter().any(|known| &known.id == id) {
                continue;
            }
            let file = std::path::Path::new(id).file_name();
            summaries.push(HostSummary {
                name: file.map_or_else(|| id.clone(), |f| f.to_string_lossy().into_owned()),
                deployed: true,
                state: Some(*state),
                caught_up: self.remote.caught_up(id),
                id: id.clone(),
            });
        }
        Ok(summaries)
    }

    /// The hosts, with their connections, their chats and which secrets
    /// are saved.
    pub async fn list(&self) -> Result<HostsList, String> {
        let settings = self.settings()?;
        let states: HashMap<String, HostState> = self.remote.states().into_iter().collect();
        let mut hosts = Vec::new();
        for host in settings.hosts {
            let id = host.id.clone();
            hosts.push(HostRow {
                state: states.get(&id).copied(),
                chats: self.remote.chats_on(&id).await?,
                password_saved: self
                    .secret(SecretName::Password { host: id.clone() })
                    .is_some(),
                passphrase_saved: self
                    .secret(SecretName::KeyPassphrase { host: id })
                    .is_some(),
                host,
            });
        }
        // What a deploy takes, without reading it: a build of one's own,
        // or this app's release, when there's a key to check it with.
        let build = match std::env::var_os("ROVIK_AGENTD_BINARY") {
            Some(path) => Some(path.to_string_lossy().into_owned()),
            None => self
                .releases()
                .has_key()
                .then(|| format!("Rovik {}'s release", self.version)),
        };
        Ok(HostsList {
            hosts,
            store: self.secrets.kind(),
            build,
        })
    }

    /// Adds a host, or changes one: how it's reached and logged into, and
    /// its secrets. A host at another address loses the key pinned for the
    /// old one; the rest (pinned key, deploy, policy) changes elsewhere.
    pub fn save(&self, mut host: Host, secrets: HostSecrets) -> Result<Host, String> {
        check(&host)?;
        if host.id.is_empty() {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        // The secrets first: a host isn't saved without the password it was
        // given, as when the store is locked.
        let id = host.id.clone();
        for (name, value) in [
            (SecretName::Password { host: id.clone() }, secrets.password),
            (SecretName::KeyPassphrase { host: id }, secrets.passphrase),
        ] {
            let kept = match value {
                None => Ok(()),
                Some(secret) if secret.0.is_empty() => self.secrets.delete(&name),
                Some(secret) => self.secrets.set(&name, &secret),
            };
            kept.map_err(|e| e.to_string())?;
        }
        let (saved, computer) = self.change(|settings| {
            match settings.host(&host.id) {
                Some(old) => {
                    let same_server = old.host == host.host && old.port == host.port;
                    host.host_key = old.host_key.clone().filter(|_| same_server);
                    host.deploy = old.deploy.clone();
                    host.approval_policy = old.approval_policy;
                }
                None => {
                    host.host_key = None;
                    host.deploy = DeployState::NotDeployed;
                }
            }
            settings.put(host.clone());
            Ok((host.clone(), settings.computer.clone()))
        })?;
        self.connect(&computer, &saved)?;
        Ok(saved)
    }

    /// The host goes, its secrets with it: not while chats are there.
    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let chats = self.remote.chats_on(id).await?;
        if chats > 0 {
            return Err(format!(
                "{} on this host: move {} back or delete {} first.",
                plural(chats, "chat is", "chats are"),
                if chats == 1 { "it" } else { "them" },
                if chats == 1 { "it" } else { "them" },
            ));
        }
        self.remote.disconnect(id);
        self.change(|settings| {
            let removed = settings.remove(id, &self.secrets);
            removed.map(|_| ()).map_err(|e| e.to_string())
        })
    }

    /// Tests the host's connection, telling `report` each step (H04).
    pub async fn test(
        &self,
        id: &str,
        report: &(dyn Fn(&Step) + Send + Sync),
    ) -> Result<Vec<Step>, String> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        let known = match &host.deploy {
            DeployState::Deployed {
                sha256: Some(sha256),
                ..
            } => vec![sha256.clone()],
            _ => Vec::new(),
        };
        let options = TestOptions {
            deployed: matches!(host.deploy, DeployState::Deployed { .. }),
            known,
            layout: Layout::default(),
            releases: Some(self.releases()),
        };
        Ok(test_connection(&connector, &options, report).await)
    }

    /// Pins the host's key, once the user compared its fingerprint.
    pub fn trust_key(&self, id: &str, key: &str) -> Result<(), String> {
        let probe = SshTarget {
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuth::Agent { socket: None },
            host_key: Some(key.to_string()),
        };
        probe.pinned_fingerprint().map_err(|e| e.to_string())?;
        self.change_host(id, |host| host.host_key = Some(key.to_string()))?;
        Ok(())
    }

    /// This app's releases, kept in its data folder.
    fn releases(&self) -> Releases {
        Releases::new(self.remote.data_dir())
    }

    /// Trusts the build of Rovik the host runs now, once the user compared
    /// it with what the connection test showed: an administrator's
    /// upgrade, say. The host connects again, checked against it (D5).
    pub fn trust_build(&self, id: &str, sha256: &str) -> Result<(), String> {
        let hex = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            return Err(format!("{sha256} isn't a SHA-256."));
        }
        let host = self.host(id)?;
        if !matches!(host.deploy, DeployState::Deployed { .. }) {
            return Err("Rovik isn't deployed for you on this host.".into());
        }
        let host = self.change_host(id, |host| {
            if let DeployState::Deployed { sha256: kept, .. } = &mut host.deploy {
                *kept = Some(sha256.to_ascii_lowercase());
            }
        })?;
        self.connect(&self.settings()?.computer, &host)
    }

    /// Who approves on the host while nobody is there (6.9).
    pub fn set_policy(&self, id: &str, policy: ApprovalPolicy) -> Result<(), String> {
        self.change_host(id, |host| host.approval_policy = policy)?;
        self.remote.set_policy(id, policy);
        Ok(())
    }

    /// Deploys Rovik for this user (H05), then connects the host. Returns
    /// the daemon's version.
    pub async fn deploy(
        &self,
        id: &str,
        sudo_password: Option<Secret>,
    ) -> Result<String, DeployFailure> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        let builds = Builds::new(self.remote.data_dir(), &self.version)?;
        let computer = self.settings()?.computer;
        let layout = Layout::default();
        let deployed = deploy::deploy(
            &connector,
            &builds,
            &computer,
            sudo_password.as_ref(),
            &layout,
        );
        let deployed = deployed.await?;
        let token = SecretName::Token { host: id.into() };
        let kept = self.secrets.set(&token, &deployed.token);
        kept.map_err(|e| e.to_string())?;
        let daemon = &deployed.daemon.daemon;
        let version = daemon
            .strip_prefix("rovik-agentd ")
            .unwrap_or(daemon)
            .to_string();
        let host = self.change_host(id, |host| {
            host.deploy = DeployState::Deployed {
                version: version.clone(),
                sha256: Some(deployed.sha256.clone()),
            };
        })?;
        self.connect(&computer, &host)?;
        Ok(version)
    }

    /// Upgrades Rovik for everyone on the host, as its administrator: the
    /// daemons start again on the build, which the app knows from then on.
    pub async fn upgrade(&self, id: &str) -> Result<(), DeployFailure> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        let builds = Builds::new(self.remote.data_dir(), &self.version)?;
        let installed = deploy::upgrade(&connector, &builds, &Layout::default()).await?;
        let host = self.change_host(id, |host| {
            if let DeployState::Deployed { sha256, .. } = &mut host.deploy {
                *sha256 = Some(installed);
            }
        })?;
        self.connect(&self.settings()?.computer, &host)?;
        Ok(())
    }

    /// Runs the host's setup script (design §7): after a deploy, and when
    /// the user asks.
    pub async fn run_setup(&self, id: &str) -> Result<SetupRun, DeployFailure> {
        let host = self.host(id)?;
        let script = host.setup_script.as_deref().unwrap_or_default();
        if script.trim().is_empty() {
            return Err("This host has no setup script.".to_string().into());
        }
        let connector = SshConnector::new(self.target(&host)?);
        Ok(deploy::run_setup(&connector, script).await?)
    }

    /// Opens the encrypted file that keeps secrets where there's no
    /// keychain (a new one is made with this passphrase), then connects
    /// the hosts whose tokens it holds.
    pub fn unlock(&self, passphrase: &Secret) -> Result<(), String> {
        self.secrets.unlock(passphrase).map_err(|e| e.to_string())?;
        self.connect_all();
        Ok(())
    }

    /// What removing Rovik for this user deletes on the host.
    pub async fn removal(&self, id: &str) -> Result<Removal, DeployFailure> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        Ok(deploy::removal(&connector).await?)
    }

    /// Removes Rovik for this user, as `removal` listed it: not while
    /// chats are there. The host stays on the page, not deployed.
    pub async fn remove_rovik(&self, id: &str, removal: &Removal) -> Result<(), DeployFailure> {
        let chats = self.remote.chats_on(id).await?;
        if chats > 0 {
            return Err(DeployFailure::Failed {
                message: format!(
                    "{} on this host: move them back or delete them first.",
                    plural(chats, "chat is", "chats are")
                ),
            });
        }
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        deploy::remove(&connector, removal, &Layout::default()).await?;
        self.remote.disconnect(id);
        let token = SecretName::Token { host: id.into() };
        self.secrets.delete(&token).map_err(|e| e.to_string())?;
        self.change_host(id, |host| host.deploy = DeployState::NotDeployed)?;
        Ok(())
    }

    /// What the connected host's daemon says of itself, its keys and its
    /// folders.
    pub async fn details(&self, id: &str) -> Result<HostDetails, String> {
        let info: HostInfo = self.call(id, Request::HostInfo).await?;
        let (keys, storage) = match self.remote.serves(id, features::HOST) {
            true => (
                Some(self.call(id, Request::KeysList).await?),
                Some(self.call(id, Request::Storage).await?),
            ),
            false => (None, None),
        };
        // The version kept follows the daemon's: an upgrade by someone
        // else shows.
        let version = info.version.clone();
        self.change_host(id, |host| {
            if let DeployState::Deployed { version: kept, .. } = &mut host.deploy {
                kept.clone_from(&version);
            }
        })?;
        let admin = info
            .install
            .as_ref()
            .is_some_and(|i| i.admins.contains(&info.user));
        Ok(HostDetails {
            upgrade: older(&info.version, &self.version),
            admin,
            info,
            keys,
            storage,
            app_version: self.version.clone(),
            clean_up: self.remote.serves(id, features::CLEAN_UP),
        })
    }

    /// "Clean up now" (R01): what the host's daemon no longer needs goes,
    /// as each day it does. What Rovik's folders take there after.
    pub async fn clean_up(&self, id: &str) -> Result<Storage, String> {
        self.call(id, Request::CleanUp).await
    }

    /// Forgets every key the host keeps; the next start there that brings
    /// one keeps it again (D11). How many went.
    pub async fn remove_keys(&self, id: &str) -> Result<usize, String> {
        self.call(id, Request::KeysRemove { provider: None }).await
    }

    /// The limits on each user's daemon there, as an administrator sees
    /// them.
    pub async fn limits(&self, id: &str) -> Result<Vec<(String, String)>, DeployFailure> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        Ok(deploy::limits(&connector, &Layout::default()).await?)
    }

    pub async fn set_limits(
        &self,
        id: &str,
        changes: &[(String, String)],
    ) -> Result<(), DeployFailure> {
        let host = self.host(id)?;
        let connector = SshConnector::new(self.target(&host)?);
        Ok(deploy::set_limits(&connector, changes, &Layout::default()).await?)
    }

    async fn call<T>(&self, id: &str, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = rovik_protocol::Fail>,
    {
        let reply = self.remote.host_call(id, request).await;
        reply.and_then(T::try_from).map_err(|fail| fail.message)
    }
}

/// The host's fields the page fills in, before they're saved.
fn check(host: &Host) -> Result<(), String> {
    let blank = |text: &str| text.trim().is_empty();
    if blank(&host.name) {
        return Err("Give the host a name.".into());
    }
    if blank(&host.host) || host.host.contains(char::is_whitespace) {
        return Err("Enter the host's name or address.".into());
    }
    if blank(&host.user) || host.user.contains(char::is_whitespace) {
        return Err("Enter the user to log in as.".into());
    }
    if host.port == 0 {
        return Err("The SSH port is a number from 1 to 65535.".into());
    }
    if host.auth == AuthKind::Key
        && host
            .key_path
            .as_ref()
            .is_none_or(|p| p.as_os_str().is_empty())
    {
        return Err("Choose the key file to log in with.".into());
    }
    Ok(())
}

/// Answers each of the server's questions with the saved password, or
/// gives up without one.
fn answering(password: Option<Secret>) -> Answerer {
    Arc::new(move |questions: Questions| {
        let password = password.clone();
        Box::pin(async move {
            let password = password?;
            Some(questions.prompts.iter().map(|_| password.clone()).collect())
        })
    })
}

/// Whether version `a` comes before version `b`, part by part.
fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map_while(|p| p.parse().ok())
            .collect()
    };
    parts(a) < parts(b)
}

fn missing(id: &str) -> String {
    format!("There's no host {id} here.")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Kdf;
    use rovik_core::chat_db::store::{self, NewDialog};
    use rovik_core::testing::TestCore;

    /// A valid Ed25519 host key, as OpenSSH writes it.
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f";

    struct Setup {
        app: TestCore,
        page: HostsPage,
        remote: Arc<Remote>,
    }

    /// The page over a fresh config folder, its secrets in an unlocked file.
    fn setup() -> Setup {
        let app = TestCore::new();
        let quick = Kdf {
            memory_kib: 256,
            iterations: 1,
            parallelism: 1,
        };
        let secrets = Arc::new(SecretStore::file(app.dir.0.join("secrets.json"), quick));
        secrets.unlock(&Secret("passphrase".into())).unwrap();
        let remote = Remote::new(app.core.clone());
        let page = HostsPage::new(
            app.dir.0.join("config"),
            app.dir.0.join("ssh"),
            secrets,
            remote.clone(),
            "0.2.0",
        );
        Setup { app, page, remote }
    }

    fn new_host() -> Host {
        Host {
            id: String::new(),
            name: "Build box".into(),
            host: "build.example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: AuthKind::Password,
            key_path: None,
            host_key: None,
            approval_policy: ApprovalPolicy::default(),
            deploy: DeployState::NotDeployed,
            setup_script: None,
        }
    }

    fn password(text: &str) -> HostSecrets {
        HostSecrets {
            password: Some(Secret(text.into())),
            passphrase: None,
        }
    }

    #[tokio::test]
    async fn a_daemon_connected_without_settings_is_summed_up_by_its_socket() {
        use crate::testing::{identity, Fake};
        let setup = setup();
        setup.page.save(new_host(), password("hunter2")).unwrap();
        let socket = "/run/user/1000/rovik/agentd.sock";
        setup
            .remote
            .connect(socket, Fake::default(), identity(socket));
        let mut summaries = Vec::new();
        for _ in 0..400 {
            summaries = setup.page.summaries().unwrap();
            if summaries.get(1).is_some_and(|s| s.caught_up) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let shown: Vec<_> = summaries
            .iter()
            .map(|s| (s.name.as_str(), s.deployed, s.state, s.caught_up))
            .collect();
        assert_eq!(
            shown,
            [
                ("Build box", false, None, false),
                ("agentd.sock", true, Some(HostState::Ready), true),
            ]
        );
        assert_eq!(summaries[1].id, socket);
    }

    #[tokio::test]
    async fn a_host_is_saved_with_its_password_in_the_store_alone() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("hunter2")).unwrap();
        assert!(!saved.id.is_empty());
        let list = setup.page.list().await.unwrap();
        assert_eq!(list.hosts.len(), 1);
        assert!(list.hosts[0].password_saved);
        assert!(!list.hosts[0].passphrase_saved);
        assert_eq!(list.hosts[0].state, None, "not deployed, not connected");
        let file = std::fs::read_to_string(setup.app.dir.0.join("config/cloud-hosts.json"));
        assert!(!file.unwrap().contains("hunter2"));
        let json = serde_json::to_string(&list).unwrap();
        assert!(!json.contains("hunter2"), "{json}");

        // Left alone, the password stays; empty, it goes.
        setup
            .page
            .save(saved.clone(), HostSecrets::default())
            .unwrap();
        assert!(setup.page.list().await.unwrap().hosts[0].password_saved);
        setup.page.save(saved, password("")).unwrap();
        assert!(!setup.page.list().await.unwrap().hosts[0].password_saved);
    }

    #[tokio::test]
    async fn the_key_is_pinned_until_the_host_moves() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        assert!(setup.page.trust_key(&saved.id, "not a key").is_err());
        setup.page.trust_key(&saved.id, KEY).unwrap();
        let renamed = Host {
            name: "Renamed".into(),
            host_key: None,
            ..saved.clone()
        };
        let kept = setup.page.save(renamed, HostSecrets::default()).unwrap();
        assert_eq!(kept.host_key.as_deref(), Some(KEY), "same server, same key");
        setup
            .page
            .set_policy(&saved.id, ApprovalPolicy::AiReviewerThenMe)
            .unwrap();
        let moved = Host {
            host: "other.example.com".into(),
            ..kept
        };
        let moved = setup.page.save(moved, HostSecrets::default()).unwrap();
        assert_eq!(moved.host_key, None, "another server shows its own key");
        assert_eq!(moved.approval_policy, ApprovalPolicy::AiReviewerThenMe);
    }

    #[tokio::test]
    async fn a_host_with_chats_isnt_removed() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        let dialog = NewDialog {
            id: "chat-1".into(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: None,
        };
        let db = &setup.app.core.db;
        db.write(move |c| store::create_dialog(c, &dialog))
            .await
            .unwrap();
        setup.remote.place("chat-1", &saved.id);
        db.write(|_| Ok::<(), String>(())).await.unwrap();
        let refused = setup.page.remove(&saved.id).await.unwrap_err();
        assert!(refused.starts_with("1 chat is on this host"), "{refused}");
        assert_eq!(setup.page.list().await.unwrap().hosts[0].chats, 1);
        let gone = setup.page.save(new_host(), password("pw")).unwrap();
        setup.page.remove(&gone.id).await.unwrap();
        assert_eq!(setup.page.list().await.unwrap().hosts.len(), 1);
    }

    #[tokio::test]
    async fn a_locked_file_opens_with_its_passphrase() {
        let app = TestCore::new();
        let quick = Kdf {
            memory_kib: 256,
            iterations: 1,
            parallelism: 1,
        };
        let path = app.dir.0.join("secrets.json");
        let earlier = SecretStore::file(path.clone(), quick);
        earlier.unlock(&Secret("passphrase".into())).unwrap();
        let secrets = Arc::new(SecretStore::file(path, quick));
        let page = HostsPage::new(
            app.dir.0.join("config"),
            app.dir.0.join("ssh"),
            secrets,
            Remote::new(app.core.clone()),
            "0.2.0",
        );
        let list = page.list().await.unwrap();
        assert_eq!(list.store, StoreKind::File { unlocked: false });
        assert!(page.save(new_host(), password("pw")).is_err());
        assert!(
            page.list().await.unwrap().hosts.is_empty(),
            "not saved without its password"
        );
        assert!(page.unlock(&Secret("not it".into())).is_err());
        page.unlock(&Secret("passphrase".into())).unwrap();
        let list = page.list().await.unwrap();
        assert_eq!(list.store, StoreKind::File { unlocked: true });
        page.save(new_host(), password("pw")).unwrap();
        assert!(page.list().await.unwrap().hosts[0].password_saved);
    }

    #[test]
    fn versions_compare_part_by_part() {
        assert!(older("0.1.0", "0.2.0"));
        assert!(older("0.9.9", "0.10.0"));
        assert!(!older("0.2.0", "0.2.0"));
        assert!(!older("1.0.0", "0.9.0"));
        assert!(older("0.1.0", "0.1.1-beta"));
    }

    #[test]
    fn a_host_needs_its_name_address_user_and_key() {
        let mut host = Host {
            id: String::new(),
            name: "Build box".into(),
            host: "build.example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: AuthKind::Key,
            key_path: Some("/home/ann/.ssh/id_ed25519".into()),
            host_key: None,
            approval_policy: ApprovalPolicy::default(),
            deploy: DeployState::NotDeployed,
            setup_script: None,
        };
        assert_eq!(check(&host), Ok(()));
        host.key_path = None;
        assert!(check(&host).unwrap_err().contains("key file"));
        host.auth = AuthKind::Agent;
        assert_eq!(check(&host), Ok(()));
        host.host = "two words".into();
        assert!(check(&host).is_err());
        host.host = "build.example.com".into();
        host.port = 0;
        assert!(check(&host).is_err());
    }

    /// D5: a build the user compared is the one each connection checks
    /// from then on, on a deployed host alone.
    #[tokio::test]
    async fn a_build_the_user_trusts_is_kept_for_the_host() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        let sha256 = "AB".repeat(32);
        let undeployed = setup.page.trust_build(&saved.id, &sha256);
        assert!(undeployed.unwrap_err().contains("isn't deployed"));
        setup
            .page
            .change_host(&saved.id, |host| {
                host.deploy = DeployState::Deployed {
                    version: "0.1.0".into(),
                    sha256: Some("00".repeat(32)),
                }
            })
            .unwrap();
        let token = SecretName::Token {
            host: saved.id.clone(),
        };
        setup
            .page
            .secrets
            .set(&token, &Secret("token".into()))
            .unwrap();
        assert!(setup.page.trust_build(&saved.id, "abc").is_err());
        setup.page.trust_build(&saved.id, &sha256).unwrap();
        let host = setup.page.host(&saved.id).unwrap();
        let DeployState::Deployed { sha256: kept, .. } = host.deploy else {
            panic!("{:?}", host.deploy);
        };
        assert_eq!(kept, Some("ab".repeat(32)));
    }

    #[test]
    fn deploy_failures_go_to_the_page_by_kind() {
        let root = DeployFailure::from(DeployError::NeedsRoot {
            command: "'/home/ann/.rovik-agentd-upload' install --admin 'ann'".into(),
        });
        let json = serde_json::to_value(&root).unwrap();
        assert_eq!(json["kind"], "needsRoot");
        assert_eq!(
            json["command"],
            "'/home/ann/.rovik-agentd-upload' install --admin 'ann'"
        );
        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("someone with root"));
        let admins = DeployFailure::from(DeployError::AskAdmins(vec!["bo".into()]));
        assert_eq!(serde_json::to_value(&admins).unwrap()["admins"][0], "bo");
    }
}
