//! Docker's official installer and checksum, fetched before any native installation.
use super::DockerProgress;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, time::Duration};

const MAX_INSTALLER: u64 = 4 * 1024 * 1024 * 1024;

pub(super) struct Build {
    pub filename: &'static str,
    base: String,
}
impl Build {
    pub fn for_platform(os: &str, arch: &str) -> Option<Self> {
        let arch = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => return None,
        };
        let (os, filename) = match os {
            "macos" => ("mac", "Docker.dmg"),
            "windows" => ("win", "Docker Desktop Installer.exe"),
            _ => return None,
        };
        Some(Self {
            filename,
            base: format!("https://desktop.docker.com/{os}/main/{arch}"),
        })
    }
    fn url(&self) -> String {
        format!("{}/{}", self.base, self.filename.replace(' ', "%20"))
    }
}

fn checksum(text: &str, filename: &str) -> Result<String, String> {
    let mut matches = text.lines().filter_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == filename).then_some(hash)
    });
    let value = matches
        .next()
        .ok_or("Docker's installer checksum is missing")?;
    if matches.next().is_some()
        || value.len() != 64
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Docker's installer checksum is invalid or ambiguous".into());
    }
    Ok(value.to_ascii_lowercase())
}

pub(super) async fn download(
    build: &Build,
    to: &Path,
    progress: impl Fn(DockerProgress),
) -> Result<(), String> {
    progress(DockerProgress::stage(
        "downloading",
        "Downloading Docker Desktop from Docker’s official site…",
    ));
    let client = rovik_core::net::builder()
        .await
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(format!("{}/checksums.txt", build.base))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut body = response.bytes_stream();
    let mut metadata = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if metadata.len() + chunk.len() > 16 * 1024 {
            return Err("Docker checksum metadata exceeds its limit".into());
        }
        metadata.extend_from_slice(&chunk);
    }
    let expected = checksum(
        std::str::from_utf8(&metadata).map_err(|e| e.to_string())?,
        build.filename,
    )?;
    let response = client
        .get(build.url())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let total = response.content_length();
    if total.is_some_and(|length| length > MAX_INSTALLER) {
        return Err("Docker installer exceeds its download limit".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(to).map_err(|e| e.to_string())?;
    let (mut digest, mut size, mut told) = (Sha256::new(), 0_u64, 0_u64);
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or("Docker download size overflow")?;
        if size > MAX_INSTALLER {
            return Err("Docker installer exceeds its download limit".into());
        }
        digest.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        if size - told >= 256 * 1024 || Some(size) == total {
            told = size;
            progress(DockerProgress {
                phase: "downloading".into(),
                message: "Downloading Docker Desktop…".into(),
                downloaded: size,
                total,
            });
        }
    }
    progress(DockerProgress::stage(
        "verifying",
        "Verifying the Docker installer…",
    ));
    let actual: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    verify(size, total, &actual, &expected)?;
    file.sync_all().map_err(|e| e.to_string())
}

fn verify(size: u64, total: Option<u64>, actual: &str, expected: &str) -> Result<(), String> {
    if size == 0 || total.is_some_and(|total| total != size) || actual != expected {
        Err(
            "Docker installer integrity check failed. Nothing was installed; retry the download."
                .into(),
        )
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installers_only_come_from_the_official_platform_paths() {
        assert_eq!(
            Build::for_platform("windows", "aarch64").unwrap().url(),
            "https://desktop.docker.com/win/main/arm64/Docker%20Desktop%20Installer.exe"
        );
        assert_eq!(
            Build::for_platform("macos", "x86_64").unwrap().url(),
            "https://desktop.docker.com/mac/main/amd64/Docker.dmg"
        );
        assert!(Build::for_platform("linux", "x86_64").is_none());
        assert!(Build::for_platform("windows", "../amd64").is_none());
    }
    #[test]
    fn missing_duplicate_malformed_and_wrong_file_checksums_are_rejected() {
        let hash = "a".repeat(64);
        assert_eq!(
            checksum(
                &format!("{hash} *Docker Desktop Installer.exe\n"),
                "Docker Desktop Installer.exe"
            )
            .unwrap(),
            hash
        );
        for text in [
            "".into(),
            format!("{hash} *another.exe"),
            "invalid *Docker.dmg".into(),
            format!("{hash} *Docker.dmg\n{hash} *Docker.dmg"),
        ] {
            assert!(checksum(&text, "Docker.dmg").is_err());
        }
        assert!(verify(10, Some(10), &hash, &hash).is_ok());
        assert!(verify(0, None, &hash, &hash).is_err());
        assert!(verify(9, Some(10), &hash, &hash).is_err());
        assert!(verify(10, Some(10), &"b".repeat(64), &hash).is_err());
    }
}
