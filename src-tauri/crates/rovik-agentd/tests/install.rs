//! Rovik for everyone on a server (H07), through the real binary with
//! `--root`: the installer's files land under a folder, and each command
//! that would change the system is printed instead of run. `nobody` plays
//! the administrator, and another account a user who isn't one.

use rovik_protocol::release::fixture;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_rovik-agentd");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const NOBODY: u32 = 65534;

/// A server under a folder, with systemd, and a small build to install in
/// place of this 500 MB debug one.
struct Server {
    dir: rovik_core::testing::TempDir,
    root: PathBuf,
    build: PathBuf,
}

impl Server {
    fn new() -> Server {
        let dir = rovik_core::testing::temp_dir();
        let root = dir.0.join("root");
        std::fs::create_dir_all(root.join("run/systemd/system")).unwrap();
        let build = dir.0.join("build");
        script(&build, "0.1.0");
        Server { dir, root, build }
    }

    /// `args` as root under the folder; through sudo for the user with
    /// `sudo_uid`.
    fn run(&self, args: &[&str], sudo_uid: Option<u32>) -> Output {
        self.run_with_key(args, sudo_uid, None)
    }

    /// `run`, the binary checking releases with `key` (a debug build takes
    /// `ROVIK_RELEASE_KEY`); with none, it has no key.
    fn run_with_key(&self, args: &[&str], sudo_uid: Option<u32>, key: Option<&str>) -> Output {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .arg("--root")
            .arg(&self.root)
            .env_remove("SUDO_UID")
            .env("ROVIK_RELEASE_KEY", key.unwrap_or(""));
        if let Some(uid) = sudo_uid {
            command.env("SUDO_UID", uid.to_string());
        }
        command.output().unwrap()
    }

    fn file(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    fn text(&self, absolute: &str) -> String {
        std::fs::read_to_string(self.file(absolute)).unwrap_or_default()
    }

    fn installed() -> Server {
        let server = Server::new();
        let build = server.build.to_string_lossy().into_owned();
        let out = server.run(&["install", "--admin", "nobody", "--binary", &build], None);
        assert!(out.status.success(), "{}", said(&out));
        server
    }
}

/// A stand-in build that answers `version` as `version`.
fn script(path: &Path, version: &str) {
    let text = format!("#!/bin/sh\necho 'rovik-agentd {version} (protocol 1)'\n");
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// An account that isn't root or nobody, and its id.
fn someone_else() -> (String, u32) {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap();
    passwd
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let uid = fields.get(2)?.parse::<u32>().ok()?;
            let name = fields[0];
            let plain = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            (uid != 0 && uid != NOBODY && plain).then(|| (name.to_string(), uid))
        })
        .next()
        .expect("an account besides root and nobody")
}

#[test]
fn install_puts_everything_in_place_and_starts_the_administrators_daemon() {
    let server = Server::installed();
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new(&format!("rovik-agentd-{VERSION}")));
    let copy = server.file(&format!("/opt/rovik/bin/rovik-agentd-{VERSION}"));
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&server.build).unwrap()
    );
    let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);

    let settings = server.text("/etc/rovik/agentd.toml");
    assert!(settings.contains("\nadmins = [\"nobody\"]\n"), "{settings}");
    assert!(
        settings.contains("\nmemory_max = \""),
        "sized to the machine: {settings}"
    );
    let unit = server.text("/etc/systemd/system/rovik-agentd@.service");
    assert!(
        unit.contains("ExecStart=/opt/rovik/bin/rovik-agentd serve"),
        "{unit}"
    );
    let limits = server.text("/etc/systemd/system/rovik-agentd@.service.d/limits.conf");
    assert!(limits.contains("\nMemoryMax="), "{limits}");
    assert!(server
        .text("/etc/systemd/system/rovik.slice")
        .contains("\nMemoryMax="));
    let sudoers = server.file("/etc/sudoers.d/rovik");
    if sudoers.exists() {
        let mode = std::fs::metadata(&sudoers).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o440);
    }

    // Run again, it keeps the settings and makes another administrator.
    let (other, _) = someone_else();
    let build = server.build.to_string_lossy().into_owned();
    let again = server.run(&["install", "--admin", &other, "--binary", &build], None);
    let out = said(&again);
    assert!(again.status.success(), "{out}");
    assert!(
        out.contains("would run: groupadd --system rovik-admin"),
        "{out}"
    );
    assert!(
        out.contains(&format!("would run: usermod -a -G rovik-admin {other}")),
        "{out}"
    );
    assert!(out.contains("would run: systemctl daemon-reload"), "{out}");
    assert!(
        out.contains(&format!(
            "would run: systemctl enable --now rovik-agentd@{other}.service"
        )),
        "{out}"
    );
    let settings = server.text("/etc/rovik/agentd.toml");
    assert!(
        settings.contains(&format!("\nadmins = [\"nobody\", \"{other}\"]\n")),
        "{settings}"
    );
}

#[test]
fn install_refuses_root_and_strangers_as_administrator() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let root = server.run(&["install", "--admin", "root", "--binary", &build], None);
    assert_eq!(root.status.code(), Some(1));
    assert!(
        said(&root).contains("never runs as root"),
        "{}",
        said(&root)
    );
    let stranger = server.run(
        &["install", "--admin", "nosuchuser9", "--binary", &build],
        None,
    );
    assert!(
        said(&stranger).contains("There's no user nosuchuser9"),
        "{}",
        said(&stranger)
    );
    assert!(!server.file("/etc/rovik/agentd.toml").exists());
}

/// H07: `user` commands act on the daemon of the user sudo names, and on
/// no one else's.
#[test]
fn user_commands_act_only_on_the_callers_daemon() {
    let server = Server::installed();
    let start = server.run(&["user", "start"], Some(NOBODY));
    assert!(start.status.success(), "{}", said(&start));
    assert_eq!(
        said(&start).trim(),
        "would run: systemctl enable --now rovik-agentd@nobody.service"
    );
    let (other, uid) = someone_else();
    let theirs = server.run(&["user", "stop"], Some(uid));
    assert_eq!(
        said(&theirs).trim(),
        format!("would run: systemctl disable --now rovik-agentd@{other}.service")
    );
    let named = server.run(&["user", "stop", "nobody"], Some(uid));
    assert_eq!(named.status.code(), Some(1), "no naming someone else");
    let no_sudo = server.run(&["user", "start"], None);
    assert!(
        said(&no_sudo).contains("through sudo"),
        "{}",
        said(&no_sudo)
    );
    let as_root = server.run(&["user", "start"], Some(0));
    assert!(
        said(&as_root).contains("root has no daemon"),
        "{}",
        said(&as_root)
    );

    std::fs::remove_dir_all(server.file("/run/systemd")).unwrap();
    let without = server.run(&["user", "start"], Some(NOBODY));
    assert!(said(&without).contains("no systemd"), "{}", said(&without));
}

/// H07: `admin` commands are refused to a user who isn't an administrator;
/// for one who is, they change the limits and the administrators, upgrade
/// to a release's signed build (B02), or to an unsigned one once those are
/// allowed, and take Rovik off the server.
#[test]
fn admin_commands_are_for_administrators() {
    let server = Server::installed();
    let (other, uid) = someone_else();
    let refused = server.run(&["admin", "limits", "max_slots=2"], Some(uid));
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        said(&refused).contains("Only Rovik's administrators"),
        "{}",
        said(&refused)
    );

    let set = server.run(
        &["admin", "limits", "max_slots=2", "memory_max=2G"],
        Some(NOBODY),
    );
    assert!(set.status.success(), "{}", said(&set));
    assert!(said(&set).contains("would run: systemctl daemon-reload"));
    let settings = server.text("/etc/rovik/agentd.toml");
    assert!(settings.contains("\nmax_slots = 2\n"), "{settings}");
    assert!(settings.contains("\nmemory_max = \"2G\"\n"), "{settings}");
    let limits = server.text("/etc/systemd/system/rovik-agentd@.service.d/limits.conf");
    assert!(limits.contains("\nMemoryMax=2G\n"), "{limits}");
    let bad = server.run(&["admin", "limits", "memory_max=lots"], Some(NOBODY));
    assert_eq!(bad.status.code(), Some(1), "{}", said(&bad));
    let shown = server.run(&["admin", "limits"], Some(NOBODY));
    assert!(said(&shown).contains("max_slots = 2\n"), "{}", said(&shown));

    let added = server.run(&["admin", "add", &other], Some(NOBODY));
    assert!(said(&added).contains(&format!("usermod -a -G rovik-admin {other}")));
    let back = server.run(&["admin", "remove", &other], Some(NOBODY));
    assert!(back.status.success(), "{}", said(&back));
    let last = server.run(&["admin", "remove", "nobody"], Some(NOBODY));
    assert!(
        said(&last).contains("at least one administrator"),
        "{}",
        said(&last)
    );

    // A release's build, signed with the key the binary checks: in, without
    // unsigned builds allowed. The same signature on another build: out.
    let signed = server.dir.0.join("signed");
    std::fs::write(&signed, fixture::BUILD).unwrap();
    std::fs::set_permissions(&signed, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(server.dir.0.join("signed.sig"), fixture::BUILD_SIG).unwrap();
    let signed = signed.to_string_lossy().into_owned();
    let key = Some(fixture::KEY);
    let upgraded = server.run_with_key(&["admin", "upgrade", &signed], Some(NOBODY), key);
    assert!(upgraded.status.success(), "{}", said(&upgraded));
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.8"));
    let without_key = server.run(&["admin", "upgrade", &signed], Some(NOBODY));
    assert!(
        said(&without_key).contains("allow_unsigned"),
        "{}",
        said(&without_key)
    );
    let forged = server.dir.0.join("forged");
    script(&forged, "9.9.7");
    std::fs::write(server.dir.0.join("forged.sig"), fixture::BUILD_SIG).unwrap();
    let forged = forged.to_string_lossy().into_owned();
    let refused = server.run_with_key(&["admin", "upgrade", &forged], Some(NOBODY), key);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        said(&refused).contains("doesn't match"),
        "{}",
        said(&refused)
    );

    let newer = server.dir.0.join("newer");
    script(&newer, "9.9.9");
    let newer = newer.to_string_lossy().into_owned();
    let unsigned = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    assert!(
        said(&unsigned).contains("allow_unsigned"),
        "{}",
        said(&unsigned)
    );
    let path = server.file("/etc/rovik/agentd.toml");
    let settings = std::fs::read_to_string(&path).unwrap() + "allow_unsigned = true\n";
    std::fs::write(&path, settings).unwrap();
    let upgraded = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    assert!(upgraded.status.success(), "{}", said(&upgraded));
    assert!(said(&upgraded).contains("would run: systemctl try-restart rovik-agentd@*.service"));
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.9"));
    assert!(server
        .file(&format!("/opt/rovik/bin/rovik-agentd-{VERSION}"))
        .exists());

    let wants = server.file("/etc/systemd/system/multi-user.target.wants");
    std::fs::create_dir_all(&wants).unwrap();
    std::fs::write(wants.join("rovik-agentd@nobody.service"), "").unwrap();
    let gone = server.run(&["admin", "uninstall"], Some(NOBODY));
    let out = said(&gone);
    assert!(gone.status.success(), "{out}");
    assert!(
        out.contains("would run: systemctl disable --now rovik-agentd@nobody.service"),
        "{out}"
    );
    for path in [
        "/opt/rovik",
        "/etc/rovik",
        "/etc/sudoers.d/rovik",
        "/etc/systemd/system/rovik.slice",
    ] {
        assert!(!server.file(path).exists(), "{path}");
    }
    assert!(!server
        .file("/etc/systemd/system/rovik-agentd@.service")
        .exists());
}
