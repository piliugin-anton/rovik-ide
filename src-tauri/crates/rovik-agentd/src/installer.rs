//! Rovik on a server for everyone (5.3, D17, H07), and the commands sudo
//! lets people run afterwards.
//!
//! `install --admin USER`, as root: the first deploy runs it through sudo,
//! or shows it for someone with root to run. It copies the binary to
//! `/opt/rovik/bin/rovik-agentd-<version>`, with `rovik-agentd` a link to
//! it; writes `/etc/rovik/agentd.toml` naming USER administrator, the
//! limits sized to the machine (5.5); writes the system unit
//! `rovik-agentd@.service`, its limits drop-in and `rovik.slice`; puts the
//! administrators in the `rovik-admin` group; installs the sudoers rule once
//! `visudo` takes it; and enables USER's daemon. Run again, it installs the
//! binary it's given and rewrites the units, keeping the settings.
//!
//! `user start|stop|restart|status`, through sudo, which lets everyone run
//! them: they act on the daemon of the user sudo runs them for
//! (`SUDO_UID`), and on no one else's.
//!
//! `admin limits|add|remove|browser|upgrade|uninstall`, through sudo, which
//! lets the `rovik-admin` group run them; the caller must also be one of
//! the administrators `agentd.toml` names. `browser` installs the Chrome
//! for Testing Rovik pins for the browser tools (`installer/browser.rs`),
//! as `install` does too, and `upgrade` with the new build.
//!
//! Without systemd, as in a container, the binary and the settings are
//! installed, and the app starts the daemon when it connects. `--root DIR`
//! puts `/` at DIR, and prints each command that would change the system
//! instead of running it: for tests, and for a look before the real thing.

use crate::cli::VERSION;
use crate::home::{self, Passwd};
use crate::limits::{check_user_name, AdminFile, ADMIN_FILE};
use rovik_protocol::release;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use toolchains::Kind;

mod browser;
mod bubblewrap;
mod claude_code;
mod packages;
/// `user toolchain <kind> <version>` and `admin toolchain …`: the shared
/// Rust and Node versions, and the caches their packages download into.
pub(crate) mod toolchains;

pub const BIN_DIR: &str = "/opt/rovik/bin";
/// The installed binary, a link to the current version.
pub const INSTALLED: &str = "/opt/rovik/bin/rovik-agentd";
pub const UNIT: &str = "/etc/systemd/system/rovik-agentd@.service";
pub const DROP_IN: &str = "/etc/systemd/system/rovik-agentd@.service.d/limits.conf";
pub const SLICE: &str = "/etc/systemd/system/rovik.slice";
pub const SUDOERS: &str = "/etc/sudoers.d/rovik";
/// Who may run the admin commands through sudo.
pub const ADMIN_GROUP: &str = "rovik-admin";
/// The Rust and Node versions every user's chats run, one copy of each,
/// owned by root and read-only to them (installer/toolchains.rs).
pub const TOOLCHAINS: &str = "/opt/rovik/toolchains";
/// The package caches the daemons share: npm's, and the crates each has
/// checked against its `Cargo.lock`.
pub const CACHES: &str = "/var/cache/rovik";
/// The group the daemons' users share, which may write those caches.
pub const SHARE_GROUP: &str = "rovik";
/// Where systemd keeps the instances that start at boot.
const WANTS: &str = "/etc/systemd/system/multi-user.target.wants";

/// The system unit, one instance per user (5.3).
pub fn unit() -> String {
    format!(
        "\
[Unit]
Description=Rovik agent daemon for %i

[Service]
User=%i
# The shared caches the daemons write (/var/cache/rovik, installer/
# toolchains.rs). The group is made before the unit is written: systemd
# won't start a unit naming one that isn't there.
SupplementaryGroups=rovik
ExecStart={INSTALLED} serve
Slice=rovik.slice
PrivateTmp=yes
# A command killed for using too much memory doesn't stop the daemon,
# and the daemon is the last process picked: it sets its children's
# score back to 0, which needs no privilege.
OOMPolicy=continue
OOMScoreAdjust=-500
# Stopping, the daemon lets the commands it runs end and pauses its runs
# for its next start, for up to 80 s: only it hears SIGTERM, and what's
# left is killed after 90 s.
KillMode=mixed
TimeoutStopSec=90
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"
    )
}

/// Each daemon's caps, from `agentd.toml`.
pub fn drop_in(file: &AdminFile) -> String {
    let mut text = String::from(
        "# From /etc/rovik/agentd.toml: change it with `rovik-agentd admin limits`.\n\
         [Service]\n",
    );
    if let Some(memory) = &file.memory_max {
        text.push_str(&format!("MemoryMax={memory}\n"));
    }
    if let Some(tasks) = file.tasks_max {
        text.push_str(&format!("TasksMax={tasks}\n"));
    }
    if let Some(quota) = &file.cpu_quota {
        text.push_str(&format!("CPUQuota={quota}\n"));
    }
    text
}

/// The caps on all of Rovik, from `agentd.toml`.
pub fn slice(file: &AdminFile) -> String {
    let mut text = String::from(
        "# From /etc/rovik/agentd.toml: change it with `rovik-agentd admin limits`.\n\
         [Unit]\n\
         Description=Rovik's agent daemons, together\n\
         \n\
         [Slice]\n",
    );
    if let Some(memory) = &file.total_memory_max {
        text.push_str(&format!("MemoryMax={memory}\n"));
    }
    if let Some(quota) = &file.total_cpu_quota {
        text.push_str(&format!("CPUQuota={quota}\n"));
    }
    text
}

/// Everyone runs the `user` commands, the administrators the `admin` ones,
/// without a password; the binary itself says who may do what.
pub fn sudoers() -> String {
    format!(
        "\
# Rovik (rovik-agentd install): everyone starts and stops their own daemon,
# and Rovik's administrators run its admin commands.
ALL ALL=(root) NOPASSWD: {INSTALLED} user *
%{ADMIN_GROUP} ALL=(root) NOPASSWD: {INSTALLED} admin *
"
    )
}

/// A user's instance of the unit.
pub fn instance(user: &str) -> String {
    format!("rovik-agentd@{user}.service")
}

/// The system the commands change: `/`, or a folder in its place.
pub struct System {
    root: PathBuf,
}

impl System {
    pub fn new(root: Option<PathBuf>) -> System {
        let root = root.unwrap_or_else(|| PathBuf::from("/"));
        // As the file system has it: `/..` is `/`, and live.
        let root = root.canonicalize().unwrap_or(root);
        System { root }
    }

    fn live(&self) -> bool {
        self.root == Path::new("/")
    }

    /// `absolute` under the root.
    fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    /// Whether systemd runs the system.
    fn systemd(&self) -> bool {
        self.path("/run/systemd/system").is_dir()
    }

    /// Runs a command that changes the system; under another root, says it.
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        self.run_with(&[], program, args)
    }

    /// `run`, with `env` set on the command (rustup's homes,
    /// installer/toolchains.rs); under another root, says it with them.
    fn run_with(
        &self,
        env: &[(&str, &str)],
        program: &str,
        args: &[&str],
    ) -> Result<String, String> {
        let shown = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .chain(std::iter::once(program.to_string()))
            .chain(args.iter().map(|a| a.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        if !self.live() {
            println!("would run: {shown}");
            return Ok(String::new());
        }
        let mut command = Command::new(program);
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command
            .args(args)
            .output()
            .map_err(|e| format!("{shown}: {e}"))?;
        match out.status.success() {
            true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            false => Err(format!(
                "{shown}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// The first of `programs` on the PATH; under another root, the first.
    fn which<'a>(&self, programs: &[&'a str]) -> Option<&'a str> {
        if !self.live() {
            return programs.first().copied();
        }
        programs.iter().copied().find(|p| on_path(p))
    }

    /// Whether `program` is on the PATH; under another root, never, so a
    /// dry run shows what installing it would run.
    fn has(&self, program: &str) -> bool {
        self.live() && on_path(program)
    }

    /// Writes a file all at once: a new one beside it, renamed over it.
    /// The new one is root's alone until it's whole and has its `mode`.
    fn write(&self, absolute: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
        use std::io::Write;
        let (temp, path) = self.beside(absolute)?;
        let written = private_file(&temp)
            .and_then(|mut file| file.write_all(bytes))
            .and_then(|()| std::fs::set_permissions(&temp, PermissionsExt::from_mode(mode)))
            .and_then(|()| std::fs::rename(&temp, &path));
        written.map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("{}: {e}", path.display())
        })
    }

    /// Points the link `absolute` at `target`, the same way.
    fn link(&self, absolute: &str, target: &str) -> Result<(), String> {
        let (temp, path) = self.beside(absolute)?;
        let _ = std::fs::remove_file(&temp);
        std::os::unix::fs::symlink(target, &temp)
            .and_then(|()| std::fs::rename(&temp, &path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The new file's name beside `absolute`, its folder made, and the
    /// path. The name has a dot, which keeps sudo from reading a sudoers
    /// file before it's whole.
    fn beside(&self, absolute: &str) -> Result<(PathBuf, PathBuf), String> {
        let path = self.path(absolute);
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(format!("{absolute} isn't a file's path"));
        };
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let temp = dir.join(format!(".{}.new", name.to_string_lossy()));
        Ok((temp, path))
    }

    fn remove(&self, absolute: &str) -> Result<(), String> {
        let path = self.path(absolute);
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        removed.map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `install --admin ADMIN`: see the module's comment. `binary` is the file
/// to install, this one unless told.
pub fn install(
    sys: &System,
    admin: &str,
    binary: Option<PathBuf>,
    browser: bool,
) -> Result<(), String> {
    if sys.live() && euid() != 0 {
        return Err(format!(
            "install changes the system for everyone: run it as root, as in \
             sudo {} install --admin {admin}",
            this_binary()
        ));
    }
    let account = regular_user(admin)?;
    // Another build is named for the version it says it is, asked as the
    // administrator: this one is VERSION.
    let (bytes, version) = match binary {
        Some(file) => {
            let bytes = read_build(&file, None)?;
            let version = version_of(&bytes, &file, &account)?;
            (bytes, version)
        }
        None => {
            let this = std::env::current_exe().map_err(|e| e.to_string())?;
            (read_build(&this, None)?, VERSION.to_string())
        }
    };
    install_build(sys, &bytes, &version)?;
    let mut file = match AdminFile::read(&sys.path(ADMIN_FILE))? {
        Some(file) => file,
        None => AdminFile::sized_for(&account.name, total_memory()),
    };
    if !file.admins.contains(&account.name) {
        file.admins.push(account.name.clone());
    }
    admin_group(sys, &file.admins)?;
    write_settings(sys, &file)?;
    write_sudoers(sys)?;
    // The shared toolchains and caches, and the unit that puts each
    // daemon in their group: before the daemon starts, which systemd
    // refuses to do with a group that isn't there.
    match toolchains::setup(sys) {
        Ok(()) => {}
        Err(why) => println!("{}", toolchains::not_set_up(&why)),
    }
    if sys.systemd() {
        sys.run("systemctl", &["daemon-reload"])?;
        sys.run("systemctl", &["enable", "--now", &instance(&account.name)])?;
        println!(
            "Rovik {version} is installed, and {}'s daemon runs.",
            account.name
        );
    } else {
        println!(
            "Rovik {version} is installed. There's no systemd here: the app starts each \
             user's daemon when it connects."
        );
    }
    println!("Its administrators: {}.", file.admins.join(", "));
    if browser {
        browser::ensure_or_say(sys);
    }
    bubblewrap::ensure_or_say(sys);
    Ok(())
}

/// `user ACTION`: the caller's own daemon, and the toolchains their chats
/// ask for.
pub fn user(sys: &System, args: &[String]) -> Result<(), String> {
    let caller = caller(sys)?;
    // A toolchain installs on a host without systemd too, so it comes
    // before the check for one.
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if let ["toolchain", kind, version, rest @ ..] = words.as_slice() {
        return user_toolchain(sys, Kind::parse(kind)?, kind, version, rest);
    }
    let [action] = words.as_slice() else {
        return Err(
            "user takes one of start, stop, restart or status, or toolchain KIND VERSION \
             [-c COMPONENT] [-t TARGET]"
                .into(),
        );
    };
    if !sys.systemd() {
        return Err("There's no systemd here: the app starts your daemon when it connects.".into());
    }
    let unit = instance(&caller.name);
    let done = match *action {
        "start" => sys.run("systemctl", &["enable", "--now", &unit]),
        "stop" => sys.run("systemctl", &["disable", "--now", &unit]),
        "restart" => sys.run("systemctl", &["restart", &unit]),
        "status" => {
            // `is-active` fails for a daemon that doesn't run, which is an
            // answer here.
            let state = sys.run("systemctl", &["is-active", &unit]);
            println!("{}", state.unwrap_or_else(|_| "inactive".into()).trim());
            return Ok(());
        }
        other => {
            return Err(format!(
                "user takes one of start, stop, restart or status, not {other}"
            ))
        }
    };
    done.map(|_| ())
}

/// `user toolchain KIND VERSION [-c COMPONENT] [-t TARGET]`, through sudo:
/// the version a chat's project needs, installed as root, once for the
/// whole server. The words are checked here, before root sees them
/// (`toolchains::parse_version`), and the settings say whether a user may
/// ask at all.
fn user_toolchain(
    sys: &System,
    kind: Kind,
    named: &str,
    version: &str,
    rest: &[&str],
) -> Result<(), String> {
    let file = AdminFile::read(&sys.path(ADMIN_FILE))?.unwrap_or_default();
    let (components, targets) = toolchains::flags(rest)?;
    if !file.toolchains.user_install {
        return Err(format!(
            "An administrator turned off installing toolchains from a chat here. To have \
             {named} {version} installed, ask one of them to run: sudo {INSTALLED} admin \
             toolchain add {named} {version}"
        ));
    }
    let allowed = &file.toolchains.allowed_rust;
    if !allowed.is_empty()
        && kind == Kind::Rust
        && !allowed.iter().any(|v| v == version.trim())
    {
        return Err(format!(
            "This server's administrators allow Rust {} only. To have another version, ask \
             one of them to run: sudo {INSTALLED} admin toolchain add {named} {version}",
            allowed.join(", ")
        ));
    }
    toolchains::ensure(sys, kind, version, &components, &targets)
}

/// `admin ...`: the limits, the administrators, upgrades and removal.
/// `allow_downgrade`: an upgrade may install an older build.
pub fn admin(sys: &System, args: &[String], allow_downgrade: bool) -> Result<(), String> {
    let caller = caller(sys)?;
    let mut file = AdminFile::read(&sys.path(ADMIN_FILE))?.ok_or("Rovik isn't installed here.")?;
    if !file.admins.contains(&caller.name) {
        return Err(format!(
            "Only Rovik's administrators here run admin commands: {}.",
            file.admins.join(", ")
        ));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["limits"] => {
            for (key, value) in file.limits() {
                println!("{key} = {value}");
            }
            Ok(())
        }
        ["limits", settings @ ..] => {
            for setting in settings {
                let (key, value) = setting
                    .split_once('=')
                    .ok_or_else(|| format!("{setting}: a limit is set as name=value"))?;
                file.set(key.trim(), value.trim())?;
            }
            write_settings(sys, &file)?;
            if sys.systemd() {
                sys.run("systemctl", &["daemon-reload"])?;
            }
            println!("The new limits apply to each daemon when it next starts.");
            Ok(())
        }
        ["add", user] => {
            let account = regular_user(user)?;
            if !file.admins.contains(&account.name) {
                file.admins.push(account.name);
            }
            write_settings(sys, &file)?;
            admin_group(sys, &file.admins)
        }
        ["remove", user] => {
            if !file.admins.iter().any(|a| a == user) {
                return Err(format!("{user} isn't one of Rovik's administrators."));
            }
            if file.admins.len() == 1 {
                return Err("Rovik keeps at least one administrator: add another first.".into());
            }
            file.admins.retain(|a| a != user);
            write_settings(sys, &file)?;
            leave_group(sys, user)
        }
        ["browser"] => browser::ensure(sys),
        // Claude Code for the connector of that name, from Anthropic's
        // package repository, once for everyone (installer/claude_code.rs).
        ["claude-code"] => claude_code::ensure(sys),
        // The shared toolchains and caches go in whether bubblewrap's
        // package does or not: a server installed before this has no
        // `rovik` group and no folder for them yet, and the daemons that
        // restart after an upgrade must find both.
        ["bubblewrap"] => both(bubblewrap::ensure(sys), toolchains::setup(sys)),
        ["toolchain", "list"] => toolchains::list(sys),
        ["toolchain", "add", kind, version, rest @ ..] => {
            let (components, targets) = toolchains::flags(rest)?;
            toolchains::ensure(sys, Kind::parse(kind)?, version, &components, &targets)
        }
        ["toolchain", "remove", kind, version] => {
            toolchains::remove(sys, Kind::parse(kind)?, version)
        }
        ["toolchain", "prune", days] => {
            let days = days
                .parse::<u64>()
                .map_err(|_| format!("prune takes a number of days, not {days}"))?;
            toolchains::prune(sys, days)
        }
        ["toolchains", setting @ ("user-install" | "shared-caches"), "on" | "off"] => {
            let on = args[2] == "on";
            match *setting {
                "user-install" => file.toolchains.user_install = on,
                _ => file.toolchains.shared_caches = on,
            }
            write_settings(sys, &file)?;
            println!(
                "{} is {}: {}.",
                setting,
                args[2],
                match on {
                    true => "a daemon reads it when it next starts",
                    false => "it applies to daemons that start from now on",
                }
            );
            Ok(())
        }
        ["toolchains", "setup"] => toolchains::setup(sys),
        ["upgrade", from] => upgrade(sys, &file, Path::new(from), &caller, allow_downgrade),
        ["uninstall"] => uninstall(sys),
        _ => Err(
            "admin takes: limits [name=value ...], add USER, remove USER, browser, \
             bubblewrap, claude-code, toolchain list | add KIND VERSION | remove KIND VERSION | prune DAYS, \
             toolchains user-install|shared-caches on|off, toolchains setup, upgrade FILE \
             [--allow-downgrade], uninstall"
                .into(),
        ),
    }
}

/// The `admin bubblewrap` arm's two outcomes as one: the shared parts go in
/// whether or not bubblewrap's package does, since a server installed
/// before them has neither, and both failures are worth saying.
fn both(bubbles: Result<(), String>, shared: Result<(), String>) -> Result<(), String> {
    match (bubbles, shared) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(why)) => Err(toolchains::not_set_up(&why)),
        (Err(why), Ok(())) => Err(why),
        (Err(one), Err(two)) => Err(format!("{one}\n{}", toolchains::not_set_up(&two))),
    }
}

/// `admin upgrade FILE`: the build beside the installed ones, the link
/// moved to it, and the daemons that ran started again. Each one, stopped,
/// pauses its runs at their next safe point and the new binary continues
/// them (R02, 5.4). A release's build comes with its signature beside it,
/// `FILE.sig`, checked with the key this binary was built with, so a new
/// build can't vouch for itself (B02); a build of one's own goes in only
/// where `agentd.toml` allows unsigned builds (B03). That build is what
/// sudo runs as root for everyone afterwards, and the upgrade runs it as
/// root at once (`admin browser`): allowing unsigned builds gives each
/// administrator root on the server.
///
/// FILE is the administrator's own file, read once, with their own access
/// to it: root reads nothing for them that they couldn't read themselves.
/// What was read is what's checked, asked its version and installed, so a
/// file put in its place meanwhile is never the one root installs. Its
/// version is asked of a copy in memory, run as the administrator, never
/// as root, and the copy under `/opt/rovik/bin` is root's alone until it
/// has passed. A build older than the installed one is refused unless the
/// command says `--allow-downgrade`.
fn upgrade(
    sys: &System,
    file: &AdminFile,
    from: &Path,
    caller: &Passwd,
    allow_downgrade: bool,
) -> Result<(), String> {
    let bytes = read_build(from, Some(caller))?;
    match signed(from, &bytes, caller) {
        Ok(true) => {}
        Ok(false) if file.allow_unsigned => {}
        Ok(false) => {
            return Err(format!(
                "{} isn't signed with Rovik's release key. To install a build of your own, \
                 set allow_unsigned = true in {ADMIN_FILE}, which lets each of Rovik's \
                 administrators run their own program as root here.",
                from.display()
            ))
        }
        Err(why) => return Err(format!("{}: {why} It wasn't installed.", from.display())),
    }
    let version = version_of(&bytes, from, caller)?;
    if !allow_downgrade {
        if let Some(installed) = installed_version(sys) {
            if older(&version, &installed) == Some(true) {
                return Err(format!(
                    "Rovik {version} is older than the installed {installed}. To install it \
                     anyway, add --allow-downgrade."
                ));
            }
        }
    }
    install_build(sys, &bytes, &version)?;
    // The new build's browser, before the daemons start on it: its pin may
    // be a newer version. One that can't be installed stops nothing else.
    match sys.run(INSTALLED, &["admin", "browser"]) {
        Ok(said) => print!("{said}"),
        Err(why) => println!("{}", browser::not_installed(&why)),
    }
    // And bubblewrap, which confines the chats' commands, for a server
    // installed before Rovik asked for it. Same: it stops nothing else.
    match sys.run(INSTALLED, &["admin", "bubblewrap"]) {
        Ok(said) => print!("{said}"),
        Err(why) => println!("{}", bubblewrap::not_installed(&why)),
    }
    // The shared toolchains, caches and unit, as the new build has them:
    // this build's own `setup` would write its older unit back over the
    // one the new build's `admin bubblewrap` just wrote.
    match sys.run(INSTALLED, &["admin", "toolchains", "setup"]) {
        Ok(said) => print!("{said}"),
        Err(why) => println!("{}", toolchains::not_set_up(&why)),
    }
    if sys.systemd() {
        sys.run("systemctl", &["try-restart", "rovik-agentd@*.service"])?;
    }
    println!(
        "Rovik {version} is installed. The daemons that ran paused their runs and start again \
         on it."
    );
    Ok(())
}

/// FILE's bytes, read once from the regular file it is. Not through a
/// link, whose target its owner could move meanwhile. With `user`, opened
/// with their ids and groups: what they can read, and nothing else.
fn read_build(from: &Path, user: Option<&Passwd>) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(from)
    };
    let opened = match user {
        Some(user) => as_user(user, open)?,
        None => open(),
    };
    let mut file = opened.map_err(|e| match e.raw_os_error() {
        Some(libc::ELOOP) => format!("{} is a link: name the build itself.", from.display()),
        _ => format!("{}: {e}", from.display()),
    })?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", from.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "{} isn't a regular file: name the build itself.",
            from.display()
        ));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", from.display()))?;
    Ok(bytes)
}

/// The version the installed link names, `rovik-agentd-<version>`, if it
/// does.
fn installed_version(sys: &System) -> Option<String> {
    let target = std::fs::read_link(sys.path(INSTALLED)).ok()?;
    let name = target.file_name()?.to_str()?;
    name.strip_prefix("rovik-agentd-").map(str::to_string)
}

/// Whether `version` comes before `than`, both numbered as releases are
/// (`1.2.3`, or `1.2.3-4` for a pre-release, which comes before `1.2.3`);
/// none when either isn't.
fn older(version: &str, than: &str) -> Option<bool> {
    Some(release_number(version)? < release_number(than)?)
}

/// `(major, minor, patch, pre-release)`: a release itself gets the highest
/// pre-release number, so its pre-releases come before it.
fn release_number(version: &str) -> Option<(u64, u64, u64, u64)> {
    let (numbers, pre) = match version.split_once('-') {
        Some((numbers, pre)) => (numbers, pre.parse::<u64>().ok()?),
        None => (version, u64::MAX),
    };
    let mut parts = numbers.split('.').map(|part| part.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

/// `admin uninstall`: every daemon stopped, and Rovik's files gone. Each
/// user's folders stay: their chats are in them.
fn uninstall(sys: &System) -> Result<(), String> {
    if sys.systemd() {
        let wants = std::fs::read_dir(sys.path(WANTS))
            .into_iter()
            .flatten()
            .flatten();
        for entry in wants {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("rovik-agentd@") && name.ends_with(".service") {
                sys.run("systemctl", &["disable", "--now", &name])?;
            }
        }
    }
    for path in [SUDOERS, UNIT, DROP_IN, SLICE, BIN_DIR, ADMIN_FILE] {
        sys.remove(path)?;
    }
    for dir in [
        "/etc/systemd/system/rovik-agentd@.service.d",
        "/etc/rovik",
        "/opt/rovik",
        CACHES,
    ] {
        sys.remove(dir)?;
    }
    if sys.systemd() {
        sys.run("systemctl", &["daemon-reload"])?;
    }
    if let Some(tool) = sys.which(&["groupdel", "delgroup"]) {
        for group in [ADMIN_GROUP, SHARE_GROUP] {
            let _ = sys.run(tool, &[group]);
        }
    }
    println!(
        "Rovik is gone from this server. Each user's ~/.rovik and ~/.local/share/rovik \
         stay: their chats are there."
    );
    Ok(())
}

/// `bytes`, the build that says it's `version`, as
/// `rovik-agentd-<version>`, and the link moved to it. It's root's alone
/// until it's whole, and then everyone's to run: each user's daemon is it.
fn install_build(sys: &System, bytes: &[u8], version: &str) -> Result<(), String> {
    let name = format!("rovik-agentd-{version}");
    sys.write(&format!("{BIN_DIR}/{name}"), bytes, 0o755)?;
    sys.link(INSTALLED, &name)
}

/// `agentd.toml`, and the units made from it. The unit names the `rovik`
/// group, which is made first: systemd won't start a daemon whose unit
/// names a group that isn't there (a server upgraded from a build that
/// never made it).
fn write_settings(sys: &System, file: &AdminFile) -> Result<(), String> {
    file.check()?;
    sys.write(ADMIN_FILE, file.to_toml().as_bytes(), 0o644)?;
    toolchains::group(sys)?;
    sys.write(UNIT, unit().as_bytes(), 0o644)?;
    sys.write(DROP_IN, drop_in(file).as_bytes(), 0o644)?;
    sys.write(SLICE, slice(file).as_bytes(), 0o644)
}

/// The `rovik-admin` group, with the administrators in it.
fn admin_group(sys: &System, admins: &[String]) -> Result<(), String> {
    let exists = sys.live()
        && Command::new("getent")
            .args(["group", ADMIN_GROUP])
            .output()
            .is_ok_and(|o| o.status.success());
    if !exists {
        match sys.which(&["groupadd", "addgroup"]) {
            Some("groupadd") => sys.run("groupadd", &["--system", ADMIN_GROUP])?,
            Some(tool) => sys.run(tool, &["-S", ADMIN_GROUP])?,
            None => {
                return Err("There's no groupadd or addgroup to make the rovik-admin group.".into())
            }
        };
    }
    for admin in admins {
        match sys.which(&["usermod", "addgroup"]) {
            Some("usermod") => sys.run("usermod", &["-a", "-G", ADMIN_GROUP, admin])?,
            Some(tool) => sys.run(tool, &[admin, ADMIN_GROUP])?,
            None => {
                return Err(format!(
                    "There's no usermod or addgroup to add {admin} to rovik-admin."
                ))
            }
        };
    }
    Ok(())
}

fn leave_group(sys: &System, user: &str) -> Result<(), String> {
    match sys.which(&["gpasswd", "deluser"]) {
        Some("gpasswd") => sys.run("gpasswd", &["-d", user, ADMIN_GROUP]).map(|_| ()),
        Some(tool) => sys.run(tool, &[user, ADMIN_GROUP]).map(|_| ()),
        None => Err(format!(
            "There's no gpasswd or deluser to take {user} out of rovik-admin."
        )),
    }
}

/// The sudoers rule, put in place once `visudo` takes it: a broken file in
/// `/etc/sudoers.d` would lock everyone out of sudo.
fn write_sudoers(sys: &System) -> Result<(), String> {
    let Some(visudo) = ["visudo", "/usr/sbin/visudo", "/sbin/visudo"]
        .into_iter()
        .find(|p| on_path(p) || Path::new(p).is_file())
    else {
        println!(
            "There's no visudo, so no sudoers rule: users need someone with root to start \
             their daemons."
        );
        return Ok(());
    };
    let (temp, path) = sys.beside(SUDOERS)?;
    std::fs::write(&temp, sudoers())
        .and_then(|()| std::fs::set_permissions(&temp, PermissionsExt::from_mode(0o440)))
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    let checked = Command::new(visudo)
        .arg("-c")
        .arg("-q")
        .arg("-f")
        .arg(&temp)
        .output();
    match checked {
        Ok(out) if out.status.success() => {
            std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))
        }
        Ok(out) => {
            let _ = std::fs::remove_file(&temp);
            Err(format!(
                "visudo refused the sudoers rule: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(format!("{visudo}: {e}"))
        }
    }
}

/// The user sudo runs the command for (`SUDO_UID`, which sudo sets and
/// no one else can).
fn caller(sys: &System) -> Result<Passwd, String> {
    let through_sudo = format!("Run it through sudo, as in: sudo {INSTALLED} user start");
    if sys.live() && euid() != 0 {
        return Err(through_sudo);
    }
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|u| u.parse::<u32>().ok())
        .ok_or(through_sudo)?;
    if uid == 0 {
        return Err(
            "root has no daemon: run it through sudo as the user whose daemon it is.".into(),
        );
    }
    let account = home::passwd_of(uid).ok_or(format!("There's no user with id {uid} here."))?;
    check_user_name(&account.name)?;
    Ok(account)
}

/// A user who can have a daemon: one that exists, and isn't root.
fn regular_user(name: &str) -> Result<Passwd, String> {
    check_user_name(name)?;
    let account = home::passwd_named(name).ok_or(format!("There's no user {name} here."))?;
    if account.uid == 0 {
        return Err(
            "Name a regular user: the daemon never runs as root, so root can't be Rovik's \
             administrator."
                .into(),
        );
    }
    Ok(account)
}

/// What the build `bytes` says its version is, asked of a copy in
/// memory, which no one else can open, and run as `user` whenever this
/// process is root: a build that isn't a release's is the administrator's
/// own program. `named` is the file it came from, for the messages.
#[cfg(target_os = "linux")]
fn version_of(bytes: &[u8], named: &Path, user: &Passwd) -> Result<String, String> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let copy = runnable_memory().map_err(|e| {
        let why = match e.raw_os_error() {
            Some(libc::EACCES) => {
                "this system runs no program from memory (vm.memfd_noexec = 2)".to_string()
            }
            _ => e.to_string(),
        };
        format!("{}: its version can't be asked: {why}", named.display())
    })?;
    (&copy)
        .write_all(bytes)
        .map_err(|e| format!("{}: {e}", named.display()))?;
    // The copy's descriptor is the child's too, where /proc/self/fd names
    // it, for the kernel and for a script's interpreter.
    let mut command = Command::new(format!("/proc/self/fd/{}", copy.as_raw_fd()));
    command.arg("version").current_dir("/");
    if euid() == 0 {
        command.uid(user.uid).gid(user.gid);
    }
    let out = command.output();
    drop(copy);
    let out = out.map_err(|e| format!("{}: {e}", named.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text
        .strip_prefix("rovik-agentd ")
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|v| {
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
        })
        .ok_or_else(|| format!("{} isn't a rovik-agentd build.", named.display()))?;
    Ok(version.to_string())
}

/// Servers run Linux (D16): elsewhere, a build's version isn't asked.
#[cfg(not(target_os = "linux"))]
fn version_of(_bytes: &[u8], named: &Path, _user: &Passwd) -> Result<String, String> {
    Err(format!(
        "{}: builds are installed on Linux servers.",
        named.display()
    ))
}

/// A file in memory a program can run from. MFD_EXEC where the kernel
/// knows it (6.3 on), so a system that makes such files unrunnable unless
/// asked (vm.memfd_noexec = 1) runs this one; it isn't closed on exec,
/// so the child that runs it has it.
#[cfg(target_os = "linux")]
fn runnable_memory() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let name = c"rovik-agentd-build";
    // SAFETY: memfd_create takes a NUL-terminated name and flags.
    let mut fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_EXEC) };
    if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
        // SAFETY: as above, on a kernel without MFD_EXEC.
        fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    }
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a new descriptor that nothing else owns.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// `f`, with `user`'s ids and groups as the ones the kernel checks what
/// it opens against, so it opens only what `user` could; root's come back
/// after. Not root (`--root` in a test), `f` runs as this process is.
#[cfg(target_os = "linux")]
fn as_user<T>(user: &Passwd, f: impl FnOnce() -> T) -> Result<T, String> {
    if euid() != 0 {
        return Ok(f());
    }
    let groups = groups_of(user)?;
    let _root = Ids::switch_to(user, &groups)?;
    Ok(f())
}

#[cfg(not(target_os = "linux"))]
fn as_user<T>(_user: &Passwd, f: impl FnOnce() -> T) -> Result<T, String> {
    match euid() {
        0 => Err("Builds are installed on Linux servers.".into()),
        _ => Ok(f()),
    }
}

/// The groups `user` is in, their own first.
#[cfg(target_os = "linux")]
fn groups_of(user: &Passwd) -> Result<Vec<libc::gid_t>, String> {
    let name = std::ffi::CString::new(user.name.as_str()).map_err(|e| e.to_string())?;
    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    loop {
        let mut count = groups.len() as libc::c_int;
        // SAFETY: `groups` has room for `count` ids; the call says how
        // many it needs when that's too few.
        let found =
            unsafe { libc::getgrouplist(name.as_ptr(), user.gid, groups.as_mut_ptr(), &mut count) };
        if found >= 0 {
            groups.truncate(count.max(0) as usize);
            return Ok(groups);
        }
        if count as usize <= groups.len() {
            return Err(format!("{}'s groups can't be read.", user.name));
        }
        groups.resize(count as usize, 0);
    }
}

/// Root's effective group and groups, put back with root's effective id
/// when dropped: the saved set-user-ID stays 0, so root can come back.
#[cfg(target_os = "linux")]
struct Ids {
    gid: libc::gid_t,
    groups: Vec<libc::gid_t>,
}

#[cfg(target_os = "linux")]
impl Ids {
    fn switch_to(user: &Passwd, groups: &[libc::gid_t]) -> Result<Ids, String> {
        // SAFETY: getgroups with 0 says how many there are, then fills a
        // buffer that size.
        let mut saved: Vec<libc::gid_t> =
            vec![0; unsafe { libc::getgroups(0, std::ptr::null_mut()) }.max(0) as usize];
        let count = unsafe { libc::getgroups(saved.len() as libc::c_int, saved.as_mut_ptr()) };
        if count < 0 {
            return Err(format!(
                "root's groups: {}",
                std::io::Error::last_os_error()
            ));
        }
        saved.truncate(count as usize);
        let root = Ids {
            // SAFETY: getegid can't fail.
            gid: unsafe { libc::getegid() },
            groups: saved,
        };
        // The groups and the group first: once the user's id is in
        // effect, they can't be changed. A step that fails puts back
        // what came before it, when `root` drops.
        // SAFETY: plain system calls on this process's ids.
        let switched = unsafe {
            libc::setgroups(groups.len(), groups.as_ptr()) == 0
                && libc::setegid(user.gid) == 0
                && libc::seteuid(user.uid) == 0
        };
        match switched {
            true => Ok(root),
            false => Err(format!(
                "{}'s ids couldn't be taken on: {}",
                user.name,
                std::io::Error::last_os_error()
            )),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Ids {
    fn drop(&mut self) {
        // SAFETY: plain system calls on this process's ids.
        let back = unsafe {
            libc::seteuid(0) == 0
                && libc::setegid(self.gid) == 0
                && libc::setgroups(self.groups.len(), self.groups.as_ptr()) == 0
        };
        if !back {
            // Going on with someone else's ids would do root's work as them.
            eprintln!(
                "rovik-agentd: root's ids couldn't be put back: {}",
                std::io::Error::last_os_error()
            );
            std::process::abort();
        }
    }
}

/// A new file at `path` that only its owner, root, can read, in place of
/// any left there before.
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Whether `from`, read as `bytes`, comes with a signature, `<from>.sig`,
/// that this binary's release key checks; an error when it has one that
/// doesn't. Without a key, nothing is taken as signed. The signature is
/// read with `user`'s access, as the build was.
fn signed(from: &Path, bytes: &[u8], user: &Passwd) -> Result<bool, String> {
    let mut signature = from.as_os_str().to_owned();
    signature.push(release::SIGNATURE);
    let read = as_user(user, || std::fs::read_to_string(PathBuf::from(signature)))?;
    let Ok(signature) = read else {
        return Ok(false);
    };
    let Some(key) = release::key() else {
        return Ok(false);
    };
    release::verify(&key, bytes, &signature).map(|()| true)
}

fn euid() -> u32 {
    // SAFETY: geteuid can't fail.
    unsafe { libc::geteuid() }
}

fn this_binary() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "rovik-agentd".into())
}

/// Whether this process runs the installed binary, `INSTALLED`: the one
/// the sudoers rule runs as root for everyone and the administrators.
/// Both paths are the file's own, the link and `/proc/self/exe` followed.
pub fn runs_installed() -> bool {
    match (std::env::current_exe(), Path::new(INSTALLED).canonicalize()) {
        (Ok(this), Ok(installed)) => this == installed,
        _ => false,
    }
}

fn on_path(program: &str) -> bool {
    find_on_path(program).is_some()
}

/// Where `program` is, when it's in a folder of the PATH.
pub(crate) fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|file| file.is_file())
}

/// The machine's memory, from /proc/meminfo; 4 GiB when it can't be read.
fn total_memory() -> u64 {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map(|kb| kb << 10)
        .unwrap_or(4 << 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An upgrade runs `admin bubblewrap`, which puts the shared parts in
    /// place whether or not bubblewrap's own package can be installed: a
    /// server installed before them gets both on the upgrade that ships
    /// them, and a failure of either is said.
    #[test]
    fn the_shared_parts_go_in_whether_bubblewrap_can_or_not() {
        assert_eq!(both(Ok(()), Ok(())), Ok(()));
        let bubbles = both(Err("there's no apt-get here".into()), Ok(())).unwrap_err();
        assert!(bubbles.contains("no apt-get"), "{bubbles}");
        let shared = both(Ok(()), Err("There's no groupadd".into())).unwrap_err();
        assert!(shared.contains("admin toolchains setup"), "{shared}");
        assert!(shared.contains("There's no groupadd"), "{shared}");
        let neither = both(Err("one".into()), Err("two".into())).unwrap_err();
        assert!(neither.contains("one") && neither.contains("two"), "{neither}");
    }

    /// H07: the sudoers rule passes `visudo -c`, where there's a visudo.
    #[test]
    fn the_sudoers_file_passes_visudo() {
        let Some(visudo) = ["visudo", "/usr/sbin/visudo"]
            .into_iter()
            .find(|p| on_path(p) || Path::new(p).is_file())
        else {
            return;
        };
        let dir = rovik_core::testing::temp_dir();
        let file = dir.0.join("rovik");
        std::fs::write(&file, sudoers()).unwrap();
        let out = Command::new(visudo)
            .arg("-c")
            .arg("-f")
            .arg(&file)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
    }

    /// The units as systemd reads them, where `systemd-analyze` is there:
    /// no warning about any line.
    #[test]
    fn systemd_takes_the_units() {
        if !on_path("systemd-analyze") {
            return;
        }
        let dir = rovik_core::testing::temp_dir();
        let mut file = AdminFile::sized_for("ann", 16 << 30);
        file.cpu_quota = Some("200%".into());
        file.total_cpu_quota = Some("800%".into());
        // verify wants ExecStart's program to be there.
        let unit_text = unit().replace(INSTALLED, "/bin/true");
        std::fs::write(dir.0.join("rovik-agentd@ann.service"), unit_text).unwrap();
        std::fs::create_dir(dir.0.join("rovik-agentd@ann.service.d")).unwrap();
        std::fs::write(
            dir.0.join("rovik-agentd@ann.service.d/limits.conf"),
            drop_in(&file),
        )
        .unwrap();
        std::fs::write(dir.0.join("rovik.slice"), slice(&file)).unwrap();
        let out = Command::new("systemd-analyze")
            .arg("verify")
            .arg(dir.0.join("rovik.slice"))
            .arg(dir.0.join("rovik-agentd@ann.service"))
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && said.trim().is_empty(), "{said}");
    }

    #[test]
    fn releases_are_ordered_by_their_numbers() {
        assert_eq!(older("1.2.3", "1.2.4"), Some(true));
        assert_eq!(older("1.2.4", "1.2.3"), Some(false));
        assert_eq!(older("1.10.0", "1.9.9"), Some(false));
        assert_eq!(older("2.0.0", "1.99.99"), Some(false));
        // A pre-release comes before its release, and after the one before.
        assert_eq!(older("1.2.3-4", "1.2.3"), Some(true));
        assert_eq!(older("1.2.3-4", "1.2.3-5"), Some(true));
        assert_eq!(older("1.2.3-4", "1.2.2"), Some(false));
        assert_eq!(older("1.2.3", "1.2.3"), Some(false));
        assert_eq!(older("dev", "1.2.3"), None);
        assert_eq!(older("1.2", "1.2.3"), None);
        assert_eq!(older("1.2.3.4", "1.2.3"), None);
        assert_eq!(older("1.2.3-beta", "1.2.3"), None);
    }

    #[test]
    fn the_units_carry_the_limits() {
        let mut file = AdminFile::sized_for("ann", 8 << 30);
        assert_eq!(
            drop_in(&file).lines().skip(2).collect::<Vec<_>>(),
            ["MemoryMax=4096M", "TasksMax=4096"]
        );
        file.set("cpu_quota", "150%").unwrap();
        file.set("total_cpu_quota", "400%").unwrap();
        assert!(drop_in(&file).ends_with("CPUQuota=150%\n"));
        assert!(slice(&file).ends_with("MemoryMax=6553M\nCPUQuota=400%\n"));
        assert!(unit().contains("ExecStart=/opt/rovik/bin/rovik-agentd serve\n"));
        assert!(unit().contains("PrivateTmp=yes\n"));
        // Stopping, only the daemon hears SIGTERM, while its runs pause (R02).
        assert!(unit().contains("KillMode=mixed\nTimeoutStopSec=90\n"));
        assert!(crate::serve::DRAIN_WAIT < std::time::Duration::from_secs(90));
        assert_eq!(instance("ann"), "rovik-agentd@ann.service");
    }
}
