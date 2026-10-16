//! Deploying to a host (H05), through the in-process SSH server and the
//! real daemon. The server's commands run as `nobody` when the tests run as
//! root, since a deploy refuses a root login. A stand-in for sudo runs the
//! installer under `--root` and starts and stops the daemon as systemd
//! would, so nothing changes on this machine. The deploy probes the
//! server, uploads a build, adds this computer's token, installs Rovik,
//! starts the daemon and says Hello to it with the new token. Then come
//! the check each connection makes, and the removal.

mod common;

use common::ssh::{Account, SshServer, PASSWORD, USER};
use common::BINARY;
use rovik_client::connection_test::{test_connection, Check, Outcome, TestOptions};
use rovik_client::deploy::{self, Binary, DeployError, Layout};
use rovik_client::ssh::{SshAuth, SshConnector, SshTarget};
use rovik_client::Identity;
use rovik_protocol::{Secret, PROTOCOL};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const NOBODY: (u32, u32) = (65534, 65534);

/// A server under a temporary folder: its user's home, the installer's
/// root, and the stand-ins.
struct Host {
    dir: rovik_core::testing::TempDir,
    home: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    ids: Option<(u32, u32)>,
    server: SshServer,
}

impl Host {
    async fn new() -> Host {
        let dir = rovik_core::testing::temp_dir();
        // SAFETY: geteuid can't fail.
        let ids = (unsafe { libc::geteuid() } == 0).then_some(NOBODY);
        let (home, root, bin) = (dir.0.join("home"), dir.0.join("root"), dir.0.join("bin"));
        for folder in [&dir.0, &home, &root, &bin] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
            own(folder, ids);
        }
        let installed = bin.join("rovik-agentd");
        std::os::unix::fs::symlink(BINARY, &installed).unwrap();
        // What the installer copies, in place of 500 MB of debug build.
        script(&bin.join("small"), "#!/bin/sh\nexit 0\n", ids);
        let build = format!(
            "#!/bin/sh\n\
             # The build a deploy uploads: the real daemon, which installs a small file.\n\
             case \"$1\" in\n\
             install) exec '{BINARY}' \"$@\" --binary '{small}' ;;\n\
             *) exec '{BINARY}' \"$@\" ;;\n\
             esac\n",
            small = bin.join("small").display()
        );
        script(&bin.join("build"), &build, ids);
        let stop = format!(
            "pkill -u \"$(id -u)\" -f '^{installed} serve'; \
             while pgrep -u \"$(id -u)\" -f '^{installed} serve' >/dev/null; do sleep 0.2; done",
            installed = installed.display()
        );
        let start = format!(
            "{stop}; setsid '{installed}' serve </dev/null >/dev/null 2>&1 &",
            installed = installed.display()
        );
        let sudo = format!(
            "#!/bin/sh\n\
             # sudo for the tests: its options dropped and the password read; the\n\
             # installer runs under --root, and the daemon starts and stops as\n\
             # systemd would.\n\
             while [ $# -gt 0 ]; do\n\
               case \"$1\" in\n\
                 -n) shift ;;\n\
                 -S) read -r _ ; shift ;;\n\
                 -p) shift 2 ;;\n\
                 *) break ;;\n\
               esac\n\
             done\n\
             echo \"$*\" >> '{log}'\n\
             case \"$2 $3\" in\n\
               install*) \"$@\" --root '{root}' >/dev/null || exit 1\n\
                 # systemd starts the administrator's daemon; without it, the app.\n\
                 if [ -d /run/systemd/system ]; then\n\
                   {start}\n\
                 fi ;;\n\
               'user start'|'user restart') {start} ;;\n\
               'user stop') {stop} ;;\n\
               *) exit 1 ;;\n\
             esac\n",
            log = dir.0.join("sudo.log").display(),
            root = root.display(),
        );
        script(&bin.join("sudo"), &sudo, ids);
        let server = SshServer::as_user(Account {
            home: home.clone(),
            ids,
        })
        .await;
        Host {
            dir,
            home,
            root,
            bin,
            ids,
            server,
        }
    }

    fn connector(&self) -> SshConnector {
        let target = SshTarget {
            host: "127.0.0.1".into(),
            port: self.server.port,
            user: USER.into(),
            auth: SshAuth::Password(Secret(PASSWORD.into())),
            host_key: Some(self.server.host_key_line()),
        };
        let attach = format!("{} attach --stdio", self.installed().display());
        SshConnector::new(target).with_attach(attach)
    }

    fn layout(&self) -> Layout {
        Layout {
            installed: self.installed().to_string_lossy().into_owned(),
            sudo: self.bin.join("sudo").to_string_lossy().into_owned(),
        }
    }

    fn installed(&self) -> PathBuf {
        self.bin.join("rovik-agentd")
    }

    fn sudo_log(&self) -> String {
        std::fs::read_to_string(self.dir.0.join("sudo.log")).unwrap_or_default()
    }
}

impl Drop for Host {
    /// The daemon goes with the test, whatever happened.
    fn drop(&mut self) {
        let pattern = format!("^{} serve", self.installed().display());
        let mut pkill = std::process::Command::new("pkill");
        pkill.arg("-f").arg(pattern);
        if let Some((uid, _)) = self.ids {
            pkill.arg("-u").arg(uid.to_string());
        }
        let _ = pkill.status();
    }
}

fn script(path: &Path, text: &str, ids: Option<(u32, u32)>) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    own(path, ids);
}

fn own(path: &Path, ids: Option<(u32, u32)>) {
    if let Some((uid, gid)) = ids {
        std::os::unix::fs::chown(path, Some(uid), Some(gid)).unwrap();
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every file under `dir`, read as text where it is text.
fn texts_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    found.push((path, String::from_utf8_lossy(&bytes).into_owned()));
                }
                _ => {}
            }
        }
    }
    found
}

/// H05: a first deploy installs Rovik, its user the administrator, starts
/// the daemon with this computer's token, and the daemon answers Hello;
/// the token is nowhere on the server, only its hash. The deploy trusts
/// the build the server runs, the one each connection checks: another is
/// reported as changed, for the user to compare. A removal stops the
/// daemon and deletes the user's two folders.
#[tokio::test(flavor = "multi_thread")]
async fn a_deploy_installs_rovik_and_the_daemon_answers_this_computers_token() {
    let host = Host::new().await;
    let connector = host.connector();
    let build = Binary::at(host.bin.join("build")).unwrap();
    let password = Secret("the user's sudo password".into());
    let deployed = deploy::deploy(
        &connector,
        &build,
        "laptop",
        Some(&password),
        &host.layout(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}\nsudo said: {}", host.sudo_log()));
    assert_eq!(deployed.daemon.protocol, PROTOCOL);
    // What the server runs, not what went up: here the installed link is
    // the real daemon, as it would be another user's install.
    let agentd = std::fs::read(BINARY).unwrap();
    let known = vec![sha256(&agentd)];
    assert_eq!(deployed.sha256, known[0]);
    assert_ne!(deployed.sha256, build.sha256);

    let user = match host.ids {
        Some(_) => "nobody".to_string(),
        None => {
            let id = std::process::Command::new("id")
                .arg("-un")
                .output()
                .unwrap();
            String::from_utf8_lossy(&id.stdout).trim().to_string()
        }
    };
    let log = host.sudo_log();
    let upload = host.home.join(".rovik-agentd-upload");
    assert!(
        log.starts_with(&format!("{} install --admin {user}", upload.display())),
        "{log}"
    );
    assert!(!upload.exists(), "the build is gone once installed");
    let settings = std::fs::read_to_string(host.root.join("etc/rovik/agentd.toml")).unwrap();
    assert!(
        settings.contains(&format!("admins = [\"{user}\"]")),
        "{settings}"
    );
    let config = std::fs::read_to_string(host.home.join(".rovik/config.toml")).unwrap();
    let hash = sha256(deployed.token.0.as_bytes());
    assert!(config.contains(&format!("laptop = \"{hash}\"")), "{config}");
    for (path, text) in texts_under(&host.dir.0) {
        assert!(
            !text.contains(&deployed.token.0),
            "the token is in {path:?}"
        );
        assert!(!text.contains(&password.0), "the password is in {path:?}");
    }

    // The check each connection makes (D5).
    let ssh = connector.session().await.unwrap();
    let checked = deploy::check_install(&ssh, &known, &user, &host.layout()).await;
    assert_eq!(checked.unwrap().sha256, known[0]);
    let changed = deploy::check_install(&ssh, &["00".repeat(32)], &user, &host.layout()).await;
    let Err(DeployError::Changed(now)) = changed else {
        panic!("a binary changed behind the app's back wasn't reported: {changed:?}");
    };
    assert_eq!(now.sha256, known[0]);
    assert_eq!(
        now.version, None,
        "a build of its own, not rovik-agentd-<version>"
    );
    // The connection test shows a changed build for the user to compare
    // and trust, and stops there; the build trusted passes.
    let options = |trusted: &str| TestOptions {
        deployed: true,
        known: vec![trusted.to_string()],
        layout: host.layout(),
    };
    let steps = test_connection(&connector, &options(&"00".repeat(32)), &|_| {}).await;
    let last = steps.last().unwrap();
    assert_eq!((last.check, last.outcome), (Check::Build, Outcome::Confirm));
    assert_eq!(
        last.build.as_ref().map(|b| b.sha256.as_str()),
        Some(known[0].as_str())
    );
    let steps = test_connection(&connector, &options(&known[0]), &|_| {}).await;
    let build = steps.iter().find(|s| s.check == Check::Build).unwrap();
    assert_eq!(build.outcome, Outcome::Passed, "{}", build.message);
    // A connector that checks on each connection looks where the installer
    // puts Rovik, which has nothing here: refused, saying so.
    let checked = host.connector().with_install_check(known.clone());
    let refused = checked.session().await.err().expect("refused");
    assert!(
        refused.to_string().contains("isn't on the server"),
        "{refused}"
    );

    // Removal: what goes, then gone.
    let removal = deploy::removal(&connector).await.unwrap();
    let folders: Vec<&str> = removal.folders.iter().map(|(p, _)| p.as_str()).collect();
    let home = host.home.display();
    let expected = [
        format!("{home}/.rovik"),
        format!("{home}/.local/share/rovik"),
    ];
    assert_eq!(folders, expected);
    assert!(removal.folders[0].1 > 0, "sizes: {removal:?}");
    deploy::remove(&connector, &removal, &host.layout())
        .await
        .unwrap();
    assert!(!host.home.join(".rovik").exists());
    assert!(!host.home.join(".local/share/rovik").exists());
    let identity = Identity {
        host: "test".into(),
        computer: "laptop".into(),
        token: deployed.token.clone(),
        client: "deploy tests".into(),
    };
    assert!(
        rovik_client::welcome(&connector, &identity).await.is_err(),
        "it stopped"
    );
}

/// U01: the host's setup script runs as the user, in their home folder,
/// and what it printed comes back in order, with how it ended.
#[tokio::test(flavor = "multi_thread")]
async fn the_setup_script_runs_at_home_and_says_how_it_went() {
    let host = Host::new().await;
    let connector = host.connector();
    let script = "echo one\necho two >&2\ntouch setup-ran\nexit 3\n";
    let run = deploy::run_setup(&connector, script).await.unwrap();
    assert_eq!(run.status, Some(3));
    assert_eq!(run.output, "one\ntwo\n");
    assert!(host.home.join("setup-ran").exists());
}

/// H05: a root login is refused, before anything is uploaded.
#[tokio::test(flavor = "multi_thread")]
async fn a_root_login_is_refused() {
    // SAFETY: geteuid can't fail.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let dir = rovik_core::testing::temp_dir();
    let server = SshServer::as_user(Account {
        home: dir.0.clone(),
        ids: None,
    })
    .await;
    let target = SshTarget {
        host: "127.0.0.1".into(),
        port: server.port,
        user: USER.into(),
        auth: SshAuth::Password(Secret(PASSWORD.into())),
        host_key: Some(server.host_key_line()),
    };
    let build = dir.0.join("build");
    std::fs::write(&build, "#!/bin/sh\n").unwrap();
    let refused = deploy::deploy(
        &SshConnector::new(target),
        &Binary::at(build).unwrap(),
        "laptop",
        None,
        &Layout::default(),
    )
    .await;
    assert_eq!(refused.err(), Some(DeployError::RootLogin));
    assert!(!dir.0.join(".rovik-agentd-upload").exists());
}
