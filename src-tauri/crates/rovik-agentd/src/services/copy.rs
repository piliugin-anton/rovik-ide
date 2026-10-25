//! Copies of the project's files a service reads (its bind mounts, env
//! files, secrets' and configs' files: rovik_core::services::policy),
//! made in the daemon's own folder before the service starts, and what the
//! service mounts. A command of the agent's can change the chat's copy of
//! the project at any time, links among it, but not these: a link put in a
//! folder's place while Rovik copies reaches nothing, since each folder is
//! opened by the one above it, never following a link, from the chat's
//! copy down. A link inside is copied as the link it is: the container
//! resolves it in its own file system, where the user's files aren't.
//!
//! A copy is made when there's none, and again when what it copies changed
//! since ([`signature`]) and its service isn't running: a database's data
//! folder, which the service writes, stays the copy's for as long as the
//! chat's files it came from stay as they were. What a service writes goes
//! to the copy, never to the chat's files.

use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

/// The most bytes one start's copies may take together: a service's big
/// data belongs in a named volume.
pub const MOST_BYTES: u64 = 256 << 20;

/// Copies the file or folder `source` of the chat's copy of the project at
/// `root` (both as their links resolve, as policy checked them) to `to`,
/// which mustn't be there. A source that isn't there is copied as an empty
/// folder, as Compose makes a bind mount's missing folder. `budget`: the
/// bytes the copies may still take.
pub fn copy(root: &Path, source: &Path, to: &Path, budget: &mut u64) -> Result<(), String> {
    let changed = || format!("{} changed while Rovik copied it", source.display());
    let rel = source
        .strip_prefix(root)
        .map_err(|_| format!("{} isn't in {}", source.display(), root.display()))?;
    let mut dir = open_path(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(changed()),
        })
        .collect::<Result<_, _>>()?;
    let Some((last, above)) = parts.split_last() else {
        return copy_dir(dir.as_raw_fd(), to, budget);
    };
    for name in above {
        dir = match open_at(dir.as_raw_fd(), name, libc::O_DIRECTORY) {
            Ok(next) => next,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return empty_dir(to),
            Err(_) => return Err(changed()),
        };
    }
    match kind_at(dir.as_raw_fd(), last) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => empty_dir(to),
        Err(e) => Err(format!("{}: {e}", source.display())),
        Ok(libc::S_IFDIR) => {
            let opened =
                open_at(dir.as_raw_fd(), last, libc::O_DIRECTORY).map_err(|_| changed())?;
            copy_dir(opened.as_raw_fd(), to, budget)
        }
        Ok(libc::S_IFREG) => {
            let opened = open_at(dir.as_raw_fd(), last, 0).map_err(|_| changed())?;
            copy_file(opened, to, budget)
        }
        Ok(_) => Err(format!(
            "{} isn't a file or a folder, so a service can't read it",
            source.display()
        )),
    }
}

/// What `source`, the file or folder of the chat's copy, is now, in short:
/// what each file in it is called, its kind, size and last change. A copy
/// whose signature differs from its source's is made again.
pub fn signature(source: &Path) -> String {
    let mut hash = Sha256::new();
    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
    {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let rel = entry
            .path()
            .strip_prefix(source)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        let changed = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let kind = match meta.file_type() {
            t if t.is_dir() => "d",
            t if t.is_symlink() => "l",
            _ => "f",
        };
        entries.push((rel, format!("{kind}:{}:{changed}", meta.len())));
    }
    if entries.is_empty() {
        return "none".to_string();
    }
    for (rel, what) in entries {
        hash.update(rel.as_bytes());
        hash.update([0]);
        hash.update(what.as_bytes());
        hash.update([0]);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `path`, a folder named in full, opened.
fn open_path(path: &Path) -> io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: open with a valid C string; the descriptor is ours.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    match fd < 0 {
        true => Err(io::Error::last_os_error()),
        // SAFETY: a new descriptor no one else owns.
        false => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

/// `name` in the folder `dir`, opened read-only with `flags`, never through
/// a link.
fn open_at(dir: RawFd, name: &std::ffi::OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let name = CString::new(name.as_bytes())?;
    // SAFETY: openat on a folder this process holds, with a valid C string.
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | flags,
        )
    };
    match fd < 0 {
        true => Err(io::Error::last_os_error()),
        // SAFETY: a new descriptor no one else owns.
        false => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

/// The kind of `name` in the folder `dir` (`S_IFDIR`, `S_IFREG`,
/// `S_IFLNK`…), a link not followed.
fn kind_at(dir: RawFd, name: &std::ffi::OsStr) -> io::Result<libc::mode_t> {
    let name = CString::new(name.as_bytes())?;
    // SAFETY: an all-zero stat is valid; fstatat fills it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstatat on a folder this process holds, with a valid C string.
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.st_mode & libc::S_IFMT)
}

/// A new, empty folder at `to`.
fn empty_dir(to: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(to)
        .map_err(|e| format!("{}: {e}", to.display()))
}

/// The folder open at `dir` copied to `to`, as [`copy`] says.
fn copy_dir(dir: RawFd, to: &Path, budget: &mut u64) -> Result<(), String> {
    let at = |e: io::Error| format!("{}: {e}", to.display());
    // SAFETY: an all-zero stat is valid; fstat fills it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat on a descriptor this process holds.
    if unsafe { libc::fstat(dir, &mut stat) } != 0 {
        return Err(at(io::Error::last_os_error()));
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(to)
        .map_err(at)?;
    // The folder's own entries, read through its descriptor: never through
    // its name, which may lead elsewhere by now.
    let entries = std::fs::read_dir(format!("/proc/self/fd/{dir}")).map_err(at)?;
    for entry in entries {
        let name = entry.map_err(at)?.file_name();
        let into = to.join(&name);
        match kind_at(dir, &name).map_err(at)? {
            libc::S_IFDIR => {
                let sub = open_at(dir, &name, libc::O_DIRECTORY).map_err(at)?;
                copy_dir(sub.as_raw_fd(), &into, budget)?;
            }
            libc::S_IFREG => {
                let file = open_at(dir, &name, 0).map_err(at)?;
                copy_file(file, &into, budget)?;
            }
            libc::S_IFLNK => {
                let target = read_link_at(dir, &name).map_err(at)?;
                std::os::unix::fs::symlink(target, &into).map_err(at)?;
            }
            // Sockets, pipes and devices are no file a service reads.
            _ => {}
        }
    }
    // The folder's mode, with its owner's access kept: the daemon removes
    // the copy when it's made again.
    let mode = (stat.st_mode as u32 & 0o777) | 0o700;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode)).map_err(at)
}

/// The file open as `file` copied to `to`, with its mode, its bytes taken
/// from `budget`.
fn copy_file(file: OwnedFd, to: &Path, budget: &mut u64) -> Result<(), String> {
    let at = |e: io::Error| format!("{}: {e}", to.display());
    let mut from = std::fs::File::from(file);
    let meta = from.metadata().map_err(at)?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file any more", to.display()));
    }
    let size = meta.len();
    if size > *budget {
        return Err(format!(
            "the project's files its services read take more than {} MB: a service's big \
             data belongs in a named volume",
            MOST_BYTES >> 20
        ));
    }
    *budget -= size;
    let mut into = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(meta.permissions().mode() & 0o777)
        .open(to)
        .map_err(at)?;
    // A file that grew meanwhile is copied as far as it was.
    io::copy(&mut io::Read::take(&mut from, size), &mut into).map_err(at)?;
    Ok(())
}

/// Where the link `name` in the folder `dir` leads, as it says.
fn read_link_at(dir: RawFd, name: &std::ffi::OsStr) -> io::Result<std::path::PathBuf> {
    let name = CString::new(name.as_bytes())?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: readlinkat into a buffer of the length given.
    let len = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    let target: std::ffi::OsString = std::os::unix::ffi::OsStringExt::from_vec(buf);
    Ok(std::path::PathBuf::from(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A project with an init folder (a script, a link inside it and one
    /// leading out of the project) and an env file.
    fn project() -> rovik_core::testing::TempDir {
        let dir = rovik_core::testing::temp_dir();
        let root = dir.0.join("project");
        fs::create_dir_all(root.join("db/more")).unwrap();
        fs::write(root.join("db/init.sql"), "create table t ();").unwrap();
        fs::set_permissions(root.join("db/init.sql"), fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(root.join("db/more/seed.sql"), "insert into t values ();").unwrap();
        std::os::unix::fs::symlink("init.sql", root.join("db/again.sql")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("db/out")).unwrap();
        fs::write(root.join("db.env"), "A=1\n").unwrap();
        dir
    }

    #[test]
    fn a_folder_is_copied_with_its_links_as_links_and_its_modes() {
        let dir = project();
        let root = dir.0.join("project").canonicalize().unwrap();
        let to = dir.0.join("copy");
        let mut budget = MOST_BYTES;
        copy(&root, &root.join("db"), &to, &mut budget).unwrap();
        assert_eq!(
            fs::read_to_string(to.join("init.sql")).unwrap(),
            "create table t ();"
        );
        assert_eq!(
            fs::read_to_string(to.join("more/seed.sql")).unwrap(),
            "insert into t values ();"
        );
        let mode = fs::metadata(to.join("init.sql"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
        // Links stay links, resolved where the copy is mounted, never here.
        assert_eq!(
            fs::read_link(to.join("again.sql")).unwrap(),
            Path::new("init.sql")
        );
        assert_eq!(
            fs::read_link(to.join("out")).unwrap(),
            Path::new("/etc/passwd")
        );
        assert_eq!(budget, MOST_BYTES - 18 - 24, "the files' bytes are counted");

        let file = dir.0.join("env-copy");
        copy(&root, &root.join("db.env"), &file, &mut budget).unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), "A=1\n");
    }

    #[test]
    fn a_missing_source_is_an_empty_folder_and_a_link_put_in_its_place_reaches_nothing() {
        let dir = project();
        let root = dir.0.join("project").canonicalize().unwrap();
        let mut budget = MOST_BYTES;
        let empty = dir.0.join("empty");
        copy(&root, &root.join("data/pg"), &empty, &mut budget).unwrap();
        assert!(fs::read_dir(&empty).unwrap().next().is_none());

        // A folder swapped for a link to somewhere outside, as a command
        // racing the copy could: refused, nothing of outside copied.
        let outside = rovik_core::testing::temp_dir();
        fs::write(outside.0.join("id_rsa"), "secret").unwrap();
        fs::rename(root.join("db"), root.join("db-was")).unwrap();
        std::os::unix::fs::symlink(&outside.0, root.join("db")).unwrap();
        let why = copy(&root, &root.join("db"), &dir.0.join("swapped"), &mut budget).unwrap_err();
        assert!(
            why.contains("changed while Rovik copied it") || why.contains("isn't a file"),
            "{why}"
        );
        let why = copy(
            &root,
            &root.join("db/id_rsa"),
            &dir.0.join("deeper"),
            &mut budget,
        )
        .unwrap_err();
        assert!(why.contains("changed while Rovik copied it"), "{why}");
        assert!(!dir.0.join("swapped/id_rsa").exists());
    }

    #[test]
    fn copies_past_the_budget_are_refused() {
        let dir = project();
        let root = dir.0.join("project").canonicalize().unwrap();
        let mut budget = 10;
        let why = copy(&root, &root.join("db"), &dir.0.join("copy"), &mut budget).unwrap_err();
        assert!(why.contains("belongs in a named volume"), "{why}");
    }

    #[test]
    fn a_change_to_the_source_changes_its_signature() {
        let dir = project();
        let root = dir.0.join("project");
        let before = signature(&root.join("db"));
        assert_eq!(
            before,
            signature(&root.join("db")),
            "the same files, the same signature"
        );
        fs::write(
            root.join("db/more/seed.sql"),
            "insert into t values (), ();",
        )
        .unwrap();
        assert_ne!(before, signature(&root.join("db")));
        assert_eq!(signature(&root.join("nothing")), "none");
    }
}
