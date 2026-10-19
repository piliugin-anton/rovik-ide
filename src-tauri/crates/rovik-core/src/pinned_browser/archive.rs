//! A build's archive: downloaded and checked against its pin, unpacked,
//! and the other versions next to it cleared away. Shared by the app's
//! download and a cloud host's install.

use super::{Build, VERSION};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// The longest the download may go without a byte arriving.
const STALL_WAIT: Duration = Duration::from_secs(60);

/// The archive of `build` downloaded into the new file `to`, refused unless
/// it's the pinned build: its size, then its SHA-256. `progress` hears the
/// bytes received so far as they come.
pub async fn download(
    build: &Build,
    to: &Path,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let url = build.url();
    // Settings → Network's timeout and proxy (`net`).
    let client = crate::net::builder()
        .await
        .user_agent(concat!("rovik/", env!("CARGO_PKG_VERSION")))
        .read_timeout(STALL_WAIT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{url}: {e}"))?;
    let mut file = new_file(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let (mut hash, mut size) = (Sha256::new(), 0u64);
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| format!("{url}: {e}"))?;
        size += chunk.len() as u64;
        if size > build.size {
            return Err(format!(
                "{url} is larger than the {} bytes Rovik pins.",
                build.size
            ));
        }
        hash.update(&chunk);
        file.write_all(&chunk)
            .map_err(|e| format!("{}: {e}", to.display()))?;
        progress(size);
    }
    verify(&url, size, &hash.finalize(), build)
}

/// A new file that only this user can read until it's checked.
fn new_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Whether `size` bytes hashing to `sha256` are the pinned build.
fn verify(url: &str, size: u64, sha256: &[u8], build: &Build) -> Result<(), String> {
    let sha256: String = sha256.iter().map(|byte| format!("{byte:02x}")).collect();
    if size == build.size && sha256 == build.sha256 {
        return Ok(());
    }
    Err(format!(
        "{url} isn't the build Rovik pins: it has {size} bytes with SHA-256 {sha256}, \
         not {} bytes with {}.",
        build.size, build.sha256
    ))
}

/// The archive's files in the new folder `to`, taken out of the one folder
/// they're all in, `folder`, everything readable by all. Nothing may lead
/// out of `to`: no absolute path or `..`, and only the links a macOS bundle
/// has, relative ones that stay inside. `to` is its owner's alone until
/// it's whole.
pub fn unpack(archive: &Path, to: &Path, folder: &str) -> Result<(), String> {
    let named = |e: &dyn std::fmt::Display| format!("{}: {e}", archive.display());
    let file = std::fs::File::open(archive).map_err(|e| named(&e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| named(&e))?;
    new_dir(to, 0o700)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| named(&e))?;
        let name = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let relative = entry
            .enclosed_name()
            .and_then(|path| Some(path.strip_prefix(folder).ok()?.to_path_buf()))
            .ok_or_else(|| format!("{name}, in the archive, isn't in {folder}."))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let path = to.join(&relative);
        if let Some(parent) = path.parent() {
            make_dirs(parent)?;
        }
        if entry.is_symlink() {
            let mut target = String::new();
            entry.read_to_string(&mut target).map_err(|e| named(&e))?;
            link(&path, &relative, &target)
                .map_err(|why| format!("{name}, in the archive, is a link {why}."))?;
            continue;
        }
        if entry.is_dir() {
            make_dirs(&path)?;
            continue;
        }
        let mut out = new_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("{}: {e}", path.display()))?;
        let executable = entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0);
        set_mode(&path, if executable { 0o755 } else { 0o644 })?;
    }
    set_mode(to, 0o755)
}

/// The link at `path` (`relative` in the folder) to `target`, which must be
/// relative and stay inside the folder.
fn link(path: &Path, relative: &Path, target: &str) -> Result<(), String> {
    let target = Path::new(target);
    let mut depth = relative.components().count().saturating_sub(1);
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return Err(format!("to {}, outside its folder", target.display())),
        }
    }
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string());
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("this system doesn't unpack".into())
    }
}

fn new_dir(path: &Path, mode: u32) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, mode);
    #[cfg(not(unix))]
    let _ = (&mut builder, mode);
    builder
        .create(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `path` and the folders it's in made, each one this makes readable by
/// all: the mode a folder is made with loses what the umask takes away.
fn make_dirs(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        make_dirs(parent)?;
    }
    match new_dir(path, 0o755) {
        Ok(()) => set_mode(path, 0o755),
        Err(_) if path.is_dir() => Ok(()),
        Err(why) => Err(why),
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// The other versions in `root`, a folder of versions, gone, and what an
/// install that was stopped left: the browser tools use the pinned version
/// only. One that's still running (Windows) stays until the next time.
pub fn prune(root: &Path) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = match name.strip_prefix('.') {
            Some(left) => leftover_of(left).is_some_and(|pid| !running(pid, &entry.path())),
            None => name != VERSION && is_version(&name),
        };
        if stale {
            remove(&entry.path()).map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// The install whose leftover `.<name>` is: `.download-<pid>.zip` or
/// `.unpack-<pid>`.
fn leftover_of(name: &str) -> Option<u32> {
    let pid = ["download-", "unpack-"]
        .iter()
        .find_map(|kind| name.strip_prefix(kind))?;
    pid.strip_suffix(".zip").unwrap_or(pid).parse().ok()
}

/// Whether the install that left `path` still runs: its process is there
/// (Unix), or it changed `path` in the last six hours.
fn running(pid: u32, path: &Path) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        let _ = path;
        // SAFETY: signal 0 only asks whether the process is there.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let changed = std::fs::metadata(path).and_then(|meta| meta.modified());
        changed
            .ok()
            .and_then(|changed| changed.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(6 * 60 * 60))
    }
}

/// Whether `name` is a version's folder: numbers between dots.
fn is_version(name: &str) -> bool {
    name.contains('.')
        && name
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A file or folder of an install's own, removed when it's done with.
pub struct Leftover(pub PathBuf);

impl Drop for Leftover {
    fn drop(&mut self) {
        let _ = remove(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pinned() -> &'static Build {
        Build::for_system("linux", "x86_64").unwrap()
    }

    #[test]
    fn only_the_pinned_build_passes() {
        let sha256 = |text: &str| Sha256::digest(text.as_bytes()).to_vec();
        let wrong = verify("u", pinned().size, &sha256("other"), pinned()).unwrap_err();
        assert!(wrong.contains("isn't the build Rovik pins"), "{wrong}");
        let bytes: Vec<u8> = pinned()
            .sha256
            .as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(verify("u", pinned().size, &bytes, pinned()), Ok(()));
        assert!(verify("u", pinned().size - 1, &bytes, pinned()).is_err());
    }

    /// An archive like Chrome for Testing's: files with their modes, folders
    /// (names ending in `/`), and links (`(name, Some(target))` in `links`).
    fn archive(dir: &Path, entries: &[(&str, u32)], links: &[(&str, &str)]) -> PathBuf {
        let path = dir.join("archive.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, mode) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
                continue;
            }
            zip.start_file(*name, options.unix_permissions(*mode))
                .unwrap();
            zip.write_all(name.as_bytes()).unwrap();
        }
        for (name, target) in links {
            zip.add_symlink(*name, *target, options).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn an_archive_is_unpacked_out_of_its_folder_readable_by_all() {
        let dir = crate::testing::temp_dir();
        let zip = archive(
            &dir.0,
            &[
                ("chrome-linux64/", 0),
                ("chrome-linux64/chrome", 0o755),
                ("chrome-linux64/deb.deps", 0o600),
                ("chrome-linux64/locales/en-US.pak", 0o644),
            ],
            &[],
        );
        let to = dir.0.join("155");
        unpack(&zip, &to, "chrome-linux64").unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("chrome")).unwrap(),
            "chrome-linux64/chrome"
        );
        #[cfg(unix)]
        {
            assert_eq!(mode(&to), 0o755);
            assert_eq!(mode(&to.join("chrome")), 0o755);
            assert_eq!(mode(&to.join("deb.deps")), 0o644);
            assert_eq!(mode(&to.join("locales")), 0o755);
            assert_eq!(mode(&to.join("locales/en-US.pak")), 0o644);
        }
    }

    /// A macOS bundle's framework links to its current version.
    #[cfg(unix)]
    #[test]
    fn a_bundles_links_inside_it_are_kept() {
        let dir = crate::testing::temp_dir();
        let framework = "chrome-mac-arm64/A.app/Contents/Frameworks/F.framework";
        let zip = archive(
            &dir.0,
            &[(&format!("{framework}/Versions/1.0/Resources/r.pak"), 0o644)],
            &[
                (&format!("{framework}/Versions/Current"), "1.0"),
                (
                    &format!("{framework}/Resources"),
                    "Versions/Current/Resources",
                ),
            ],
        );
        let to = dir.0.join("155");
        unpack(&zip, &to, "chrome-mac-arm64").unwrap();
        let framework = to.join("A.app/Contents/Frameworks/F.framework");
        assert_eq!(
            std::fs::read_link(framework.join("Versions/Current")).unwrap(),
            Path::new("1.0")
        );
        assert_eq!(
            std::fs::read_to_string(framework.join("Resources/r.pak")).unwrap(),
            "chrome-mac-arm64/A.app/Contents/Frameworks/F.framework/Versions/1.0/Resources/r.pak"
        );
    }

    #[test]
    fn nothing_in_an_archive_leads_out_of_its_folder() {
        for bad in ["elsewhere/chrome", "chrome-linux64/../../escaped"] {
            let dir = crate::testing::temp_dir();
            let zip = archive(&dir.0, &[(bad, 0o755)], &[]);
            let why = unpack(&zip, &dir.0.join("155"), "chrome-linux64").unwrap_err();
            assert!(why.contains("isn't in chrome-linux64"), "{why}");
            assert!(!dir.0.join("escaped").exists());
        }
        for target in ["/bin/sh", "../../../etc", "a/../../.."] {
            let dir = crate::testing::temp_dir();
            let zip = archive(&dir.0, &[], &[("chrome-linux64/sub/chrome", target)]);
            let why = unpack(&zip, &dir.0.join("155"), "chrome-linux64").unwrap_err();
            assert!(why.contains("is a link"), "{target}: {why}");
        }
    }

    #[test]
    fn leftovers_are_known_by_their_installs() {
        assert_eq!(leftover_of("download-12.zip"), Some(12));
        assert_eq!(leftover_of("unpack-34"), Some(34));
        assert_eq!(leftover_of("hidden"), None);
        assert!(is_version("155.0.8059.39"));
        assert!(!is_version("155"));
        assert!(!is_version("notes.txt"));
        assert!(!is_version("1..2"));
    }

    #[test]
    fn other_versions_and_stopped_installs_leftovers_go() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.join("browser");
        let ours = format!(".unpack-{}", std::process::id());
        for name in [VERSION, "150.0.7871.186", "notes", &ours] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        // An install that stopped long ago.
        let stopped = root.join(".download-999999999.zip");
        std::fs::write(&stopped, b"").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&stopped)
            .unwrap()
            .set_modified(old)
            .unwrap();
        prune(&root).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected = vec![ours, VERSION.to_string(), "notes".to_string()];
        expected.sort();
        assert_eq!(left, expected);
    }
}
