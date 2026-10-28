//! What Settings → Cloud hosts does (U01). The hosts this computer knows
//! are in `cloud-hosts.json`, with their secrets in the keychain (H01).
//! Each host has its connection test (H04), and its deploy, upgrade and
//! removal (H05). Once connected, its daemon says what it is, which keys it
//! keeps and what its folders take (the Host group). Deployed hosts are
//! connected when the app starts, each through Rovik's SSH or the user's
//! own (H02, H03), which the terminal panel's remote terminals take too.
//!
//! The test and the deploy go through Rovik's own SSH, which runs their
//! commands; a host set to "Use my system ssh" is deployed with another
//! way in first.

use crate::client::{HostState, Identity};
use crate::connection_test::{test_connection, Outcome, Step, TestOptions, TestTarget};
use crate::deploy::{self, Builds, DeployError, Layout, Removal, SetupRun};
use crate::host_settings::{AuthKind, DeployState, Host, HostSettings, Transport};
use crate::host_shell::{HostShell, ProcessConnector, ProcessShell};
use crate::release::Releases;
use crate::remote::{ApprovalPolicy, Remote};
use crate::secrets::{SecretName, SecretStore, StoreKind};
use crate::ssh::{Answerer, Questions, Ssh, SshAuth, SshConnector, SshTarget};
use crate::system_ssh::{Local, SystemSsh, SystemSshConnector};
use crate::wsl::{self, Wsl};
use rovik_protocol::autopilot::{AutonomySettings, ChatTask, DeliveryTest, Effort};
use rovik_protocol::host::{HostInfo, KeyInfo, Storage};
use rovik_protocol::{features, Request, Response, Secret};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

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
    /// Why a deployed host couldn't be connected, by its id: no token
    /// saved here, the store locked. Shown in place of "Connecting…",
    /// which it would otherwise say for good.
    unconnected: Mutex<HashMap<String, String>>,
    /// What this window made of the settings file, which another of
    /// Rovik's windows may change (`follow`).
    followed: Mutex<Followed>,
}

/// What a window made of the hosts' settings file: when it read it for
/// the policies and for the connections, and what each of its connections
/// was made of (the host's settings but its policies, and its token).
#[derive(Default)]
struct Followed {
    policies: Option<(SystemTime, u64)>,
    connections: Option<(SystemTime, u64)>,
    connected: HashMap<String, (String, Option<String>)>,
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
    /// Why the app couldn't connect to it, when it couldn't: no token for
    /// it is saved here, say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The user's chats there or moving there.
    pub chats: usize,
    /// Whether a password and a key passphrase are saved: never the secrets.
    pub password_saved: bool,
    pub passphrase_saved: bool,
    /// The additive requests its daemon serves, once it has said (4.5): the
    /// MCP servers page offers a host that serves `mcp`.
    pub serves: Vec<String>,
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

/// How long "Clean up now" may take: gigabytes of worktrees and temporary
/// files to delete, and a full `git gc` of each project's repository.
const CLEAN_UP_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What "Clean up now" did (R01).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CleanedUp {
    /// What Rovik's folders take on the host after.
    pub storage: Storage,
    /// How many bytes less they take than just before.
    pub freed: u64,
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
    /// A terminal can open on it (`HostsPage::shell_access`): a host of
    /// the settings, which SSH reaches, not a daemon that the developer
    /// setting names by its socket.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    /// The WSL distribution on this computer (`WSL_HOST`): its chats are
    /// this computer's, not a cloud's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wsl: bool,
    /// It works on its own (H02): its chats start in Autopilot.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub autonomous: bool,
    /// The effort its tasks take while it works on its own, when it sets
    /// one (H01); else the computer's. The chat checks what a task needs
    /// before it sends one (T09).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

/// How a terminal reaches a host's shell (`HostsPage::shell_access`).
pub enum ShellAccess {
    /// Rovik's own SSH, logged in as the host's settings say.
    Ssh(SshTarget),
    /// The user's own ssh, run in a terminal here (H03).
    System(SystemSsh),
    /// `wsl.exe`, run in a terminal here: the user's shell in the
    /// distribution.
    Wsl(Wsl),
}

/// How the page reaches a host for its test, deploy, upgrade and removal:
/// through Rovik's SSH, or through `wsl.exe` for a WSL distribution here.
enum HostLink {
    Ssh(SshConnector),
    Wsl {
        wsl: Wsl,
        shell: ProcessShell,
        root: ProcessShell,
        connector: ProcessConnector,
    },
}

impl HostLink {
    /// The SSH session, made now; none for WSL, whose shell needs none.
    async fn session(&self) -> Result<Option<Arc<Ssh>>, String> {
        match self {
            HostLink::Ssh(connector) => connector
                .session()
                .await
                .map(Some)
                .map_err(|e| e.to_string()),
            HostLink::Wsl { .. } => Ok(None),
        }
    }

    /// Where a deploy, an upgrade or a removal acts, `session` being the
    /// SSH one.
    fn target<'a>(
        &'a self,
        session: Option<&'a Ssh>,
        name: &'a str,
    ) -> Result<deploy::Target<'a>, String> {
        match self {
            HostLink::Ssh(connector) => Ok(deploy::Target {
                shell: session.ok_or("The SSH session wasn't made.")?,
                connector,
                root: None,
                name,
                browser: true,
            }),
            HostLink::Wsl {
                shell,
                root,
                connector,
                ..
            } => Ok(deploy::Target {
                shell,
                connector,
                root: Some(root),
                name,
                browser: false,
            }),
        }
    }

    fn test_target(&self) -> TestTarget<'_> {
        match self {
            HostLink::Ssh(connector) => TestTarget::Ssh(connector),
            HostLink::Wsl {
                wsl,
                shell,
                connector,
                ..
            } => TestTarget::Wsl {
                wsl,
                shell,
                connector,
            },
        }
    }
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
            unconnected: Mutex::new(HashMap::new()),
            followed: Mutex::new(Followed::default()),
        }
    }

    fn followed(&self) -> MutexGuard<'_, Followed> {
        self.followed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// When the settings file was written last, and its length.
    fn modified(&self) -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(self.config_dir.join(crate::host_settings::FILE)).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    /// The hosts' policies as the settings file has them, when it changed
    /// since it was read for them: another window may have set one, and
    /// this window would send its own old one to the host, which keeps one
    /// per computer (Remote asks before each configuration it sends).
    pub fn follow_policies(&self) {
        let at = self.modified();
        {
            let mut followed = self.followed();
            if followed.policies == at {
                return;
            }
            followed.policies = at;
        }
        let Ok(settings) = self.settings() else {
            return;
        };
        for host in &settings.hosts {
            self.remote.set_policy(&host.id, host.approval_policy);
            self.remote.set_autonomy(&host.id, host.autonomy.clone());
        }
    }

    /// This window's connections as the settings file has the hosts, when
    /// it changed since: another window may have added, deployed, changed
    /// or removed one. A host whose connection was made of other settings
    /// (its address, its key, a new deploy's token) connects again; one
    /// that's gone is let go of. The app looks every few seconds.
    pub fn follow(&self) {
        let at = self.modified();
        {
            let mut followed = self.followed();
            if followed.connections == at {
                return;
            }
            followed.connections = at;
        }
        self.follow_policies();
        let Ok(settings) = self.settings() else {
            return;
        };
        for host in &settings.hosts {
            let made = self.followed().connected.get(&host.id).cloned();
            if made.as_ref() != Some(&self.made_of(host)) {
                if let Err(e) = self.connect(&settings.computer, host) {
                    log::warn!("{}: {e}", host.name);
                }
            }
        }
        let gone: Vec<String> = (self.followed().connected.keys())
            .filter(|id| settings.host(id).is_none())
            .cloned()
            .collect();
        for id in gone {
            self.remote.disconnect(&id);
            self.followed().connected.remove(&id);
        }
    }

    /// What a host's connection is made of: its settings but those that go
    /// by `follow_policies` (and what only a deploy runs), and its token's
    /// digest (a deploy makes a new one; the token itself isn't kept).
    fn made_of(&self, host: &Host) -> (String, Option<String>) {
        let mut made = host.clone();
        made.approval_policy = ApprovalPolicy::default();
        made.autonomy = AutonomySettings::default();
        made.setup_script = None;
        let token = matches!(host.deploy, DeployState::Deployed { .. })
            .then(|| {
                self.secret(SecretName::Token {
                    host: host.id.clone(),
                })
            })
            .flatten()
            .map(|token| rovik_core::file_guard::sha256_hex(token.0.as_bytes()));
        (serde_json::to_string(&made).unwrap_or_default(), token)
    }

    /// Why each host that couldn't be connected couldn't, by its id.
    fn unconnected(&self) -> HashMap<String, String> {
        self.unconnected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
        // And one across Rovik's windows: another's change isn't written over.
        let lock = self.config_dir.join("cloud-hosts.lock");
        let _windows = rovik_core::os_lock::hold(&lock)
            .map_err(|e| log::warn!("{}: {e}", lock.display()))
            .ok();
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
        {
            let at = self.modified();
            let mut followed = self.followed();
            followed.policies = at;
            followed.connections = at;
        }
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
        let made = self.made_of(host);
        self.followed().connected.insert(host.id.clone(), made);
        let connected = self.connect_in(computer, host);
        let mut unconnected = self.unconnected.lock().unwrap_or_else(|e| e.into_inner());
        match &connected {
            Ok(()) => {
                unconnected.remove(&host.id);
            }
            Err(why) => {
                unconnected.insert(host.id.clone(), why.clone());
            }
        }
        connected
    }

    fn connect_in(&self, computer: &str, host: &Host) -> Result<(), String> {
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
        self.remote.set_autonomy(&host.id, host.autonomy.clone());
        if let Some(wsl) = host.wsl() {
            let connector = self.wsl_connector(host, &wsl);
            self.remote.connect(&host.id, connector, identity);
            // This computer's chats run there (Remote::start).
            self.remote.set_home(Some(&host.id));
            return Ok(());
        }
        if host.auth == AuthKind::SystemSsh {
            let connector = SystemSshConnector::new(system_ssh(host), self.local(host)?);
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

    /// Where the user's own ssh forwards a host's daemon here: each of
    /// Rovik's processes its own socket, as ssh takes the path over.
    fn local(&self, host: &Host) -> Result<Local, String> {
        #[cfg(unix)]
        {
            Ok(Local::Socket(self.sockets.join(format!(
                "{}.sock",
                rovik_core::os_lock::slotted(&host.id)
            ))))
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.sockets, host);
            // A free port each time the forward starts (`Local::Port(0)`):
            // one picked here could be taken by then.
            Ok(Local::Port(0))
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

    /// How a terminal reaches the host's shell: `wsl.exe` for a WSL
    /// distribution, the user's own ssh for a host set to it, Rovik's SSH
    /// with the host's way in otherwise.
    pub fn shell_access(&self, id: &str) -> Result<ShellAccess, String> {
        let host = self.host(id)?;
        if let Some(wsl) = host.wsl() {
            return Ok(ShellAccess::Wsl(wsl));
        }
        match host.auth {
            AuthKind::SystemSsh => Ok(ShellAccess::System(system_ssh(&host))),
            _ => Ok(ShellAccess::Ssh(self.target(&host)?)),
        }
    }

    /// How the page reaches `host` for its test, deploy, upgrade and
    /// removal.
    fn link(&self, host: &Host) -> Result<HostLink, String> {
        match host.wsl() {
            Some(wsl) => Ok(HostLink::Wsl {
                shell: wsl.shell(),
                root: wsl.root_shell(),
                connector: self.wsl_connector(host, &wsl),
                wsl,
            }),
            None => Ok(HostLink::Ssh(SshConnector::new(self.target(host)?))),
        }
    }

    /// The daemon in a WSL distribution, checked against the build this
    /// computer trusts once it's deployed (D5), as SSH's connector is.
    fn wsl_connector(&self, host: &Host, wsl: &Wsl) -> ProcessConnector {
        let mut connector = wsl.connector(&Layout::default().installed);
        if let DeployState::Deployed {
            sha256: Some(sha256),
            ..
        } = &host.deploy
        {
            let user = wsl.user.clone().unwrap_or_default();
            connector =
                connector.with_install_check(vec![sha256.clone()], Some(self.releases()), user);
        }
        connector
    }

    /// The hosts' names and connections, for the chats (U02, U03): no
    /// secret is read for them. After those of the settings come those the
    /// app connects to without settings, the daemons whose sockets the
    /// developer setting `ROVIK_REMOTE_CHATS` names, by the socket's file
    /// name: their chats show their connection, and can move there.
    pub fn summaries(&self) -> Result<Vec<HostSummary>, String> {
        let settings = self.settings()?;
        let states = self.remote.states();
        let unconnected = self.unconnected();
        // A deployed host the app couldn't connect to is as good as offline
        // to its chats, not forever connecting.
        let state_of = |id: &str| {
            let known = states.iter().find(|(host, _)| host == id).map(|(_, s)| *s);
            known.or_else(|| unconnected.contains_key(id).then_some(HostState::Offline))
        };
        let mut summaries: Vec<HostSummary> = settings
            .hosts
            .into_iter()
            .map(|host| {
                let autonomous = host.approval_policy == ApprovalPolicy::Autonomous;
                HostSummary {
                    state: state_of(&host.id),
                    deployed: matches!(host.deploy, DeployState::Deployed { .. }),
                    caught_up: self.remote.caught_up(&host.id),
                    terminal: true,
                    wsl: host.wsl().is_some(),
                    autonomous,
                    effort: host.autonomy.effort.filter(|_| autonomous),
                    id: host.id,
                    name: host.name,
                }
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
                terminal: false,
                wsl: false,
                autonomous: false,
                effort: None,
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
        let unconnected = self.unconnected();
        let mut hosts = Vec::new();
        // The WSL distribution isn't a cloud host: its own page shows it.
        for host in settings.hosts.into_iter().filter(|h| h.wsl().is_none()) {
            let id = host.id.clone();
            let problem = unconnected
                .get(&id)
                .cloned()
                .or_else(|| self.remote.refusal(&id));
            hosts.push(HostRow {
                state: states
                    .get(&id)
                    .copied()
                    .or(problem.as_ref().map(|_| HostState::Offline)),
                problem,
                chats: self.remote.chats_on(&id).await?,
                password_saved: self
                    .secret(SecretName::Password { host: id.clone() })
                    .is_some(),
                passphrase_saved: self
                    .secret(SecretName::KeyPassphrase { host: id.clone() })
                    .is_some(),
                serves: self.remote.features(&id),
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
        let new_host = host.id.is_empty();
        if new_host {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        // The secrets first: a host isn't saved without the password it was
        // given, as when the store is locked. The secret its way of logging
        // in doesn't use goes: a password left from before its key.
        let id = host.id.clone();
        let gone = || Some(Secret(String::new()));
        let (password, passphrase) = match (host.wsl().is_some(), host.auth) {
            (false, AuthKind::Key) => (gone(), secrets.passphrase),
            (false, AuthKind::Password | AuthKind::KeyboardInteractive) => {
                (secrets.password, gone())
            }
            (true, _) | (false, AuthKind::Agent | AuthKind::SystemSsh) => (gone(), gone()),
        };
        for (name, value) in [
            (SecretName::Password { host: id.clone() }, password),
            (SecretName::KeyPassphrase { host: id }, passphrase),
        ] {
            let kept = match value {
                None => Ok(()),
                Some(secret) if secret.0.is_empty() => self.secrets.delete(&name),
                Some(secret) => self.secrets.set(&name, &secret),
            };
            kept.map_err(|e| e.to_string())?;
        }
        let changed = self.change(|settings| {
            match settings.host(&host.id) {
                Some(old) => {
                    let same_server = old.host == host.host
                        && old.port == host.port
                        && old.transport == host.transport;
                    host.host_key = old.host_key.clone().filter(|_| same_server);
                    host.deploy = old.deploy.clone();
                    host.approval_policy = old.approval_policy;
                    // Changed through `set_autonomy` only (H01).
                    host.autonomy = old.autonomy.clone();
                }
                None => {
                    host.host_key = None;
                    host.deploy = DeployState::NotDeployed;
                    host.autonomy = AutonomySettings::default();
                }
            }
            settings.put(host.clone());
            Ok((host.clone(), settings.computer.clone()))
        });
        let (saved, computer) = match changed {
            Ok(saved) => saved,
            Err(e) => {
                // A host that couldn't be saved keeps no secrets.
                if new_host {
                    for name in [
                        SecretName::Password {
                            host: host.id.clone(),
                        },
                        SecretName::KeyPassphrase {
                            host: host.id.clone(),
                        },
                    ] {
                        let _ = self.secrets.delete(&name);
                    }
                }
                return Err(e);
            }
        };
        // Saved either way: a connection that can't be made (no token here
        // yet) is the host's state on the page, not the form's failure.
        if let Err(e) = self.connect(&computer, &saved) {
            log::warn!("{}: {e}", saved.name);
        }
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
        self.followed().connected.remove(id);
        self.unconnected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
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
        let link = self.link(&host)?;
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
        let steps = test_connection(link.test_target(), &options, report).await;
        // A host that turned the app away is tried again on its own only
        // now and then, or not at all (`HostState::Refused`): once the test
        // gets through, as after a fix on the server, the app connects
        // again at once.
        let through = steps
            .iter()
            .all(|step| !matches!(step.outcome, Outcome::Failed | Outcome::Confirm));
        let refused = self.remote.refusal(id).is_some();
        if through && refused && options.deployed {
            if let Err(e) = self.connect(&self.settings()?.computer, &host) {
                log::warn!("{}: {e}", host.name);
            }
        }
        Ok(steps)
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

    /// Who approves on the host while nobody is there (6.9); a connected
    /// host is told what follows at once (H02).
    pub async fn set_policy(&self, id: &str, policy: ApprovalPolicy) -> Result<(), String> {
        self.change_host(id, |host| host.approval_policy = policy)?;
        self.remote.set_policy(id, policy);
        self.remote.reconfigure(id).await;
        Ok(())
    }

    /// What the host's tasks follow while it works on its own (H01),
    /// checked first (`AutonomySettings::checked`); a connected host is
    /// told at once (H02). Returns them as kept.
    pub async fn set_autonomy(
        &self,
        id: &str,
        autonomy: AutonomySettings,
    ) -> Result<AutonomySettings, String> {
        let autonomy = autonomy.checked()?;
        let kept = autonomy.clone();
        self.change_host(id, |host| host.autonomy = kept)?;
        self.remote.set_autonomy(id, autonomy.clone());
        self.remote.reconfigure(id).await;
        Ok(autonomy)
    }

    /// This computer's chats on the host, each with its latest Autopilot
    /// task (autopilot-plan H08), as `rovik-agentd status` lists them.
    pub async fn tasks(&self, id: &str) -> Result<Vec<ChatTask>, String> {
        if !self.remote.serves(id, features::AUTONOMY) {
            return Err("This server's Rovik is too old to list its tasks.".into());
        }
        let listed = self.remote.host_call(id, Request::TaskStatus).await;
        let listed = listed.map_err(|fail| fail.message)?;
        Vec::<ChatTask>::try_from(listed).map_err(|fail| fail.message)
    }

    /// "Test delivery" (autopilot-plan H05): the host pushes an empty
    /// branch to the open project's remote and deletes it.
    pub async fn test_delivery(&self, id: &str) -> Result<DeliveryTest, String> {
        self.remote.test_delivery(id).await
    }

    /// Deploys Rovik for this user (H05), then connects the host. Returns
    /// the daemon's version.
    pub async fn deploy(
        &self,
        id: &str,
        sudo_password: Option<Secret>,
    ) -> Result<String, DeployFailure> {
        let host = self.host(id)?;
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        let builds = Builds::new(self.remote.data_dir(), &self.version)?;
        let computer = self.settings()?.computer;
        let layout = Layout::default();
        let deployed = deploy::deploy(&target, &builds, &computer, sudo_password.as_ref(), &layout);
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
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        let builds = Builds::new(self.remote.data_dir(), &self.version)?;
        let installed = deploy::upgrade(&target, &builds, &Layout::default()).await?;
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
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        Ok(deploy::run_setup(target.shell, script).await?)
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
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        Ok(deploy::removal(target.shell).await?)
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
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        deploy::remove(&target, removal, &Layout::default()).await?;
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
    /// as each day it does. What Rovik's folders take there after, and how
    /// much less than just before: measured then, not when the page asked,
    /// so what changed meanwhile isn't counted as freed.
    pub async fn clean_up(&self, id: &str) -> Result<CleanedUp, String> {
        // Both measure every folder, which takes a while on a big server.
        let ask = |request| async move {
            let reply = self.remote.host_call_within(id, request, CLEAN_UP_WAIT);
            let storage = reply.await.and_then(Storage::try_from);
            storage.map_err(|fail| fail.message)
        };
        let before = ask(Request::Storage).await?;
        let after = ask(Request::CleanUp).await?;
        Ok(CleanedUp {
            freed: before.total().saturating_sub(after.total()),
            storage: after,
        })
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
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        Ok(deploy::limits(&target, &Layout::default()).await?)
    }

    pub async fn set_limits(
        &self,
        id: &str,
        changes: &[(String, String)],
    ) -> Result<(), DeployFailure> {
        let host = self.host(id)?;
        let link = self.link(&host)?;
        let session = link.session().await?;
        let target = link.target(session.as_deref(), &host.name)?;
        Ok(deploy::set_limits(&target, changes, &Layout::default()).await?)
    }

    async fn call<T>(&self, id: &str, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = rovik_protocol::Fail>,
    {
        let reply = self.remote.host_call(id, request).await;
        reply.and_then(T::try_from).map_err(|fail| fail.message)
    }
}

/// The WSL distribution's host in the settings (Windows): one per
/// computer, set up from Settings → This computer → WSL, kept out of the
/// Cloud hosts page. Its chats are this computer's.
pub const WSL_HOST: &str = "wsl";

/// How long installing the distribution's packages may take.
const PACKAGES_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// The WSL page's state (Settings → This computer → WSL).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WslStatus {
    /// `wsl.exe` answers here: Windows with WSL installed.
    pub available: bool,
    /// Why it doesn't, when it doesn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// This Windows can't run WSL2 (`wsl::unsupported_windows`): no
    /// install would help.
    pub unsupported: bool,
    /// The distributions installed here.
    pub distros: Vec<String>,
    /// The distribution set up, when one is, and the user in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Rovik runs in it for this computer.
    pub deployed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<HostState>,
    /// Why the app couldn't connect to it, when it couldn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Rovik there is older than this app: the app updates it at its
    /// start (`wsl_update`).
    pub outdated: bool,
    pub app_version: String,
}

impl HostsPage {
    /// The WSL distribution's host, once set up.
    pub fn wsl_host(&self) -> Result<Option<Host>, String> {
        Ok(self.settings()?.host(WSL_HOST).cloned())
    }

    pub async fn wsl_status(&self) -> Result<WslStatus, String> {
        let unsupported = wsl::unsupported_windows().await;
        let (available, unavailable, distros) = match (&unsupported, wsl::distros().await) {
            (Some(why), _) => (false, Some(why.clone()), Vec::new()),
            (None, Ok(distros)) => (true, None, distros),
            (None, Err(why)) => (false, Some(wsl::explained(why)), Vec::new()),
        };
        let host = self.wsl_host()?;
        let (distro, user) = match host.as_ref().and_then(Host::wsl) {
            Some(wsl) => (Some(wsl.distro), wsl.user),
            None => (None, None),
        };
        let deploy = host.map(|h| h.deploy).unwrap_or_default();
        let version = match &deploy {
            DeployState::Deployed { version, .. } => Some(version.clone()),
            DeployState::NotDeployed => None,
        };
        let state = self
            .remote
            .states()
            .into_iter()
            .find(|(id, _)| id == WSL_HOST)
            .map(|(_, state)| state);
        let problem = self
            .unconnected()
            .get(WSL_HOST)
            .cloned()
            .or_else(|| self.remote.refusal(WSL_HOST))
            .map(wsl::explained);
        Ok(WslStatus {
            available,
            unavailable,
            unsupported: unsupported.is_some(),
            distros,
            distro,
            user,
            deployed: matches!(deploy, DeployState::Deployed { .. }),
            outdated: version
                .as_deref()
                .is_some_and(|version| older(version, &self.version)),
            version,
            state: state.or(problem.as_ref().map(|_| HostState::Offline)),
            problem,
            app_version: self.version.clone(),
        })
    }

    /// Sets the distribution up as this computer's Linux side: Rovik
    /// deployed in it (installed as root inside it, no password), with the
    /// packages its sandbox and overlays take (bubblewrap, FUSE), this
    /// computer's token added, and its daemon started and connected. A
    /// distribution set up before is let go of first. Returns the daemon's
    /// version.
    pub async fn wsl_setup(
        &self,
        distro: String,
        user: Option<String>,
    ) -> Result<String, DeployFailure> {
        let mut wsl = Wsl::new(distro, user);
        // A distribution whose default user is root (installed by the app,
        // `--no-launch`, or by hand without its first run) gets one: the
        // daemon and the commands don't run as root.
        if wsl.user.is_none() {
            wsl.user = Self::wsl_user(&wsl).await?;
        }
        if let Some(old) = self.wsl_host()? {
            if old.wsl().as_ref() != Some(&wsl) {
                self.wsl_remove().await?;
            }
        }
        let host = Host {
            id: WSL_HOST.into(),
            name: "WSL".into(),
            transport: Transport::Wsl {
                distro: wsl.distro.clone(),
                user: wsl.user.clone(),
            },
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: AuthKind::Agent,
            key_path: None,
            host_key: None,
            approval_policy: ApprovalPolicy::default(),
            autonomy: AutonomySettings::default(),
            deploy: DeployState::NotDeployed,
            setup_script: None,
        };
        self.save(host, HostSecrets::default())?;
        // The packages, as root in the distribution: best-effort, as the
        // installer adds bubblewrap itself and says when it couldn't.
        let root: &dyn HostShell = &wsl.root_shell();
        if let Err(e) = root.run(&wsl::packages_script(), &[], PACKAGES_WAIT).await {
            log::warn!("{}: its packages weren't installed: {e}", wsl.display());
        }
        self.deploy(WSL_HOST, None).await.map_err(explained)
    }

    /// Tests the distribution's connection, a step at a time.
    pub async fn wsl_test(
        &self,
        report: &(dyn Fn(&Step) + Send + Sync),
    ) -> Result<Vec<Step>, String> {
        self.test(WSL_HOST, report).await
    }

    /// Whom Rovik runs as in a distribution the user named no user for:
    /// its default user, or `rovik`, made as root, when that's root.
    async fn wsl_user(wsl: &Wsl) -> Result<Option<String>, DeployFailure> {
        let shell: &dyn HostShell = &wsl.shell();
        let out = shell.run("id -un", &[], crate::ssh::EXEC_WAIT).await?;
        if out.status != Some(0) {
            let said = String::from_utf8_lossy(&out.stderr);
            return Err(DeployFailure::Failed {
                message: wsl::explained(format!("{}: {}", wsl.display(), said.trim())),
            });
        }
        if String::from_utf8_lossy(&out.stdout).trim() != "root" {
            return Ok(None);
        }
        let root: &dyn HostShell = &wsl.root_shell();
        let made = root.run(&wsl::user_script(), &[], PACKAGES_WAIT).await?;
        let user = String::from_utf8_lossy(&made.stdout).trim().to_string();
        if made.status != Some(0) || user.is_empty() {
            return Err(DeployFailure::Failed {
                message: format!(
                    "{}: its default user is root, and no user could be made for Rovik: {}",
                    wsl.display(),
                    String::from_utf8_lossy(&made.stderr).trim()
                ),
            });
        }
        Ok(Some(user))
    }

    /// Rovik in the distribution brought to this app's version, as an app
    /// update leaves it behind: the user is the administrator there.
    pub async fn wsl_update(&self) -> Result<(), DeployFailure> {
        self.upgrade(WSL_HOST).await.map_err(explained)
    }

    /// Rovik removed from the distribution, which the app then forgets.
    pub async fn wsl_remove(&self) -> Result<(), DeployFailure> {
        let deployed = self
            .wsl_host()?
            .is_some_and(|host| matches!(host.deploy, DeployState::Deployed { .. }));
        if deployed {
            let removal = self.removal(WSL_HOST).await?;
            self.remove_rovik(WSL_HOST, &removal).await?;
        }
        self.remove(WSL_HOST).await.map_err(DeployFailure::from)
    }
}

/// A WSL deploy's failure, with what `wsl.exe`'s error means in front
/// when it's a known one (`wsl::explain`).
fn explained(failure: DeployFailure) -> DeployFailure {
    match failure {
        DeployFailure::Failed { message } => DeployFailure::Failed {
            message: wsl::explained(message),
        },
        other => other,
    }
}

/// The host's fields the page fills in, before they're saved.
fn check(host: &Host) -> Result<(), String> {
    let blank = |text: &str| text.trim().is_empty();
    // The id names a socket file (`local`) and goes in paths: letters,
    // digits, - and _, as a UUID has.
    let plain_id = host
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !plain_id || host.id.len() > 64 {
        return Err("The host's id is letters, digits, - and _.".into());
    }
    if blank(&host.name) {
        return Err("Give the host a name.".into());
    }
    // A WSL distribution: its name and the user's, which go to wsl.exe as
    // arguments (one that starts with - would be an option to it).
    if let Some(wsl) = host.wsl() {
        let plain = |text: &str| {
            !blank(text)
                && !text.contains(char::is_whitespace)
                && !text.starts_with('-')
                && !text.contains(char::is_control)
        };
        if !plain(&wsl.distro) {
            return Err("Choose the WSL distribution.".into());
        }
        if wsl.user.as_deref().is_some_and(|user| !plain(user)) {
            return Err("The user in the distribution is a name without spaces.".into());
        }
        return Ok(());
    }
    // An address or a user that starts with - would be an option to ssh.
    if blank(&host.host)
        || host.host.contains(char::is_whitespace)
        || host.host.starts_with('-')
        || host.host.contains(char::is_control)
    {
        return Err("Enter the host's name or address.".into());
    }
    if blank(&host.user)
        || host.user.contains(char::is_whitespace)
        || host.user.starts_with('-')
        || host.user.contains(char::is_control)
    {
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

/// The user's own ssh to `host`, as its settings reach it (H03).
fn system_ssh(host: &Host) -> SystemSsh {
    SystemSsh {
        destination: format!("{}@{}", host.user, host.host),
        port: (host.port != 22).then_some(host.port),
        program: "ssh".into(),
    }
}

/// Answers the server's questions with the saved password: a round whose
/// every prompt asks for a password, and a round with no prompt (only its
/// instructions). Any other (a verification code, a one-time password)
/// gives up: only the user can answer it, and the password goes to no
/// other prompt. Without a saved password, it gives up too.
fn answering(password: Option<Secret>) -> Answerer {
    Arc::new(move |questions: Questions| {
        let password = password.clone();
        Box::pin(async move {
            if questions.prompts.is_empty() {
                return Some(Vec::new());
            }
            let password = password?;
            let all = questions.prompts.iter();
            if !all
                .clone()
                .all(|(prompt, echo)| asks_password(prompt, *echo))
            {
                return None;
            }
            Some(all.map(|_| password.clone()).collect())
        })
    })
}

/// Whether a keyboard-interactive prompt asks for the account's password:
/// typed hidden, it names a password, and no one-time code.
fn asks_password(prompt: &str, echo: bool) -> bool {
    let prompt = prompt.to_lowercase();
    let code = [
        "one-time",
        "one time",
        "otp",
        "oath",
        "verification",
        "code",
        "token",
        "passcode",
        "pin",
        "authenticator",
        "2fa",
        "factor",
        "duo",
        "yubi",
    ];
    !echo && prompt.contains("password") && !code.iter().any(|word| prompt.contains(word))
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

    /// Keyboard-interactive: the saved password answers the server's
    /// password prompts and nothing else, a two-factor code's least of all.
    #[tokio::test]
    async fn the_password_answers_only_password_prompts() {
        let answer = answering(Some(Secret("hunter2".into())));
        let round = |prompts: &[(&str, bool)]| Questions {
            name: String::new(),
            instructions: String::new(),
            prompts: prompts.iter().map(|(p, e)| (p.to_string(), *e)).collect(),
        };
        let answered = answer(round(&[("Password: ", false)])).await;
        assert_eq!(answered, Some(vec![Secret("hunter2".into())]));
        let theirs = answer(round(&[("ann@build's password:", false)])).await;
        assert!(theirs.is_some());
        assert_eq!(
            answer(round(&[])).await,
            Some(Vec::new()),
            "instructions only"
        );
        for prompt in [
            ("Verification code: ", false),
            ("One-time password (OATH) for `ann': ", false),
            ("Passcode or option (1-3): ", false),
            ("Password: ", true),
            ("Username: ", true),
        ] {
            assert_eq!(answer(round(&[prompt])).await, None, "{prompt:?}");
        }
        let both = answer(round(&[
            ("Password: ", false),
            ("Verification code: ", false),
        ]));
        assert_eq!(both.await, None);
        assert_eq!(answering(None)(round(&[("Password: ", false)])).await, None);
    }
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
            transport: Transport::Ssh,
            host: "build.example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: AuthKind::Password,
            key_path: None,
            host_key: None,
            approval_policy: ApprovalPolicy::default(),
            autonomy: AutonomySettings::default(),
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
            .map(|s| {
                (
                    s.name.as_str(),
                    s.deployed,
                    s.state,
                    s.caught_up,
                    s.terminal,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("Build box", false, None, false, true),
                ("agentd.sock", true, Some(HostState::Ready), true, false),
            ]
        );
        assert_eq!(summaries[1].id, socket);
    }

    /// A terminal reaches a host as its daemon's connection does: Rovik's
    /// SSH with the saved way in, or the user's own ssh.
    #[tokio::test]
    async fn a_terminal_reaches_a_host_the_way_its_settings_say() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("hunter2")).unwrap();
        let Ok(ShellAccess::Ssh(target)) = setup.page.shell_access(&saved.id) else {
            panic!("not through Rovik's SSH");
        };
        let SshAuth::Password(secret) = &target.auth else {
            panic!("not with the password");
        };
        let reached = (target.host.as_str(), target.port, target.user.as_str());
        assert_eq!(reached, ("build.example.com", 22, "ann"));
        assert_eq!(secret.0, "hunter2");

        let own = Host {
            auth: AuthKind::SystemSsh,
            port: 2222,
            ..saved
        };
        let own = setup.page.save(own, HostSecrets::default()).unwrap();
        let Ok(ShellAccess::System(ssh)) = setup.page.shell_access(&own.id) else {
            panic!("not through the user's own ssh");
        };
        assert_eq!(ssh.destination, "ann@build.example.com");
        assert_eq!(ssh.port, Some(2222));
        assert!(setup.page.shell_access("gone").is_err());
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
        assert!(
            list.hosts[0].serves.is_empty(),
            "a daemon that hasn't spoken serves nothing"
        );
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
            .await
            .unwrap();
        let moved = Host {
            host: "other.example.com".into(),
            ..kept
        };
        let moved = setup.page.save(moved, HostSecrets::default()).unwrap();
        assert_eq!(moved.host_key, None, "another server shows its own key");
        assert_eq!(moved.approval_policy, ApprovalPolicy::AiReviewerThenMe);
    }

    /// H01: the block a host works on its own with, checked and kept; the
    /// host's form saved again leaves it as it was.
    #[tokio::test]
    async fn the_autonomy_block_is_checked_and_kept_by_the_hosts_page() {
        use rovik_protocol::autopilot::Delivery;
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        setup
            .page
            .set_policy(&saved.id, ApprovalPolicy::Autonomous)
            .await
            .unwrap();
        let bad = AutonomySettings {
            notify_url: Some("mailto:me@example.com".into()),
            ..Default::default()
        };
        assert!(setup.page.set_autonomy(&saved.id, bad).await.is_err());
        let block = AutonomySettings {
            delivery: Delivery::PushBranch {
                deploy_key: " ~/.ssh/rovik_deploy ".into(),
                open_pr: true,
            },
            notify_url: Some("https://ntfy.sh/t".into()),
            effort: Some(Effort::Thorough),
            ..Default::default()
        };
        let kept = setup.page.set_autonomy(&saved.id, block).await.unwrap();
        assert_eq!(
            kept.delivery,
            Delivery::PushBranch {
                deploy_key: "~/.ssh/rovik_deploy".into(),
                open_pr: true
            }
        );
        // The form, saved with the block it never had.
        let form = Host {
            name: "Renamed".into(),
            autonomy: AutonomySettings::default(),
            ..saved.clone()
        };
        let saved = setup.page.save(form, HostSecrets::default()).unwrap();
        assert_eq!(saved.autonomy, kept);
        assert_eq!(saved.approval_policy, ApprovalPolicy::Autonomous);
        let listed = setup.page.list().await.unwrap();
        assert_eq!(listed.hosts[0].host.autonomy, kept);
        // H02: the chats are told it works on its own, and until when,
        // with the effort its tasks take then (T09: what they need).
        let summary = &setup.page.summaries().unwrap()[0];
        assert!(summary.autonomous);
        assert_eq!(summary.effort, Some(Effort::Thorough));
        setup
            .page
            .set_policy(&saved.id, ApprovalPolicy::WaitForMe)
            .await
            .unwrap();
        let summary = &setup.page.summaries().unwrap()[0];
        assert!(!summary.autonomous);
        assert_eq!(summary.effort, None, "the computer's, under another policy");
    }

    /// Another window over the same settings and secrets: a process of its
    /// own, with its own connections.
    fn another_window(setup: &Setup) -> (HostsPage, Arc<Remote>) {
        let remote = Remote::new(setup.app.core.clone());
        let page = HostsPage::new(
            setup.app.dir.0.join("config"),
            setup.app.dir.0.join("ssh"),
            setup.page.secrets.clone(),
            remote.clone(),
            "0.2.0",
        );
        (page, remote)
    }

    /// A window that set a host's policy: the other window, which read the
    /// file before, has it once it follows the file (and Remote asks it to
    /// before each configuration it sends the host), not its old one.
    #[tokio::test]
    async fn a_policy_another_window_set_is_the_one_sent() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        let (other, theirs) = another_window(&setup);
        other.connect_all();
        assert_eq!(theirs.policy(&saved.id), ApprovalPolicy::WaitForMe);
        setup
            .page
            .set_policy(&saved.id, ApprovalPolicy::AiReviewerThenMe)
            .await
            .unwrap();
        assert_eq!(
            theirs.policy(&saved.id),
            ApprovalPolicy::WaitForMe,
            "not read yet"
        );
        other.follow_policies();
        assert_eq!(theirs.policy(&saved.id), ApprovalPolicy::AiReviewerThenMe);
    }

    /// The hosts another window deployed, deployed again (a new token) or
    /// removed: this window's connections follow the file.
    #[tokio::test]
    async fn the_hosts_another_window_changes_are_followed_here() {
        let setup = setup();
        let saved = setup.page.save(new_host(), password("pw")).unwrap();
        let (other, theirs) = another_window(&setup);
        other.connect_all();
        let connected = |remote: &Remote| remote.states().iter().any(|(id, _)| *id == saved.id);
        assert!(!connected(&theirs), "not deployed: not connected");

        // Deployed in this window: connected in the other once it follows.
        let token = SecretName::Token {
            host: saved.id.clone(),
        };
        setup
            .page
            .secrets
            .set(&token, &Secret("t1".into()))
            .unwrap();
        let deployed = |version: &str| DeployState::Deployed {
            version: version.into(),
            sha256: None,
        };
        setup
            .page
            .change_host(&saved.id, |h| h.deploy = deployed("0.2.0"))
            .unwrap();
        other.follow();
        assert!(connected(&theirs));
        let made = other.followed().connected[&saved.id].clone();

        // Deployed again: a new token, and the same settings.
        setup
            .page
            .secrets
            .set(&token, &Secret("t2".into()))
            .unwrap();
        setup
            .page
            .change_host(&saved.id, |h| h.deploy = deployed("0.2.0"))
            .unwrap();
        other.follow();
        let again = other.followed().connected[&saved.id].clone();
        assert_eq!(made.0, again.0);
        assert_ne!(made.1, again.1, "connected again with the new token");
        assert!(
            !again.1.as_deref().unwrap_or("").contains("t2"),
            "not the token itself"
        );

        // Removed there: let go of here.
        setup.page.remove(&saved.id).await.unwrap();
        other.follow();
        assert!(!connected(&theirs));
        assert!(!other.followed().connected.contains_key(&saved.id));
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

    /// "Clean up now": the host's folders measured just before the cleanup
    /// and after it; the page gets what's left, and what went.
    #[tokio::test]
    async fn clean_up_says_what_it_freed_measured_just_before() {
        use crate::testing::{identity, Fake};
        let setup = setup();
        let host = setup.page.save(new_host(), password("hunter2")).unwrap();
        let gb = 1 << 30;
        let before = Storage {
            projects: gb,
            work: 3 * gb,
            data: gb,
            tmp: 2 * gb,
        };
        let after = Storage {
            work: gb,
            tmp: 0,
            ..before.clone()
        };
        let fake = Fake::default();
        *fake.storage.lock().unwrap() = Some((before, after.clone()));
        setup
            .remote
            .connect(&host.id, fake.clone(), identity(&host.name));
        for _ in 0..400 {
            if (setup.remote.states().iter())
                .any(|(id, s)| *id == host.id && *s == HostState::Ready)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let cleaned = setup.page.clean_up(&host.id).await.unwrap();
        assert_eq!(cleaned.storage, after);
        assert_eq!(cleaned.freed, 4 * gb);
        let asked = fake.requests.lock().unwrap().clone();
        assert!(
            matches!(asked[..], [.., Request::Storage, Request::CleanUp]),
            "{asked:?}"
        );
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
            transport: Transport::Ssh,
            host: "build.example.com".into(),
            port: 22,
            user: "ann".into(),
            auth: AuthKind::Key,
            key_path: Some("/home/ann/.ssh/id_ed25519".into()),
            host_key: None,
            approval_policy: ApprovalPolicy::default(),
            autonomy: AutonomySettings::default(),
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
