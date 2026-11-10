//! Retained standalone images keep Windows installation files unlocked.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

fn digest(file: &mut File) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Publishes a complete content-addressed image without modifying older images.
/// The caller holds the launch lease. Retention is deliberate: another running
/// backend may still need an old image, even if its installed source changed.
pub(super) fn stage(source: &Path, directory: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    if std::fs::symlink_metadata(directory)?
        .file_type()
        .is_symlink()
    {
        return Err(io::Error::other(
            "Backend image directory must not be a link",
        ));
    }
    #[cfg(windows)]
    let mut input = {
        use std::os::windows::fs::OpenOptionsExt;
        // An installer must not change the source midway through this snapshot.
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0)
            .open(source)?
    };
    #[cfg(not(windows))]
    let mut input = File::open(source)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::copy(&mut input, &mut temporary)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let hash = digest(&mut File::open(temporary.path())?)?;
    let target = directory.join(format!("rovik-backend-{hash}.exe"));
    match temporary.persist_noclobber(&target) {
        Ok(_) => Ok(target),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&target)?;
            if !metadata.file_type().is_file() || digest(&mut File::open(&target)?)? != hash {
                return Err(io::Error::other(
                    "Retained backend image is damaged; cannot launch it safely",
                ));
            }
            Ok(target)
        }
        Err(error) => Err(error.error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_changes_do_not_modify_retained_images() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("installed.exe");
        let images = root.path().join("images");
        std::fs::write(&source, b"old executable").unwrap();
        let old = stage(&source, &images).unwrap();
        assert_eq!(stage(&source, &images).unwrap(), old);
        std::fs::write(&source, b"new executable").unwrap();
        let new = stage(&source, &images).unwrap();
        assert_ne!(old, new);
        std::fs::remove_file(source).unwrap();
        assert_eq!(std::fs::read(old).unwrap(), b"old executable");
        assert_eq!(std::fs::read(new).unwrap(), b"new executable");
        assert_eq!(std::fs::read_dir(images).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn an_installation_still_open_for_writing_is_not_snapshotted() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("installed.exe");
        let images = root.path().join("images");
        let mut writer = File::create(&source).unwrap();
        writer.write_all(b"unfinished installation").unwrap();
        assert!(stage(&source, &images).is_err());
        assert_eq!(std::fs::read_dir(&images).unwrap().count(), 0);
        drop(writer);
        assert!(stage(&source, &images).is_ok());
    }

    #[test]
    fn damaged_cached_images_are_refused_without_overwriting_them() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("installed.exe");
        let images = root.path().join("images");
        std::fs::write(&source, b"executable").unwrap();
        let target = stage(&source, &images).unwrap();
        std::fs::write(&target, b"damaged").unwrap();
        assert!(stage(&source, &images).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"damaged");
        assert_eq!(std::fs::read_dir(images).unwrap().count(), 1);
    }
}
