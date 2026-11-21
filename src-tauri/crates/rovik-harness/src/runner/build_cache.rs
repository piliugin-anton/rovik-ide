//! Controller-private compiler entries for repeated preparation checks.
//! Archive bytes are interpreted only inside a disposable offline sandbox.
use super::*;
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

const LIMIT: usize = 144 * 1024 * 1024;

pub(super) struct TrialCache {
    path: PathBuf,
    key: Digest,
}

impl TrialCache {
    pub(super) fn new(
        ledger: &Ledger,
        lease: &Lease,
        stage: &str,
        observation: &Observation,
        command: &str,
    ) -> Result<Self> {
        let directory =
            crate::artifacts::private_directory(&ledger.root().join("trial-compiler-cache"))?;
        let slot = Digest::of_json(&(
            &lease.run_id,
            stage,
            command,
            &observation.configuration_digest,
        ))?;
        let key = Digest::of_json(&(
            &observation.candidate_digest,
            &observation.oracle_digest,
            &observation.environment_digest,
            &observation.command_digest,
            &observation.parameters,
        ))?;
        Ok(Self {
            path: directory.join(slot.hex()),
            key,
        })
    }

    pub(super) fn seed(&self, dependencies: &Path) -> Result<()> {
        let mut input = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut header = [0; 64];
        if input.read_exact(&mut header).is_ok() && header == self.key.hex().as_bytes() {
            let destination = dependencies.join(".rovik-trial-cache.tar");
            let mut output = File::options()
                .create_new(true)
                .write(true)
                .open(destination)?;
            std::io::copy(&mut input.take(LIMIT as u64), &mut output)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(std::fs::Permissions::from_mode(0o444))?;
            }
        }
        Ok(())
    }

    pub(super) fn save(&self, backend: &ContainerBackend, job: &str) {
        let mut cmd = backend.command();
        cmd.args([
            "exec",
            job,
            "/bin/tar",
            "-C",
            "/build/sccache",
            "-cf",
            "-",
            ".",
        ]);
        if let Ok(out) = process(
            cmd,
            Duration::from_secs(15),
            LIMIT,
            &Cancellation::default(),
            || Ok(()),
        ) {
            if out.code == Some(0) && !out.exceeded && !out.timed_out {
                // A failed or interrupted export leaves the previous checkpoint available.
                let _ = self.publish(&out.stdout);
            }
        }
    }

    fn publish(&self, bytes: &[u8]) -> Result<()> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| Error::new("invalid compiler cache path".into()))?;
        // Old trial sessions share one storage budget. Concurrent writers may skip
        // a checkpoint; readers hold an open immutable inode across replacement.
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("publish.lock"))?;
        if lock.try_lock().is_err() {
            return Ok(());
        }
        let mut archives = Vec::new();
        let mut total = bytes.len() as u64 + 64;
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.path() == self.path || entry.file_name().len() != 64 {
                continue;
            }
            let metadata = entry.metadata()?;
            total += metadata.len();
            archives.push((metadata.modified()?, entry.path(), metadata.len()));
        }
        archives.sort_unstable();
        for (_, path, size) in archives {
            if total <= 1024 * 1024 * 1024 {
                break;
            }
            std::fs::remove_file(path)?;
            total -= size;
        }
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(self.key.hex().as_bytes())?;
        file.write_all(bytes)?;
        file.persist(&self.path)
            .map_err(|error| Error::from(error.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_restore_only_the_matching_compiler_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slot");
        let cache = TrialCache {
            path: path.clone(),
            key: Digest::of(b"baseline"),
        };
        cache.publish(b"first").unwrap();
        cache.publish(b"second").unwrap();
        let deps = dir.path().join("deps");
        std::fs::create_dir(&deps).unwrap();
        TrialCache {
            path,
            key: Digest::of(b"changed oracle"),
        }
        .seed(&deps)
        .unwrap();
        assert!(!deps.join(".rovik-trial-cache.tar").exists());
        cache.seed(&deps).unwrap();
        assert_eq!(
            std::fs::read(deps.join(".rovik-trial-cache.tar")).unwrap(),
            b"second"
        );
    }
}
