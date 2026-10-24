//! The daemon's folders (5.1): `~/.rovik` for its own files and
//! `~/.local/share/rovik` for the agent's work, both its user's own and
//! closed to everyone else (D10). Keeping the work outside `~/.rovik` means
//! the agent's ordinary commands never name the daemon's files (D8).
//! `--data DIR` puts both in DIR, for tests and development.

use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub struct Home {
    /// The daemon's own files: its socket, lock, config and the core's data.
    pub root: PathBuf,
    /// What the agent works in: worktrees and temporary files.
    pub share: PathBuf,
}

impl Home {
    pub fn locate(data: Option<PathBuf>) -> Result<Home, String> {
        if let Some(dir) = data {
            return Ok(Home {
                root: dir.clone(),
                share: dir,
            });
        }
        let home = home_dir().ok_or("The account has no home folder.")?;
        Ok(Home {
            root: home.join(".rovik"),
            share: home.join(".local/share/rovik"),
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.root.join("agentd.sock")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// The core's data folder: the chat database, attachments, the agent log.
    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    /// The chats' result indexes (6.5).
    pub fn results(&self) -> PathBuf {
        self.root.join("results")
    }

    /// The core's config folder: project-settings.json (K03).
    pub fn core_config(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// TMPDIR for the daemon and the agent's commands (K13).
    pub fn tmp(&self) -> PathBuf {
        self.share.join("tmp")
    }

    /// The chats' worktrees (6.4).
    pub fn work(&self) -> PathBuf {
        self.share.join("work")
    }

    /// This user's Yarn and pnpm, which Rovik runs through Corepack on a
    /// host's Node (rovik_core::toolchains::add_shim), at the end of the
    /// agent's commands' PATH.
    pub fn bin(&self) -> PathBuf {
        self.share.join("bin")
    }

    /// Creates the folders the first time. Refuses ones another user owns
    /// or that others can open: chats and keys are kept in them.
    pub fn prepare(&self) -> Result<(), String> {
        for dir in self.folders() {
            private_folder(dir)?;
        }
        for dir in [self.logs(), self.tmp()] {
            private_folder(&dir)?;
        }
        Ok(())
    }

    /// Problems `prepare` would refuse, without creating anything (doctor).
    pub fn problems(&self) -> Vec<String> {
        self.folders()
            .into_iter()
            .filter(|dir| dir.exists())
            .filter_map(|dir| check_private(dir).err())
            .collect()
    }

    /// The two folders, or the one `--data` names.
    pub fn folders(&self) -> Vec<&Path> {
        let mut folders = vec![self.root.as_path()];
        if self.share != self.root {
            folders.push(&self.share);
        }
        folders
    }

    /// One daemon per account: the lock is held for as long as the returned
    /// file is open.
    pub fn lock(&self) -> Result<File, String> {
        let path = self.root.join("agentd.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "Another rovik-agentd runs for this account ({} is held).",
                path.display()
            ));
        }
        Ok(file)
    }
}

/// A folder of this user's that only they can open, made if it's missing.
fn private_folder(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    check_private(dir)
}

fn check_private(dir: &Path) -> Result<(), String> {
    let meta = fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} isn't a folder.", dir.display()));
    }
    if meta.uid() != uid() {
        return Err(format!("{} belongs to another user.", dir.display()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is open to other users (mode {:o}). Close it with: chmod 700 {}",
            dir.display(),
            meta.mode() & 0o777,
            dir.display()
        ));
    }
    Ok(())
}

pub fn uid() -> u32 {
    // SAFETY: getuid can't fail.
    unsafe { libc::getuid() }
}

/// $HOME, or the passwd entry's when it's unset (a service's environment).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| passwd().map(|p| p.home))
}

/// An account's name, ids, home and shell, from the passwd database.
pub struct Passwd {
    pub name: String,
    pub uid: u32,
    /// Read by the installer's Linux-only code (servers run Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub gid: u32,
    pub home: PathBuf,
    pub shell: String,
}

/// This process's account.
pub fn passwd() -> Option<Passwd> {
    passwd_of(uid())
}

/// The account with id `uid`.
pub fn passwd_of(uid: u32) -> Option<Passwd> {
    // SAFETY: getpwuid_r fills the entry and its strings in the buffers
    // `lookup` gives it.
    lookup(|entry, buf, len, found| unsafe { libc::getpwuid_r(uid, entry, buf, len, found) })
}

/// The account named `name`.
pub fn passwd_named(name: &str) -> Option<Passwd> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: as in `passwd_of`; `name` outlives the call.
    lookup(|entry, buf, len, found| unsafe {
        libc::getpwnam_r(name.as_ptr(), entry, buf, len, found)
    })
}

type Found = *mut libc::passwd;

fn lookup(
    get: impl FnOnce(*mut libc::passwd, *mut libc::c_char, usize, *mut Found) -> libc::c_int,
) -> Option<Passwd> {
    use std::ffi::CStr;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter, and the lookup
    // writes the strings it points to into `buf`, which outlives them here.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: Found = std::ptr::null_mut();
    let status = get(&mut entry, buf.as_mut_ptr(), buf.len(), &mut found);
    if status != 0 || found.is_null() {
        return None;
    }
    let text = |p: *const libc::c_char| {
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    };
    Some(Passwd {
        name: text(entry.pw_name)?,
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        home: PathBuf::from(text(entry.pw_dir)?),
        shell: text(entry.pw_shell).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn data_puts_everything_in_one_folder() {
        let home = Home::locate(Some("/d".into())).unwrap();
        assert_eq!(home.socket(), Path::new("/d/agentd.sock"));
        assert_eq!(home.data(), Path::new("/d/data"));
        assert_eq!(home.tmp(), Path::new("/d/tmp"));
    }

    #[test]
    fn folders_are_made_private_and_open_ones_refused() {
        let dir = rovik_core::testing::temp_dir();
        let home = Home::locate(Some(dir.0.join("rovik"))).unwrap();
        home.prepare().unwrap();
        for d in [home.root.clone(), home.logs(), home.tmp()] {
            assert_eq!(fs::metadata(&d).unwrap().mode() & 0o777, 0o700, "{d:?}");
        }
        assert!(home.problems().is_empty());
        fs::set_permissions(&home.root, fs::Permissions::from_mode(0o755)).unwrap();
        let refused = home.prepare().unwrap_err();
        assert!(refused.contains("chmod 700"), "{refused}");
        assert_eq!(home.problems().len(), 1);
    }

    #[test]
    fn a_second_daemon_cant_take_the_lock() {
        let dir = rovik_core::testing::temp_dir();
        let home = Home::locate(Some(dir.0.join("rovik"))).unwrap();
        home.prepare().unwrap();
        let held = home.lock().unwrap();
        // flock locks belong to the open file, so a second open is refused.
        assert!(home.lock().unwrap_err().contains("Another rovik-agentd"));
        drop(held);
        assert!(home.lock().is_ok());
    }

    #[test]
    fn the_passwd_entry_is_this_users() {
        let entry = passwd().unwrap();
        assert!(!entry.name.is_empty());
        assert!(entry.home.is_absolute());
        assert_eq!(entry.uid, uid());
        let named = passwd_named(&entry.name).unwrap();
        assert_eq!((named.uid, named.home), (entry.uid, entry.home));
        assert!(passwd_named("no such user, surely").is_none());
    }
}
