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
//! It's written a key a line, the administrators on one, which the
//! connection test's probe reads with sed (H04).

use serde::Deserialize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

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
    /// `admin upgrade` takes a build that isn't signed (B03).
    #[serde(default)]
    pub allow_unsigned: bool,
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
/// take (5.5).
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub max_worktrees: Option<usize>,
    pub max_disk_gb: Option<u64>,
    /// `~/.rovik` and `~/.local/share/rovik`, which the disk cap counts.
    pub folders: Vec<PathBuf>,
    /// Whom to ask for more.
    pub admins: Vec<String>,
}

impl Caps {
    pub fn new(file: &AdminFile, folders: Vec<PathBuf>) -> Caps {
        Caps {
            max_worktrees: file.max_worktrees,
            max_disk_gb: file.max_disk_gb,
            folders,
            admins: file.admins.clone(),
        }
    }

    /// Whether a new worktree for `chat` fits, the one it had aside.
    pub fn room_for_worktree(&self, worktrees: &Path, chat: &str) -> Result<(), String> {
        let ask = match self.admins.as_slice() {
            [] => "the server's administrators".to_string(),
            admins => admins.join(" or "),
        };
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
        if let Some(most) = self.max_disk_gb {
            let used: u64 = self.folders.iter().map(|f| size_of(f)).sum();
            if used >= most << 30 {
                return Err(format!(
                    "Rovik's folders take {} GB of the {most} GB this server allows you: \
                     delete chats you're done with, or ask {ask} for more.",
                    used >> 30
                ));
            }
        }
        Ok(())
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
