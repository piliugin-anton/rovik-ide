//! Rovik for everyone on a server (H07), through the real binary with
//! `--root`: the installer's files land under a folder, and each command
//! that would change the system is printed instead of run. `nobody` plays
//! the administrator, and another account a user who isn't one.

use rovik_protocol::release::fixture;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BINARY: &str = env!("CARGO_BIN_EXE_rovik-agentd");
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
    // The build installed is named for the version it says it is, not this
    // installer's.
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-0.1.0"));
    let copy = server.file("/opt/rovik/bin/rovik-agentd-0.1.0");
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
    allow_unsigned(&server);
    let upgraded = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    assert!(upgraded.status.success(), "{}", said(&upgraded));
    assert!(said(&upgraded).contains("would run: systemctl try-restart rovik-agentd@*.service"));
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.9"));
    assert!(server.file("/opt/rovik/bin/rovik-agentd-0.1.0").exists());

    // An older build goes in only when asked to, and never through a link,
    // whose target could be moved while root reads it.
    let older = server.dir.0.join("older");
    script(&older, "9.9.8");
    let older = older.to_string_lossy().into_owned();
    let refused = server.run(&["admin", "upgrade", &older], Some(NOBODY));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(
        said(&refused).contains("older than the installed 9.9.9"),
        "{}",
        said(&refused)
    );
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.9"));
    let linked = server.dir.0.join("linked");
    std::os::unix::fs::symlink(&older, &linked).unwrap();
    let linked = linked.to_string_lossy().into_owned();
    let refused = server.run(
        &["admin", "upgrade", &linked, "--allow-downgrade"],
        Some(NOBODY),
    );
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(said(&refused).contains("is a link"), "{}", said(&refused));
    let downgraded = server.run(
        &["admin", "upgrade", &older, "--allow-downgrade"],
        Some(NOBODY),
    );
    assert!(downgraded.status.success(), "{}", said(&downgraded));
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.8"));
    // Nothing staged stays behind, installed or refused.
    let staged: Vec<String> = std::fs::read_dir(server.file("/opt/rovik/bin"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(staged.is_empty(), "{staged:?}");

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

/// root reads an administrator's build with the administrator's own
/// access: a file they can't read isn't installed, even where unsigned
/// builds are, so root copies nothing of theirs that they couldn't read.
#[test]
fn an_upgrade_reads_the_build_as_its_administrator() {
    // SAFETY: geteuid can't fail.
    if unsafe { libc::geteuid() } != 0 {
        // Only root reads past a file's permissions.
        return;
    }
    let server = Server::installed();
    allow_unsigned(&server);
    let private = server.dir.0.join("private");
    script(&private, "9.9.9");
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let named = private.to_string_lossy().into_owned();
    let refused = server.run(&["admin", "upgrade", &named], Some(NOBODY));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(
        said(&refused).contains("Permission denied"),
        "{}",
        said(&refused)
    );
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-0.1.0"));
    assert!(!server.file("/opt/rovik/bin/rovik-agentd-9.9.9").exists());

    // The same file, readable by them: in.
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();
    let upgraded = server.run(&["admin", "upgrade", &named], Some(NOBODY));
    assert!(upgraded.status.success(), "{}", said(&upgraded));
    let link = std::fs::read_link(server.file("/opt/rovik/bin/rovik-agentd")).unwrap();
    assert_eq!(link, Path::new("rovik-agentd-9.9.9"));
}

/// `--no-browser` leaves the browser tools' browser out, as the app does
/// for a WSL distribution; bubblewrap and the container engine still go in.
#[test]
fn the_browser_can_be_left_out() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let installed = server.run(
        &[
            "install",
            "--admin",
            "nobody",
            "--binary",
            &build,
            "--no-browser",
        ],
        None,
    );
    let out = said(&installed);
    assert!(installed.status.success(), "{out}");
    let browser =
        rovik_core::pinned_browser::Build::for_system("linux", std::env::consts::ARCH).unwrap();
    assert!(!out.contains(&browser.url()), "{out}");
    assert!(!out.contains("<deb.deps>"), "{out}");
    assert!(out.contains("install bubblewrap"), "{out}");
    assert!(out.contains(&compose_download()), "{out}");
}

/// What a dry run says it would download for the projects' services: the
/// pinned Docker Compose for this CPU, checked against its SHA-256.
fn compose_download() -> String {
    use rovik_core::pinned_compose::{Build, VERSION};
    let build = Build::for_cpu(std::env::consts::ARCH).unwrap();
    format!(
        "would download {} (SHA-256 {}) to /opt/rovik/compose/{VERSION}/docker-compose",
        build.url(),
        build.sha256
    )
}

/// D21: install puts in place what runs the projects' services: Podman,
/// with what runs it rootless, the pinned Docker Compose, and subordinate
/// ids for the administrator's containers. `admin containers` tries again
/// for an administrator, and `admin bubblewrap`, which every upgrade runs
/// with the new build, brings it too; ids already given are kept.
#[test]
fn the_container_engine_comes_with_every_install_and_upgrade() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let installed = server.run(&["install", "--admin", "nobody", "--binary", &build], None);
    let out = said(&installed);
    assert!(installed.status.success(), "{out}");
    assert!(
        out.contains(
            "would run: env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 \
             -q -y --no-install-recommends install podman uidmap netavark aardvark-dns passt \
             slirp4netns iptables nftables catatonit skopeo"
        ),
        "{out}"
    );
    assert!(out.contains(&compose_download()), "{out}");
    let ids = "nobody:100000:65536\n";
    assert_eq!(server.text("/etc/subuid"), ids);
    assert_eq!(server.text("/etc/subgid"), ids);

    let (_, uid) = someone_else();
    let refused = server.run(&["admin", "containers"], Some(uid));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    for command in ["containers", "bubblewrap"] {
        let again = server.run(&["admin", command], Some(NOBODY));
        let out = said(&again);
        assert!(again.status.success(), "{out}");
        assert!(out.contains("install podman"), "{out}");
        assert!(out.contains(&compose_download()), "{out}");
    }
    assert_eq!(server.text("/etc/subuid"), ids);
    assert_eq!(server.text("/etc/subgid"), ids);
}

/// D19: install puts the Chrome for Testing Rovik pins in place for the
/// browser tools, checked against its pinned SHA-256, with the packages it
/// needs. `admin browser` does it for an administrator, and an upgrade runs
/// the new build's before the daemons start on it.
#[test]
fn the_pinned_browser_comes_with_rovik_and_its_upgrades() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let installed = server.run(&["install", "--admin", "nobody", "--binary", &build], None);
    let out = said(&installed);
    assert!(installed.status.success(), "{out}");
    let pin =
        rovik_core::pinned_browser::Build::for_system("linux", std::env::consts::ARCH).unwrap();
    assert!(
        out.contains(&format!(
            "would download {} ({} bytes, SHA-256 {})",
            pin.url(),
            pin.size,
            pin.sha256
        )),
        "{out}"
    );
    assert!(
        out.contains(
            "would run: env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 \
             -q -y --no-install-recommends satisfy <deb.deps>"
        ),
        "{out}"
    );
    // And bubblewrap, from the distro's package, which confines the chats'
    // commands (a dry run never finds it installed).
    assert!(
        out.contains(
            "would run: env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 \
             -q -y --no-install-recommends install bubblewrap"
        ),
        "{out}"
    );

    let (_, uid) = someone_else();
    let refused = server.run(&["admin", "browser"], Some(uid));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    let again = server.run(&["admin", "browser"], Some(NOBODY));
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains(&format!("would download {}", pin.url())));
    let again = server.run(&["admin", "bubblewrap"], Some(NOBODY));
    assert!(again.status.success(), "{}", said(&again));
    assert!(
        said(&again).contains("install bubblewrap"),
        "{}",
        said(&again)
    );

    allow_unsigned(&server);
    let newer = server.dir.0.join("newer");
    script(&newer, "9.9.9");
    let newer = newer.to_string_lossy().into_owned();
    let upgraded = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    let out = said(&upgraded);
    assert!(upgraded.status.success(), "{out}");
    let browser = out.find("would run: /opt/rovik/bin/rovik-agentd admin browser");
    let bubblewrap = out.find("would run: /opt/rovik/bin/rovik-agentd admin bubblewrap");
    let restart = out.find("would run: systemctl try-restart");
    assert!(
        matches!((browser, bubblewrap, restart), (Some(b), Some(w), Some(r)) if b < r && w < r),
        "{out}"
    );
}

/// D20: pasta, which gives each user's commands a network of their own, is
/// required. Install and upgrade put it in place before they change
/// anything, so where it can't be installed nothing has changed; `admin
/// pasta` tries again, and so does `admin bubblewrap`, which an upgrade from
/// an older build runs.
#[test]
fn pasta_goes_in_first_on_install_and_upgrade() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let installed = server.run(&["install", "--admin", "nobody", "--binary", &build], None);
    let out = said(&installed);
    assert!(installed.status.success(), "{out}");
    let pasta = out.find("--no-install-recommends install passt");
    let first_change = out.find("would run: groupadd");
    assert!(
        matches!((pasta, first_change), (Some(p), Some(c)) if p < c),
        "{out}"
    );

    for command in ["pasta", "bubblewrap"] {
        let again = server.run(&["admin", command], Some(NOBODY));
        assert!(again.status.success(), "{}", said(&again));
        assert!(said(&again).contains("install passt"), "{}", said(&again));
    }

    allow_unsigned(&server);
    let newer = server.dir.0.join("newer");
    script(&newer, "9.9.9");
    let newer = newer.to_string_lossy().into_owned();
    let upgraded = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    let out = said(&upgraded);
    assert!(upgraded.status.success(), "{out}");
    let pasta = out.find("install passt");
    let new_build = out.find("would run: /opt/rovik/bin/rovik-agentd admin browser");
    assert!(
        matches!((pasta, new_build), (Some(p), Some(b)) if p < b),
        "{out}"
    );
}

/// `agentd.toml` with unsigned builds allowed, for a test's own build. A
/// top-level key goes before `[toolchains]`, the file's only table.
fn allow_unsigned(server: &Server) {
    let path = server.file("/etc/rovik/agentd.toml");
    let settings = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("allow_unsigned = true\n{settings}")).unwrap();
}

/// mode of a file or folder the install made.
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// The shared toolchains and caches: `install` makes the `rovik` group
/// first (systemd won't start a unit naming a group that isn't there),
/// then the folders, then the unit that runs each daemon in the group.
#[test]
fn install_puts_the_shared_toolchains_and_caches_in_place() {
    let server = Server::new();
    let build = server.build.to_string_lossy().into_owned();
    let done = server.run(&["install", "--admin", "nobody", "--binary", &build], None);
    let out = said(&done);
    assert!(done.status.success(), "{out}");

    let admin_group = out.find("would run: groupadd --system rovik-admin");
    let shared_group = out.find("would run: groupadd --system rovik\n");
    let reload = out.find("would run: systemctl daemon-reload");
    assert!(
        matches!((admin_group, shared_group, reload), (Some(a), Some(s), Some(r)) if a < s && s < r),
        "{out}"
    );
    let toolchains = server.file("/opt/rovik/toolchains");
    assert_eq!(mode(&toolchains), 0o755, "the versions' root");
    for cache in ["/var/cache/rovik/npm", "/var/cache/rovik/crates"] {
        let path = server.file(cache);
        assert!(path.is_dir(), "{cache}");
        assert_eq!(mode(&path), 0o2770, "{cache}");
        assert!(
            out.contains(&format!("would run: chown root:rovik {cache}")),
            "{out}"
        );
    }
    // The image cache is root's to fill and the daemons' to read.
    let images = server.file("/var/cache/rovik/images");
    assert_eq!(mode(&images), 0o755, "the image cache");
    assert!(
        !out.contains("chown root:rovik /var/cache/rovik/images"),
        "{out}"
    );
    let unit = server.text("/etc/systemd/system/rovik-agentd@.service");
    assert!(unit.contains("\nSupplementaryGroups=rovik\n"), "{unit}");

    // Nothing there once it's all in place: a second install makes no
    // folder (a dry run can't ask getent whether the group is there, so it
    // says it would make it again).
    let again = said(&server.run(&["install", "--admin", "nobody", "--binary", &build], None));
    assert!(!again.contains("chown root:rovik"), "{again}");
}

/// `admin bubblewrap` puts the shared parts in place too, which is how a
/// server installed before them gets both on an upgrade: the old build's
/// upgrade runs the new build's `admin bubblewrap` before the daemons
/// restart.
#[test]
fn an_upgrade_brings_the_shared_parts_with_bubblewrap() {
    let server = Server::installed();
    // As an install before them left it: no group, no folders, a unit
    // naming neither.
    std::fs::remove_dir_all(server.file("/opt/rovik/toolchains")).unwrap();
    std::fs::remove_dir_all(server.file("/var/cache/rovik")).unwrap();
    let unit = "/etc/systemd/system/rovik-agentd@.service";
    std::fs::write(server.file(unit), "Description=an old unit\n").unwrap();

    let (_, uid) = someone_else();
    let refused = server.run(&["admin", "bubblewrap"], Some(uid));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(!server.file("/var/cache/rovik").exists(), "not for others");

    let done = server.run(&["admin", "bubblewrap"], Some(NOBODY));
    let out = said(&done);
    assert!(done.status.success(), "{out}");
    assert!(
        out.contains("would run: groupadd --system rovik\n"),
        "{out}"
    );
    assert!(server.file("/var/cache/rovik/npm").is_dir(), "{out}");
    let written = server.text(unit);
    assert!(written.contains("SupplementaryGroups=rovik"), "{written}");
    assert!(
        out.contains("would run: systemctl daemon-reload"),
        "{out}"
    );
}

/// A settings change rewrites the unit, which names the `rovik` group: the
/// group is made first, for a server upgraded from a build that never made
/// it (systemd won't start a unit naming a group that isn't there).
#[test]
fn a_settings_change_makes_the_group_its_unit_names() {
    let server = Server::installed();
    let unit = "/etc/systemd/system/rovik-agentd@.service";
    std::fs::write(server.file(unit), "Description=an old unit\n").unwrap();
    let done = server.run(&["admin", "limits", "max_slots=2"], Some(NOBODY));
    let out = said(&done);
    assert!(done.status.success(), "{out}");
    assert!(out.contains("would run: groupadd --system rovik\n"), "{out}");
    assert!(server.text(unit).contains("SupplementaryGroups=rovik"));
}

/// An upgrade puts the shared parts in place with the new build's own
/// command, so the old build never writes its unit back over the new one.
#[test]
fn an_upgrade_sets_up_the_shared_parts_with_the_new_build() {
    let server = Server::installed();
    let newer = server.dir.0.join("newer");
    script(&newer, "9.9.9");
    let newer = newer.to_string_lossy().into_owned();
    allow_unsigned(&server);
    let upgraded = server.run(&["admin", "upgrade", &newer], Some(NOBODY));
    let out = said(&upgraded);
    assert!(upgraded.status.success(), "{out}");
    assert!(
        out.contains("would run: /opt/rovik/bin/rovik-agentd admin toolchains setup"),
        "{out}"
    );
}

/// `user toolchain`: a chat's project names a version, and root installs
/// it once for the whole server, through the sudo rule every user has.
/// The words root gets are checked: only a version of the shape its kind
/// has.
/// A Rust version that's there already, without a component or target the
/// project names: only those are added, the version left as it is.
#[test]
fn an_installed_rust_gets_the_components_and_targets_it_lacks() {
    let server = Server::installed();
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => return,
    };
    let rustlib = server.file(&format!(
        "/opt/rovik/toolchains/rust/rustup/toolchains/1.82-{triple}/lib/rustlib"
    ));
    std::fs::create_dir_all(rustlib.join(triple)).unwrap();
    std::fs::write(
        rustlib.join("components"),
        format!("cargo-{triple}\nrustc-{triple}\nrustfmt-preview-{triple}\n"),
    )
    .unwrap();
    let rust = |words: &[&str]| {
        let mut args = vec!["user", "toolchain", "rust", "1.82"];
        args.extend_from_slice(words);
        said(&server.run(&args, Some(NOBODY)))
    };
    let out = rust(&["-c", "rustfmt", "-c", "clippy", "-t", "wasm32-unknown-unknown", "-t", triple]);
    assert!(!out.contains("toolchain install"), "the version stays: {out}");
    assert!(
        out.contains("rustup component add --toolchain 1.82 clippy\n"),
        "only what's missing: {out}"
    );
    assert!(
        out.contains("rustup target add --toolchain 1.82 wasm32-unknown-unknown\n"),
        "{out}"
    );
    assert!(out.contains("is installed, now with clippy, wasm32-unknown-unknown"), "{out}");
    // All there: nothing runs.
    let out = rust(&["-c", "rustfmt", "-t", triple]);
    assert!(!out.contains("would run"), "{out}");
    assert!(out.contains("Rust 1.82 is installed."), "{out}");
}

#[test]
fn a_user_installs_a_toolchain_as_root() {
    let server = Server::installed();
    let rust = said(&server.run(
        &["user", "toolchain", "rust", "1.82", "-c", "rustfmt"],
        Some(NOBODY),
    ));
    assert!(
        rust.contains("would download https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init")
            || rust.contains("aarch64-unknown-linux-gnu/rustup-init"),
        "{rust}"
    );
    assert!(
        rust.contains(
            "RUSTUP_HOME=/opt/rovik/toolchains/rust/rustup \
             CARGO_HOME=/opt/rovik/toolchains/rust/cargo"
        ),
        "{rust}"
    );
    assert!(
        rust.contains("--no-modify-path -y --profile minimal --default-toolchain stable"),
        "{rust}"
    );
    assert!(
        rust.contains("rustup toolchain install 1.82 --profile minimal -c rustfmt"),
        "{rust}"
    );

    // A host without systemd installs one too: the daemon's own is the
    // app's business, not the toolchain's.
    std::fs::remove_dir_all(server.file("/run/systemd")).unwrap();
    let again = server.run(&["user", "toolchain", "node", "20"], Some(NOBODY));
    let out = said(&again);
    assert!(again.status.success(), "{out}");
    assert!(
        out.contains("would download https://nodejs.org/dist/v20.11.0/node-v20.11.0-linux-")
            && out.contains("/SHASUMS256.txt)"),
        "{out}"
    );
    // A version isn't any text at all.
    for bad in ["1.82; rm -rf /", "../x", "", "1.82.0.1"] {
        let refused = server.run(&["user", "toolchain", "rust", bad], Some(NOBODY));
        assert_eq!(refused.status.code(), Some(1), "{bad}");
        assert!(
            said(&refused).contains("isn't a rust version Rovik installs"),
            "{bad}: {}",
            said(&refused)
        );
    }
    let unknown = server.run(&["user", "toolchain", "python", "3"], Some(NOBODY));
    assert!(said(&unknown).contains("say rust or node"), "{}", said(&unknown));
}

/// D21: a user's daemon has an image of a public registry fetched into the
/// server's image cache as root, by skopeo, checked words only. One fetched
/// lately and held there isn't fetched again. An image of another
/// registry, words that aren't a reference, or the shared caches turned
/// off: refused, and the daemon pulls the image itself.
#[test]
fn a_user_has_an_image_fetched_into_the_cache_as_root() {
    let server = Server::installed();
    let (_, uid) = someone_else();
    let fetched = server.run(&["user", "image", "redis:7-alpine"], Some(uid));
    let out = said(&fetched);
    assert!(fetched.status.success(), "{out}");
    assert!(
        out.contains(
            "would run: skopeo --command-timeout 15m copy --quiet --retry-times 2 \
             docker://docker.io/library/redis:7-alpine \
             oci:/var/cache/rovik/images:docker.io/library/redis:7-alpine\n"
        ),
        "{out}"
    );
    let asked = server.text("/var/cache/rovik/images/asked.json");
    assert!(
        asked.contains("\"docker.io/library/redis:7-alpine\""),
        "{asked}"
    );

    // Held, as skopeo leaves it: not fetched again.
    let index = r#"{"schemaVersion": 2, "manifests": [{"digest": "sha256:00",
        "annotations": {"org.opencontainers.image.ref.name": "docker.io/library/redis:7-alpine"}}]}"#;
    std::fs::write(server.file("/var/cache/rovik/images/index.json"), index).unwrap();
    let again = server.run(
        &["user", "image", "docker.io/library/redis:7-alpine"],
        Some(uid),
    );
    let out = said(&again);
    assert!(again.status.success(), "{out}");
    assert!(!out.contains("would run"), "{out}");
    assert!(out.contains("is in the server's image cache"), "{out}");

    for (words, said_why) in [
        (
            "registry.example.com/team/db:1",
            "isn't an image the server caches",
        ),
        (
            "redis:7 --src-creds=me:secret",
            "isn't an image's reference",
        ),
        ("redis:7;id", "isn't an image's reference"),
    ] {
        let refused = server.run(&["user", "image", words], Some(uid));
        assert_eq!(refused.status.code(), Some(1), "{words}");
        assert!(
            said(&refused).contains(said_why),
            "{words}: {}",
            said(&refused)
        );
    }
    // An option of skopeo's doesn't get past the command line.
    let option = server.run(&["user", "image", "--src-creds=me:secret"], Some(uid));
    assert!(!option.status.success(), "{}", said(&option));
    assert!(!said(&option).contains("would run"), "{}", said(&option));
    let off = server.run(
        &["admin", "toolchains", "shared-caches", "off"],
        Some(NOBODY),
    );
    assert!(off.status.success(), "{}", said(&off));
    let refused = server.run(&["user", "image", "postgres:16"], Some(uid));
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        said(&refused).contains("turned the shared caches off"),
        "{}",
        said(&refused)
    );
}

/// `admin images list` says what the cache holds; `admin images prune
/// DAYS` takes what no daemon asked for in that many days. Both are the
/// administrators'.
#[test]
fn administrators_list_and_prune_the_image_cache() {
    let server = Server::installed();
    let (_, uid) = someone_else();
    let empty = said(&server.run(&["admin", "images", "list"], Some(NOBODY)));
    assert!(empty.contains("The image cache is empty"), "{empty}");
    let index = r#"{"schemaVersion": 2, "manifests": [{"digest": "sha256:00",
        "annotations": {"org.opencontainers.image.ref.name": "docker.io/library/redis:7-alpine"}}]}"#;
    std::fs::write(server.file("/var/cache/rovik/images/index.json"), index).unwrap();
    let listed = said(&server.run(&["admin", "images", "list"], Some(NOBODY)));
    assert!(
        listed.contains("docker.io/library/redis:7-alpine: 0 MB, last asked for never"),
        "{listed}"
    );
    let refused = server.run(&["admin", "images", "prune", "30"], Some(uid));
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    let pruned = server.run(&["admin", "images", "prune", "30"], Some(NOBODY));
    assert!(pruned.status.success(), "{}", said(&pruned));
    assert!(
        said(&pruned).contains("1 image(s) gone"),
        "{}",
        said(&pruned)
    );
    let listed = said(&server.run(&["admin", "images", "list"], Some(NOBODY)));
    assert!(listed.contains("The image cache is empty"), "{listed}");
}

/// An administrator can turn a chat's installs off, and name the Rust
/// versions one may install; their own `admin toolchain add` works either
/// way.
#[test]
fn a_chat_installs_a_toolchain_only_where_administrators_allow_it() {
    let server = Server::installed();
    let path = server.file("/etc/rovik/agentd.toml");
    let settings = std::fs::read_to_string(&path).unwrap();
    assert!(settings.contains("\n[toolchains]\n"), "{settings}");

    // Rust 1.83 isn't one of the versions the administrators allowed.
    let limited = settings.replace(
        "shared_caches = true\n",
        "shared_caches = true\nallowed_rust = [\"1.82\"]\n",
    );
    std::fs::write(&path, limited).unwrap();
    let refused = server.run(&["user", "toolchain", "rust", "1.83"], Some(NOBODY));
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        said(&refused).contains("allow Rust 1.82 only"),
        "{}",
        said(&refused)
    );
    let allowed = server.run(&["user", "toolchain", "rust", "1.82"], Some(NOBODY));
    assert!(allowed.status.success(), "{}", said(&allowed));

    // Off altogether: the message names what an administrator runs.
    let off = std::fs::read_to_string(&path)
        .unwrap()
        .replace("user_install = true", "user_install = false");
    std::fs::write(&path, off).unwrap();
    let refused = server.run(&["user", "toolchain", "rust", "1.82"], Some(NOBODY));
    assert_eq!(refused.status.code(), Some(1));
    let message = said(&refused);
    assert!(
        message.contains("turned off installing toolchains"),
        "{message}"
    );
    assert!(
        message.contains("admin toolchain add rust 1.82"),
        "the admin command: {message}"
    );
    // An administrator's own add still works.
    let added = server.run(&["admin", "toolchain", "add", "rust", "1.82"], Some(NOBODY));
    assert!(added.status.success(), "{}", said(&added));
}

/// `admin toolchain list|add|remove|prune`, for Rovik's administrators
/// only.
#[test]
fn administrators_manage_the_shared_toolchains() {
    let server = Server::installed();
    let (_, uid) = someone_else();
    let commands: [&[&str]; 6] = [
        &["admin", "toolchain", "list"],
        &["admin", "toolchain", "add", "node", "20"],
        &["admin", "toolchain", "remove", "node", "20"],
        &["admin", "toolchain", "prune", "30"],
        &["admin", "toolchains", "user-install", "off"],
        &["admin", "toolchains", "setup"],
    ];
    for args in commands {
        let refused = server.run(args, Some(uid));
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(
            said(&refused).contains("Only Rovik's administrators"),
            "{args:?}: {}",
            said(&refused)
        );
    }

    let added = server.run(&["admin", "toolchain", "add", "node", "20"], Some(NOBODY));
    let out = said(&added);
    assert!(added.status.success(), "{out}");
    assert!(
        out.contains("would download https://nodejs.org/dist/v20.11.0/"),
        "{out}"
    );
    // A real install leaves the folder: this is what the file says.
    let version = server.file("/opt/rovik/toolchains/node/20.11.0");
    std::fs::create_dir_all(version.join("bin")).unwrap();
    std::fs::write(version.join("bin/node"), "").unwrap();
    let listed = said(&server.run(&["admin", "toolchain", "list"], Some(NOBODY)));
    assert!(listed.contains("node: 20.11.0"), "{listed}");
    assert!(listed.contains("rust: none"), "{listed}");

    // `node 20` names the newest 20.x.y there.
    let removed = server.run(&["admin", "toolchain", "remove", "node", "20"], Some(NOBODY));
    assert!(removed.status.success(), "{}", said(&removed));
    assert!(said(&removed).contains("Node 20.11.0 is gone"), "{}", said(&removed));
    assert!(!version.exists());
    let listed = said(&server.run(&["admin", "toolchain", "list"], Some(NOBODY)));
    assert!(!listed.contains("20.11.0"), "{listed}");

    // What nothing used for the days it's given goes; what a chat used
    // stays.
    for (name, days_ago) in [("20.11.0", 30), ("22.0.0", 0)] {
        let dir = server.file(&format!("/opt/rovik/toolchains/node/{name}"));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/node"), "").unwrap();
        let used = server.file(&format!("/opt/rovik/toolchains/node/{name}.used"));
        std::fs::write(&used, "0\n").unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400);
        std::fs::File::options()
            .write(true)
            .open(&used)
            .unwrap()
            .set_modified(when)
            .unwrap();
        // Node itself last ran then too: prune reads its access time.
        std::fs::File::options()
            .write(true)
            .open(dir.join("bin/node"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(when).set_modified(when))
            .unwrap();
    }
    let pruned = server.run(&["admin", "toolchain", "prune", "7"], Some(NOBODY));
    assert!(pruned.status.success(), "{}", said(&pruned));
    assert!(said(&pruned).contains("1 toolchain(s) gone"), "{}", said(&pruned));
    assert!(!server.file("/opt/rovik/toolchains/node/20.11.0").exists());
    assert!(server.file("/opt/rovik/toolchains/node/22.0.0").is_dir());

    // The settings, written to `agentd.toml` as the daemons read them.
    let off = server.run(&["admin", "toolchains", "user-install", "off"], Some(NOBODY));
    assert!(off.status.success(), "{}", said(&off));
    let settings = server.text("/etc/rovik/agentd.toml");
    assert!(settings.contains("user_install = false\n"), "{settings}");
    let on = server.run(&["admin", "toolchains", "shared-caches", "off"], Some(NOBODY));
    assert!(on.status.success(), "{}", said(&on));
    let settings = server.text("/etc/rovik/agentd.toml");
    assert!(settings.contains("shared_caches = false\n"), "{settings}");
    let wrong = server.run(&["admin", "toolchains", "caches", "off"], Some(NOBODY));
    assert_eq!(wrong.status.code(), Some(1));
    let message = said(&wrong);
    assert!(message.contains("admin takes:"), "{message}");
}

/// `admin uninstall` takes the shared caches too, and both groups.
#[test]
fn uninstall_takes_the_shared_caches_and_both_groups() {
    let server = Server::installed();
    assert!(server.file("/var/cache/rovik/npm").is_dir());
    assert!(server.file("/opt/rovik/toolchains").is_dir());
    let gone = server.run(&["admin", "uninstall"], Some(NOBODY));
    let out = said(&gone);
    assert!(gone.status.success(), "{out}");
    assert!(!server.file("/var/cache/rovik").exists(), "{out}");
    assert!(!server.file("/opt/rovik/toolchains").exists(), "{out}");
    assert!(out.contains("would run: groupdel rovik-admin"), "{out}");
    assert!(out.contains("would run: groupdel rovik\n"), "{out}");
}
