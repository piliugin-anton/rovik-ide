//! `/etc/rovik/agentd.toml` (5.5, D17): Rovik's administrators on this
//! server, and the limits that bind every user's daemon. root writes it
//! (`install`, `admin`), everyone reads it. Each daemon enforces the slots,
//! worktrees and disk it names; systemd enforces the memory, task and CPU
//! caps, through the unit's drop-in and `rovik.slice`, which the installer
//! writes from it.
//!
//!   admins = ["ann", "bo"]
//!   max_slots = 2
//!   max_worktrees = 40
//!   max_disk_gb = 50
//!   memory_max = "8G"
//!   tasks_max = 4096
//!   cpu_quota = "200%"
//!   total_memory_max = "26G"
//!   total_cpu_quota = "800%"
//!   allow_unsigned = false
//!
//!   [toolchains]
//!   user_install = true
//!   shared_caches = true
//!   allowed_rust = ["1.82"]
//!
//! It's written a key a line, the administrators on one, which the
//! connection test's probe reads with sed (H04).

use serde::Deserialize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a measure of Rovik's folders stands for what adds to them
/// without a worktree (a push, an upload): a push comes before each
/// message, and the folders are walked whole.
const MEASURE_AGAIN: Duration = Duration::from_secs(30);

/// Where it is.
pub const ADMIN_FILE: &str = "/etc/rovik/agentd.toml";

#[derive(Deserialize, Default, Debug, Clone, PartialEq)]
pub struct AdminFile {
    /// The accounts that change the limits and upgrade Rovik.
    #[serde(default)]
    pub admins: Vec<String>,
    /// Per user, enforced by each daemon: heavy-work slots at once, chats'
    /// worktrees kept, and the size of Rovik's two folders together.
    pub max_slots: Option<usize>,
    pub max_worktrees: Option<usize>,
    pub max_disk_gb: Option<u64>,
    /// Per user, on the daemon's cgroup, which holds everything it starts:
    /// systemd's `MemoryMax=`, `TasksMax=` and `CPUQuota=`.
    pub memory_max: Option<String>,
    pub tasks_max: Option<u64>,
    pub cpu_quota: Option<String>,
    /// For all of Rovik, on `rovik.slice`.
    pub total_memory_max: Option<String>,
    pub total_cpu_quota: Option<String>,
    /// `admin upgrade` takes a build that isn't signed with the release
    /// key: a build of one's own (B03). A release's signed build goes in
    /// either way. sudo runs the installed build as root for everyone, so
    /// this gives each administrator root on the server.
    #[serde(default)]
    pub allow_unsigned: bool,
    /// The shared toolchains (installer/toolchains.rs): who may install
    /// one, and whether the package caches are shared. Not limits: they're
    /// changed with `admin toolchains …`.
    #[serde(default)]
    pub toolchains: ToolchainSettings,
}

/// The shared toolchains' settings: a `[toolchains]` table in
/// `agentd.toml`. An administrator changes them with `admin toolchains
/// user-install on|off` and `admin toolchains shared-caches on|off`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ToolchainSettings {
    /// A chat may install the version its project asks for, through the
    /// same sudo rule as `user start`: `rovik-agentd user toolchain …`. An
    /// administrator's `admin toolchain add` works either way.
    #[serde(default = "yes")]
    pub user_install: bool,
    /// Every daemon shares one npm cache and one store of crates.
    #[serde(default = "yes")]
    pub shared_caches: bool,
    /// The Rust versions a user may install, such as `1.82`: empty takes
    /// any valid one.
    #[serde(default)]
    pub allowed_rust: Vec<String>,
}

/// The default of the table's two switches.
fn yes() -> bool {
    true
}

impl Default for ToolchainSettings {
    fn default() -> Self {
        ToolchainSettings {
            user_install: true,
            shared_caches: true,
            allowed_rust: Vec::new(),
        }
    }
}

impl ToolchainSettings {
    /// The table as `agentd.toml` writes it.
    fn to_toml(&self) -> String {
        let mut text = String::from("\n[toolchains]\n");
        text.push_str(&format!("user_install = {}\n", self.user_install));
        text.push_str(&format!("shared_caches = {}\n", self.shared_caches));
        if !self.allowed_rust.is_empty() {
            let versions: Vec<String> = self.allowed_rust.iter().map(|v| format!("\"{v}\"")).collect();
            text.push_str(&format!("allowed_rust = [{}]\n", versions.join(", ")));
        }
        text
    }
}

/// The keys `admin limits` sets, in the order they're shown.
pub const LIMITS: [&str; 8] = [
    "max_slots",
    "max_worktrees",
    "max_disk_gb",
    "memory_max",
    "tasks_max",
    "cpu_quota",
    "total_memory_max",
    "total_cpu_quota",
];

/// The tasks a daemon may run at once, unless set: enough for builds, not
/// for a fork bomb.
const TASKS: u64 = 4096;

impl AdminFile {
    /// The file at `path`, if Rovik is installed.
    pub fn read(path: &Path) -> Result<Option<AdminFile>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let file: AdminFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        file.check()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(file))
    }

    /// A new install's: `admin` its administrator, the limits sized to a
    /// machine with `memory` bytes (5.5). Per user, half the memory and an
    /// equal share of the CPUs (each daemon weighs the same, uncapped); for
    /// all of Rovik, 80% of the memory.
    pub fn sized_for(admin: &str, memory: u64) -> AdminFile {
        let megabytes = |share: u64| format!("{}M", (memory * share / 100) >> 20);
        AdminFile {
            admins: vec![admin.into()],
            memory_max: Some(megabytes(50)),
            tasks_max: Some(TASKS),
            total_memory_max: Some(megabytes(80)),
            ..AdminFile::default()
        }
    }

    /// Refuses what systemd or the daemon couldn't take: these values go
    /// into unit files.
    pub fn check(&self) -> Result<(), String> {
        for admin in &self.admins {
            check_user_name(admin)?;
        }
        for (key, value) in [
            ("max_slots", self.max_slots.map(|n| n as u64)),
            ("max_worktrees", self.max_worktrees.map(|n| n as u64)),
            ("max_disk_gb", self.max_disk_gb),
            ("tasks_max", self.tasks_max),
        ] {
            if value == Some(0) {
                return Err(format!("{key} is at least 1"));
            }
        }
        for (key, value) in [
            ("memory_max", &self.memory_max),
            ("total_memory_max", &self.total_memory_max),
        ] {
            if let Some(value) = value.as_deref().filter(|v| !is_size(v)) {
                return Err(format!(
                    "{key} is a size, such as 8G, 512M or 50%, not {value:?}"
                ));
            }
        }
        for (key, value) in [
            ("cpu_quota", &self.cpu_quota),
            ("total_cpu_quota", &self.total_cpu_quota),
        ] {
            if let Some(value) = value.as_deref().filter(|v| !is_percent(v)) {
                return Err(format!(
                    "{key} is a share of one CPU, such as 200% for two, not {value:?}"
                ));
            }
        }
        // The same check root makes of a version a chat asks it to install
        // (installer/toolchains.rs): this list goes into a file root reads.
        for version in &self.toolchains.allowed_rust {
            crate::installer::toolchains::parse_version(
                crate::installer::toolchains::Kind::Rust,
                version,
            )
            .map_err(|why| format!("allowed_rust: {why}"))?;
        }
        Ok(())
    }

    /// Sets a limit, as `admin limits key=value` does; `none` takes it away.
    /// A value refused leaves the file as it was.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let mut next = self.clone();
        next.change(key, value)?;
        next.check()?;
        *self = next;
        Ok(())
    }

    fn change(&mut self, key: &str, value: &str) -> Result<(), String> {
        let none = value == "none";
        let number = |value: &str| -> Result<Option<u64>, String> {
            match none {
                true => Ok(None),
                false => value
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("{key} is a number, not {value:?}")),
            }
        };
        let text = || (!none).then(|| value.to_string());
        match key {
            "max_slots" => self.max_slots = number(value)?.map(|n| n as usize),
            "max_worktrees" => self.max_worktrees = number(value)?.map(|n| n as usize),
            "max_disk_gb" => self.max_disk_gb = number(value)?,
            "tasks_max" => self.tasks_max = number(value)?,
            "memory_max" => self.memory_max = text(),
            "cpu_quota" => self.cpu_quota = text(),
            "total_memory_max" => self.total_memory_max = text(),
            "total_cpu_quota" => self.total_cpu_quota = text(),
            other => {
                return Err(format!(
                    "There's no limit {other}. The limits: {}.",
                    LIMITS.join(", ")
                ))
            }
        }
        Ok(())
    }

    /// Each limit and its value, `none` when it isn't set.
    pub fn limits(&self) -> Vec<(&'static str, String)> {
        let shown = |value: Option<String>| value.unwrap_or_else(|| "none".into());
        let values = [
            self.max_slots.map(|n| n.to_string()),
            self.max_worktrees.map(|n| n.to_string()),
            self.max_disk_gb.map(|n| n.to_string()),
            self.memory_max.clone(),
            self.tasks_max.map(|n| n.to_string()),
            self.cpu_quota.clone(),
            self.total_memory_max.clone(),
            self.total_cpu_quota.clone(),
        ];
        LIMITS.into_iter().zip(values.map(shown)).collect()
    }

    /// The file's text: a key a line, those not set left out.
    pub fn to_toml(&self) -> String {
        let quoted = |text: &str| format!("\"{text}\"");
        let mut text = String::from(
            "# Rovik on this server: its administrators and the limits on each\n\
             # user's daemon. Change them with `rovik-agentd admin`, which\n\
             # rewrites this file and the units made from it.\n",
        );
        let admins: Vec<String> = self.admins.iter().map(|a| quoted(a)).collect();
        text.push_str(&format!("admins = [{}]\n", admins.join(", ")));
        for (key, value) in self.limits() {
            if value == "none" {
                continue;
            }
            let value = match key {
                "memory_max" | "cpu_quota" | "total_memory_max" | "total_cpu_quota" => {
                    quoted(&value)
                }
                _ => value,
            };
            text.push_str(&format!("{key} = {value}\n"));
        }
        if self.allow_unsigned {
            text.push_str("allow_unsigned = true\n");
        }
        text.push_str(&self.toolchains.to_toml());
        text
    }

    /// The heavy-work slots a daemon runs with (5.5): see `Slots`.
    pub fn slots(&self, user: Option<usize>, others: u32) -> Slots {
        Slots {
            manual: user.or((others > 0).then_some(1)),
            cap: self.max_slots,
        }
    }
}

/// The heavy-work slots: the number the user set in their config.toml, or
/// one while other users' daemons run here, else the machine's own rule
/// (`None`); and never more than the administrators' cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slots {
    pub manual: Option<usize>,
    pub cap: Option<usize>,
}

/// What a daemon enforces of the administrators' limits itself, besides the
/// slots: the worktrees its user keeps, and the disk Rovik's two folders
/// take (5.5). Each is checked before what would add to it: a worktree, a
/// push, an upload, an import.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub max_worktrees: Option<usize>,
    pub max_disk_gb: Option<u64>,
    /// `~/.rovik` and `~/.local/share/rovik`, which the disk cap counts.
    pub folders: Vec<PathBuf>,
    /// Whom to ask for more.
    pub admins: Vec<String>,
    /// The folders' last measure, and when it was taken: shared by the
    /// clones.
    measured: Arc<Mutex<Option<(Instant, u64)>>>,
}

impl Caps {
    pub fn new(file: &AdminFile, folders: Vec<PathBuf>) -> Caps {
        Caps {
            max_worktrees: file.max_worktrees,
            max_disk_gb: file.max_disk_gb,
            folders,
            admins: file.admins.clone(),
            measured: Arc::default(),
        }
    }

    /// Whom to ask for more.
    fn ask(&self) -> String {
        match self.admins.as_slice() {
            [] => "the server's administrators".to_string(),
            admins => admins.join(" or "),
        }
    }

    /// Whether Rovik's folders are under the disk cap, as measured within
    /// `MEASURE_AGAIN`: for what adds to them without a worktree.
    pub fn room_on_disk(&self) -> Result<(), String> {
        self.under_disk_cap(false)
    }

    /// The disk cap, against a measure taken now when `fresh`.
    fn under_disk_cap(&self, fresh: bool) -> Result<(), String> {
        let Some(most) = self.max_disk_gb else {
            return Ok(());
        };
        let used = self.used(fresh);
        if used >= most << 30 {
            return Err(format!(
                "Rovik's folders take {} GB of the {most} GB this server allows you: \
                 delete chats you're done with, or ask {} for more.",
                used >> 30,
                self.ask()
            ));
        }
        Ok(())
    }

    /// What Rovik's folders take: measured again when `fresh` or when the
    /// last measure is too old. One measure at a time.
    fn used(&self, fresh: bool) -> u64 {
        let mut measured = self.measured.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, used)) = *measured {
            if !fresh && at.elapsed() < MEASURE_AGAIN {
                return used;
            }
        }
        let used = self.folders.iter().map(|f| size_of(f)).sum();
        *measured = Some((Instant::now(), used));
        used
    }

    /// Whether a new worktree for `chat` fits, the one it had aside. The
    /// caller makes worktrees one at a time, so two can't both fit the
    /// last place.
    pub fn room_for_worktree(&self, worktrees: &Path, chat: &str) -> Result<(), String> {
        let ask = self.ask();
        if let Some(most) = self.max_worktrees {
            let kept = std::fs::read_dir(worktrees)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name() != chat && e.path().is_dir())
                .count();
            if kept >= most {
                return Err(format!(
                    "This server keeps {most} worktrees per user, and your chats have {kept}: \
                     delete chats you're done with, or ask {ask} for more."
                ));
            }
        }
        self.under_disk_cap(true)
    }
}

/// The bytes the files under `dir` take: links not followed, and other
/// file systems (the virtual files' mount) not entered.
pub fn size_of(dir: &Path) -> u64 {
    let Ok(top) = std::fs::symlink_metadata(dir) else {
        return 0;
    };
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() && meta.dev() == top.dev() {
                pending.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// A user name as `useradd` takes it, safe in a unit's instance name and in
/// the sudoers file.
pub fn check_user_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    match first && rest && name.len() <= 32 {
        true => Ok(()),
        false => Err(format!("{name:?} isn't a user name Rovik takes")),
    }
}

/// `8G`, `512M`, `50%`, `infinity`: what `MemoryMax=` takes.
fn is_size(value: &str) -> bool {
    if value == "infinity" {
        return true;
    }
    let digits = value.trim_end_matches(['K', 'M', 'G', 'T', '%']);
    let unit = &value[digits.len()..];
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && unit.len() <= 1
}

/// `200%`: what `CPUQuota=` takes.
fn is_percent(value: &str) -> bool {
    value
        .strip_suffix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_a_key_a_line() {
        let mut file = AdminFile::sized_for("ann", 16 << 30);
        assert_eq!(file.memory_max.as_deref(), Some("8192M"));
        assert_eq!(file.total_memory_max.as_deref(), Some("13107M"));
        file.admins.push("bo".into());
        file.set("max_slots", "2").unwrap();
        file.set("cpu_quota", "200%").unwrap();
        let text = file.to_toml();
        assert!(text.contains("\nadmins = [\"ann\", \"bo\"]\n"), "{text}");
        assert!(text.contains("\nmax_slots = 2\n"), "{text}");
        assert!(text.contains("\ncpu_quota = \"200%\"\n"), "{text}");
        assert!(!text.contains("max_worktrees"), "{text}");
        let dir = rovik_core::testing::temp_dir();
        let path = dir.0.join("agentd.toml");
        std::fs::write(&path, &text).unwrap();
        assert_eq!(AdminFile::read(&path).unwrap(), Some(file.clone()));
        file.set("cpu_quota", "none").unwrap();
        assert_eq!(file.cpu_quota, None);
        assert_eq!(AdminFile::read(&dir.0.join("missing.toml")).unwrap(), None);
    }

    #[test]
    fn the_toolchain_table_round_trips_and_checks_its_versions() {
        let mut file = AdminFile::sized_for("ann", 8 << 30);
        // Both switches are on until an administrator turns one off.
        assert_eq!(file.toolchains, ToolchainSettings::default());
        assert!(file.toolchains.user_install && file.toolchains.shared_caches);
        let text = file.to_toml();
        assert!(
            text.contains("\n[toolchains]\nuser_install = true\nshared_caches = true\n"),
            "{text}"
        );
        // A file without the table keeps them.
        let dir = rovik_core::testing::temp_dir();
        let path = dir.0.join("agentd.toml");
        std::fs::write(&path, "admins = [\"ann\"]\n").unwrap();
        let plain = AdminFile::read(&path).unwrap().unwrap();
        assert_eq!(plain.toolchains, ToolchainSettings::default());

        file.toolchains.user_install = false;
        file.toolchains.shared_caches = false;
        file.toolchains.allowed_rust = vec!["1.82".into(), "nightly".into()];
        let text = file.to_toml();
        assert!(
            text.contains(
                "\n[toolchains]\nuser_install = false\nshared_caches = false\n\
                 allowed_rust = [\"1.82\", \"nightly\"]\n"
            ),
            "{text}"
        );
        std::fs::write(&path, &text).unwrap();
        assert_eq!(AdminFile::read(&path).unwrap(), Some(file.clone()));

        // A version root wouldn't take is refused, as `user toolchain`
        // refuses it: this list goes into a file root reads.
        for bad in ["1.82; rm -rf /", "../x", "", "1.82.0.1"] {
            file.toolchains.allowed_rust = vec![bad.into()];
            assert!(file.check().is_err(), "{bad:?} was taken");
        }
        file.toolchains.allowed_rust = vec!["stable".into(), "1.82.0".into()];
        assert_eq!(file.check(), Ok(()));
    }

    #[test]
    fn what_would_break_a_unit_file_is_refused() {
        let mut file = AdminFile::sized_for("ann", 8 << 30);
        assert!(file
            .set("memory_max", "8G\nExecStartPre=/bin/evil")
            .is_err());
        assert!(file.set("memory_max", "lots").is_err());
        assert!(file.set("cpu_quota", "2").is_err());
        assert!(file.set("max_slots", "0").is_err());
        assert_eq!(file.max_slots, None, "a refused value isn't kept");
        assert!(file.set("max_slots", "two").is_err());
        assert!(file
            .set("colour", "blue")
            .unwrap_err()
            .contains("max_slots"));
        file.set("memory_max", "50%").unwrap();
        file.set("total_memory_max", "infinity").unwrap();
        file.admins.push("bo\nALL ALL=(ALL) ALL".into());
        assert!(file.check().is_err());
        assert!(check_user_name("ann.b-c_1").is_ok());
        assert!(check_user_name("-rf").is_err());
        assert!(check_user_name("").is_err());
    }

    #[test]
    fn worktrees_and_disk_are_capped() {
        let dir = rovik_core::testing::temp_dir();
        let work = dir.0.join("work");
        for chat in ["c1", "c2"] {
            std::fs::create_dir_all(work.join(chat)).unwrap();
        }
        std::fs::write(work.join("c1/big"), vec![0u8; 3 << 20]).unwrap();
        let file = AdminFile {
            admins: vec!["ann".into()],
            max_worktrees: Some(2),
            ..AdminFile::default()
        };
        let caps = Caps::new(&file, vec![dir.0.clone()]);
        assert!(caps.room_for_worktree(&work, "c2").is_ok(), "its own again");
        let full = caps.room_for_worktree(&work, "c3").unwrap_err();
        assert!(
            full.contains("keeps 2 worktrees") && full.contains("ask ann"),
            "{full}"
        );
        assert_eq!(size_of(&dir.0), 3 << 20);
        let disk = Caps {
            max_worktrees: None,
            max_disk_gb: Some(1),
            ..caps
        };
        assert!(disk.room_for_worktree(&work, "c3").is_ok(), "3 MB of 1 GB");
        assert!(disk.room_on_disk().is_ok());
    }

    /// 5.5: what adds to Rovik's folders without a worktree (a push, an
    /// upload) is refused at the disk cap too, measured again now and then;
    /// a new worktree measures them at once.
    #[test]
    fn pushes_and_uploads_stop_at_the_disk_cap() {
        let dir = rovik_core::testing::temp_dir();
        let file = AdminFile {
            admins: vec!["ann".into()],
            max_disk_gb: Some(1),
            ..AdminFile::default()
        };
        let caps = Caps::new(&file, vec![dir.0.clone()]);
        assert!(caps.room_on_disk().is_ok());
        // A file of a gigabyte that takes no room: its length counts.
        let big = std::fs::File::create(dir.0.join("big")).unwrap();
        big.set_len(1 << 30).unwrap();
        assert!(caps.room_on_disk().is_ok(), "the last measure still stands");
        let full = caps
            .room_for_worktree(&dir.0.join("work"), "c1")
            .unwrap_err();
        assert!(
            full.contains("1 GB of the 1 GB") && full.contains("ask ann"),
            "{full}"
        );
        // That measure stands for the others, the clones' too.
        assert!(caps.clone().room_on_disk().is_err());
    }

    /// H07: the administrators' `max_slots` wins over the user's number,
    /// and one slot is the default while others' daemons run.
    #[test]
    fn the_admin_files_max_slots_wins_over_the_users() {
        let mut file = AdminFile::default();
        let slots = |file: &AdminFile, user, others| file.slots(user, others);
        assert_eq!(
            slots(&file, None, 0),
            Slots {
                manual: None,
                cap: None
            }
        );
        assert_eq!(slots(&file, None, 2).manual, Some(1), "others' daemons run");
        assert_eq!(
            slots(&file, Some(3), 2).manual,
            Some(3),
            "the user's number"
        );
        file.max_slots = Some(2);
        let capped = slots(&file, Some(4), 0);
        // The queue takes the lower (workload::queue's tests).
        assert_eq!(
            capped,
            Slots {
                manual: Some(4),
                cap: Some(2)
            }
        );
    }
}
