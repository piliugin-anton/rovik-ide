//! A service's image through the server's image cache (D21,
//! installer/images.rs): where this user's Podman lacks it, the daemon asks
//! root to fetch it into the cache (`user image`, through the sudo rule
//! every user has), then copies it from there into the user's storage, as
//! a pull from its registry would. An image the cache doesn't take, or one
//! that couldn't be fetched or copied, is left to Compose, which pulls it
//! itself (`--pull missing`): the cache saves a fetch, and never stops a
//! service.

use super::podman::Podman;
use crate::installer::images::Reference;
use crate::installer::INSTALLED;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// How long root's fetch may take: skopeo's own limit, and a little.
const FETCH_WITHIN: Duration = Duration::from_secs(16 * 60);

/// How long copying an image from the cache may take: unpacking a big one.
const COPY_WITHIN: Duration = Duration::from_secs(10 * 60);

/// The server's image cache, as this daemon uses it.
pub struct Cache {
    /// Its folder, `/var/cache/rovik/images`.
    dir: PathBuf,
}

impl Cache {
    /// The cache in `dir`.
    pub fn new(dir: PathBuf) -> Cache {
        Cache { dir }
    }

    /// Each of `images` in `podman`'s storage, through the cache, where it
    /// takes it and the image isn't there yet. What doesn't work is logged:
    /// Compose then pulls the image itself.
    pub async fn bring(&self, podman: &Podman, images: &[String]) {
        for image in images {
            let Ok(reference) = Reference::parse(image) else {
                continue;
            };
            if !reference.cacheable() {
                continue;
            }
            let full = reference.full();
            if podman.podman(&["image", "exists", &full]).await.is_ok() {
                continue;
            }
            let brought = match fetch(&full).await {
                Ok(()) => self.import(podman, &full).await,
                Err(why) => Err(why),
            };
            if let Err(why) = brought {
                log::info!("services: {full} isn't from the image cache, Compose pulls it: {why}");
            }
        }
    }

    /// `full`, which the cache holds, copied into `podman`'s storage under
    /// its name.
    async fn import(&self, podman: &Podman, full: &str) -> Result<(), String> {
        let source = format!("oci:{}:{full}", self.dir.display());
        let id = podman
            .podman_within(&["pull", "--quiet", &source], COPY_WITHIN)
            .await?;
        // Podman names it by its name in the cache, which is this one; the
        // tag makes sure of it.
        podman.podman(&["tag", id.trim(), full]).await.map(drop)
    }
}

/// `sudo -n rovik-agentd user image FULL`: root's fetch into the cache, or
/// what it said when it couldn't.
async fn fetch(full: &str) -> Result<(), String> {
    let mut command = rovik_core::process::command("sudo");
    command
        .args(["-n", INSTALLED, "user", "image", full])
        .stdin(Stdio::null());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let out = tokio::time::timeout(FETCH_WITHIN, command.output())
        .await
        .map_err(|_| {
            format!(
                "root's fetch took more than {} minutes",
                FETCH_WITHIN.as_secs() / 60
            )
        })?
        .map_err(|e| format!("sudo {INSTALLED} user image: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(super::podman::last_lines(
            &format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            4,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// For real: an image skopeo put in a cache, as root's fetch does,
    /// copied into a user's Podman under its name, and Podman finds it by
    /// the short name a compose file gives. Run with `ROVIK_REAL_PODMAN=1`,
    /// as a user whose Podman runs rootless, where skopeo is installed and
    /// Docker Hub can be reached.
    #[tokio::test]
    #[ignore]
    async fn an_image_in_the_cache_is_copied_into_the_users_podman() {
        if std::env::var("ROVIK_REAL_PODMAN").as_deref() != Ok("1") {
            return;
        }
        let dir = rovik_core::testing::temp_dir();
        let cache = dir.0.join("images");
        let full = "docker.io/library/alpine:3.20";
        let fetched = std::process::Command::new("skopeo")
            .args(["copy", "--quiet", "--retry-times", "2"])
            .arg(format!("docker://{full}"))
            .arg(format!("oci:{}:{full}", cache.display()))
            .status()
            .expect("skopeo");
        assert!(fetched.success());
        let podman = Podman::new(dir.0.join("containers"), PathBuf::from("/bin/true"));
        Cache::new(cache).import(&podman, full).await.unwrap();
        for name in [full, "alpine:3.20"] {
            podman.podman(&["image", "exists", name]).await.unwrap();
        }
    }
}
