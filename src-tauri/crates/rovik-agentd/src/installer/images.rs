//! The images the projects' services run, kept once for the whole server
//! (D21): `/var/cache/rovik/images`, an OCI image layout that root fills
//! and every daemon reads. A database's image then comes from its registry
//! once, not once for each user: a registry limits how often one address
//! may fetch anonymously, and every user of a server shares its address.
//!
//! A daemon whose user's Podman lacks an image asks for it through the sudo
//! rule `user *` already allows: `rovik-agentd user image REFERENCE`. The
//! reference is checked here first ([`Reference::parse`]: no other text
//! reaches root), and only an image of a public registry ([`PUBLIC`]) is
//! fetched, without a login: one that needs the user's is theirs to pull.
//! skopeo copies it as the registry serves it, each file checked against
//! its digest, so root never unpacks an image. The daemon's Podman then
//! copies it from here into the user's own storage (services/images.rs).
//!
//! The cache follows the `shared_caches` setting: with that off, each
//! daemon pulls its own images. An image fetched a week ago or more is
//! fetched again when a daemon next asks for it, so a tag that moved on is
//! followed. `admin images prune DAYS` removes the images no daemon asked
//! for in that many days, and `admin images list` says what's here. A
//! `.lock` file holds one change at a time.

use super::{System, ADMIN_FILE, INSTALLED};
use crate::limits::AdminFile;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where the cache is.
pub(crate) const IMAGES: &str = "/var/cache/rovik/images";

/// The registries root fetches images from, anonymously: the public ones
/// most projects' compose files name.
pub(crate) const PUBLIC: &[&str] = &[
    "docker.io",
    "ghcr.io",
    "quay.io",
    "gcr.io",
    "registry.k8s.io",
    "mcr.microsoft.com",
    "public.ecr.aws",
    "registry.gitlab.com",
];

/// How long a fetched image is served before it's fetched again.
const FRESH_FOR: Duration = Duration::from_secs(7 * 86400);

/// How long one fetch may take, skopeo's own timeout (`--command-timeout`).
const FETCH_WITHIN: &str = "15m";

/// An image's reference, as Docker reads one: `postgres:16` is
/// `docker.io/library/postgres:16`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reference {
    /// Its registry: `docker.io`.
    pub(crate) registry: String,
    /// Its repository there: `library/postgres`.
    pub(crate) repository: String,
    /// `:` and its tag, or `@` and its digest, which wins over a tag.
    pub(crate) version: String,
}

impl Reference {
    /// `text` as Docker reads an image's name (distribution's reference
    /// grammar), or why it isn't one.
    pub(crate) fn parse(text: &str) -> Result<Reference, String> {
        let bad = || format!("{text:?} isn't an image's reference");
        let text = text.trim();
        if text.is_empty() || text.len() > 512 {
            return Err(bad());
        }
        let (named, digest) = match text.split_once('@') {
            Some((named, digest)) => (named, Some(digest)),
            None => (text, None),
        };
        if let Some(digest) = digest {
            let hex = digest.strip_prefix("sha256:").ok_or_else(bad)?;
            if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(bad());
            }
        }
        // The tag follows the last colon after the last slash: a colon
        // before it is the registry's port.
        let after_slash = named.rfind('/').map_or(0, |i| i + 1);
        let (name, tag) = match named[after_slash..].rfind(':') {
            Some(i) => (
                &named[..after_slash + i],
                Some(&named[after_slash + i + 1..]),
            ),
            None => (named, None),
        };
        if tag.is_some_and(|tag| !tag_ok(tag)) {
            return Err(bad());
        }
        // A first part with a dot or a port, or `localhost`, is a
        // registry; otherwise the image is Docker Hub's.
        let (registry, path) = match name.split_once('/') {
            Some((first, rest)) if first.contains(['.', ':']) || first == "localhost" => {
                (first, rest)
            }
            _ => ("docker.io", name),
        };
        let registry = match registry {
            "index.docker.io" | "registry-1.docker.io" => "docker.io",
            other => other,
        };
        let repository = match (registry, path.contains('/')) {
            ("docker.io", false) => format!("library/{path}"),
            _ => path.to_string(),
        };
        let docker_part = |part: &str| {
            joined(
                part,
                |b| b.is_ascii_lowercase() || b.is_ascii_digit(),
                |joint| matches!(joint, "." | "_" | "__") || joint.bytes().all(|b| b == b'-'),
            )
        };
        if !registry_ok(registry)
            || !repository.split('/').all(docker_part)
            || registry.len() + 1 + repository.len() > 255
        {
            return Err(bad());
        }
        let version = match (digest, tag) {
            (Some(digest), _) => format!("@{digest}"),
            (None, Some(tag)) => format!(":{tag}"),
            (None, None) => ":latest".to_string(),
        };
        Ok(Reference {
            registry: registry.to_string(),
            repository,
            version,
        })
    }

    /// The reference in full: `docker.io/library/postgres:16`.
    pub(crate) fn full(&self) -> String {
        format!("{}/{}{}", self.registry, self.repository, self.version)
    }

    /// Whether the cache takes it: an image of a public registry, whose
    /// reference is a name an OCI layout can hold (`__` in a name, or a tag
    /// that doesn't end in a letter or digit, isn't).
    pub(crate) fn cacheable(&self) -> bool {
        let oci_part = |part: &str| {
            joined(
                part,
                |b| b.is_ascii_alphanumeric(),
                |joint| joint == "--" || (joint.len() == 1 && "-._:@+".contains(joint)),
            )
        };
        PUBLIC.contains(&self.registry.as_str()) && self.full().split('/').all(oci_part)
    }
}

/// Whether `part` is words of `word` bytes, joined by what `joint` takes,
/// starting and ending with a word.
fn joined(part: &str, word: fn(u8) -> bool, joint: fn(&str) -> bool) -> bool {
    let bytes = part.as_bytes();
    if !bytes.first().is_some_and(|&b| word(b)) || !bytes.last().is_some_and(|&b| word(b)) {
        return false;
    }
    let mut from = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (word(b), from) {
            (false, None) => from = Some(i),
            (true, Some(start)) => {
                if !joint(&part[start..i]) {
                    return false;
                }
                from = None;
            }
            _ => {}
        }
    }
    true
}

/// A tag: a letter, digit or `_`, then up to 127 of them, dots and dashes.
fn tag_ok(tag: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut chars = tag.chars();
    tag.len() <= 128 && chars.next().is_some_and(word) && chars.all(|c| word(c) || ".-".contains(c))
}

/// A registry: a host name, with a port or not.
fn registry_ok(registry: &str) -> bool {
    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    };
    let label = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    host.split('.').all(label)
        && port.is_none_or(|port| {
            (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
        })
}

/// When each image was fetched and last asked for, in seconds since 1970:
/// `asked.json` in the cache.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Asked {
    #[serde(default)]
    images: BTreeMap<String, Times>,
}

/// One image's times.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct Times {
    /// When root last fetched it.
    #[serde(default)]
    fetched: u64,
    /// When a daemon last asked for it.
    #[serde(default)]
    asked: u64,
}

impl Asked {
    /// The cache's, or none yet.
    fn read(dir: &Path) -> Asked {
        std::fs::read_to_string(dir.join("asked.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Written whole, for the daemons to read.
    fn write(&self, sys: &System) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        sys.write(&format!("{IMAGES}/asked.json"), text.as_bytes(), 0o644)
    }
}

/// Seconds since 1970, now.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// `user image REFERENCE`, through sudo: the image in the cache, fetched
/// as root when it isn't there or was fetched long enough ago. The
/// reference is checked here, before root uses it, and the settings say
/// whether the server caches images at all.
pub(super) fn fetch(sys: &System, words: &str) -> Result<(), String> {
    let file = AdminFile::read(&sys.path(ADMIN_FILE))?.unwrap_or_default();
    if !file.toolchains.shared_caches {
        return Err(
            "An administrator turned the shared caches off here: your daemon pulls the \
             images itself."
                .into(),
        );
    }
    let image = Reference::parse(words)?;
    let full = image.full();
    if !image.cacheable() {
        return Err(format!(
            "{full} isn't an image the server caches: it caches those of {} by their plain \
             names. Your daemon pulls it itself.",
            PUBLIC.join(", ")
        ));
    }
    let dir = sys.path(IMAGES);
    if !dir.is_dir() {
        return Err(format!(
            "There's no image cache here yet. An administrator makes it with: sudo \
             {INSTALLED} admin toolchains setup"
        ));
    }
    if sys.live() && !sys.has("skopeo") {
        return Err(format!(
            "skopeo, which fetches the images, isn't installed here. An administrator \
             installs it with: sudo {INSTALLED} admin containers"
        ));
    }
    let _lock = super::flock_file(&dir.join(".lock"))?;
    let mut asked = Asked::read(&dir);
    let now = now();
    let times = asked.images.get(&full).copied().unwrap_or_default();
    let fresh = now.saturating_sub(times.fetched) < FRESH_FOR.as_secs();
    let mut times = Times {
        asked: now,
        ..times
    };
    if !(fresh && held(&dir, &full)) {
        sys.run(
            "skopeo",
            &[
                "--command-timeout",
                FETCH_WITHIN,
                "copy",
                "--quiet",
                "--retry-times",
                "2",
                &format!("docker://{full}"),
                &format!("oci:{IMAGES}:{full}"),
            ],
        )?;
        times.fetched = now;
    }
    asked.images.insert(full.clone(), times);
    asked.write(sys)?;
    println!("{full} is in the server's image cache.");
    Ok(())
}

/// The cache's `index.json`: each image it holds, by name.
fn index(dir: &Path) -> Value {
    std::fs::read_to_string(dir.join("index.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({"schemaVersion": 2, "manifests": []}))
}

/// The name an index entry gives its image.
fn name_of(entry: &Value) -> Option<&str> {
    entry["annotations"]["org.opencontainers.image.ref.name"].as_str()
}

/// Whether the cache at `dir` holds the image `full`.
fn held(dir: &Path, full: &str) -> bool {
    let index = index(dir);
    let entries = index["manifests"].as_array().cloned().unwrap_or_default();
    entries.iter().any(|entry| name_of(entry) == Some(full))
}

/// The file of the blob `digest` (`sha256:…`) in the cache at `dir`.
fn blob(dir: &Path, digest: &str) -> Option<std::path::PathBuf> {
    let hex = digest.strip_prefix("sha256:")?;
    hex.bytes()
        .all(|b| b.is_ascii_hexdigit())
        .then(|| dir.join("blobs/sha256").join(hex))
}

/// The blobs an image is: its manifest, and the config and layers it names
/// (an index's manifests' too), with their sizes.
fn blobs_of(dir: &Path, manifest: &str) -> BTreeMap<String, u64> {
    let mut blobs = BTreeMap::new();
    let mut manifests = vec![manifest.to_string()];
    while let Some(digest) = manifests.pop() {
        let Some(file) = blob(dir, &digest) else {
            continue;
        };
        let size = std::fs::metadata(&file)
            .map(|m| m.len())
            .unwrap_or_default();
        if blobs.insert(digest, size).is_some() {
            continue;
        }
        let Some(read) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        let named =
            std::iter::once(&read["config"]).chain(read["layers"].as_array().into_iter().flatten());
        for part in named {
            if let (Some(digest), size) = (part["digest"].as_str(), part["size"].as_u64()) {
                blobs.insert(digest.to_string(), size.unwrap_or_default());
            }
        }
        for inner in read["manifests"].as_array().into_iter().flatten() {
            if let Some(digest) = inner["digest"].as_str() {
                manifests.push(digest.to_string());
            }
        }
    }
    blobs
}

/// `admin images list`: each image in the cache, its size and when a daemon
/// last asked for it.
pub(super) fn list(sys: &System) -> Result<(), String> {
    let dir = sys.path(IMAGES);
    let index = index(&dir);
    let asked = Asked::read(&dir);
    let entries = index["manifests"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        println!("The image cache is empty: a daemon fills it when a project's service needs one.");
        return Ok(());
    }
    for entry in &entries {
        let (Some(name), Some(digest)) = (name_of(entry), entry["digest"].as_str()) else {
            continue;
        };
        let size: u64 = blobs_of(&dir, digest).values().sum();
        let when = asked
            .images
            .get(name)
            .map(|times| UNIX_EPOCH + Duration::from_secs(times.asked));
        let when = when.map_or("never".to_string(), super::toolchains::age);
        println!(
            "{name}: {} MB, last asked for {when}",
            size.div_ceil(1 << 20)
        );
    }
    Ok(())
}

/// `admin images prune DAYS`: the images no daemon asked for in `days`
/// days gone from the cache, and the files only they used.
pub(super) fn prune(sys: &System, days: u64) -> Result<(), String> {
    let dir = sys.path(IMAGES);
    if !dir.is_dir() {
        println!("There's no image cache here.");
        return Ok(());
    }
    let _lock = super::flock_file(&dir.join(".lock"))?;
    let mut asked = Asked::read(&dir);
    let since = now().saturating_sub(days * 86400);
    let mut index = index(&dir);
    let entries = index["manifests"].as_array().cloned().unwrap_or_default();
    let (kept, gone): (Vec<Value>, Vec<Value>) = entries.into_iter().partition(|entry| {
        name_of(entry)
            .and_then(|name| asked.images.get(name))
            .is_some_and(|times| times.asked >= since)
    });
    index["manifests"] = Value::Array(kept.clone());
    let text = serde_json::to_string(&index).map_err(|e| e.to_string())?;
    sys.write(&format!("{IMAGES}/index.json"), text.as_bytes(), 0o644)?;
    for entry in &gone {
        if let Some(name) = name_of(entry) {
            asked.images.remove(name);
        }
    }
    asked.write(sys)?;
    // What the images still here are made of stays; the rest goes.
    let mut needed: HashSet<String> = HashSet::new();
    for entry in &kept {
        if let Some(digest) = entry["digest"].as_str() {
            needed.extend(blobs_of(&dir, digest).into_keys());
        }
    }
    let mut freed = 0;
    let files = std::fs::read_dir(dir.join("blobs/sha256"))
        .into_iter()
        .flatten()
        .flatten();
    for file in files {
        let digest = format!("sha256:{}", file.file_name().to_string_lossy());
        if needed.contains(&digest) {
            continue;
        }
        freed += file.metadata().map(|m| m.len()).unwrap_or_default();
        std::fs::remove_file(file.path()).map_err(|e| format!("{}: {e}", file.path().display()))?;
    }
    println!(
        "{} image(s) gone from the cache, {} MB freed.",
        gone.len(),
        freed.div_ceil(1 << 20)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_is_read_as_docker_reads_it() {
        let full = |text: &str| Reference::parse(text).map(|r| r.full());
        assert_eq!(
            full("postgres"),
            Ok("docker.io/library/postgres:latest".into())
        );
        assert_eq!(
            full(" redis:7-alpine "),
            Ok("docker.io/library/redis:7-alpine".into())
        );
        assert_eq!(
            full("bitnami/redis:7"),
            Ok("docker.io/bitnami/redis:7".into())
        );
        assert_eq!(
            full("index.docker.io/library/nginx"),
            Ok("docker.io/library/nginx:latest".into())
        );
        assert_eq!(
            full("ghcr.io/org/app:1.2.3"),
            Ok("ghcr.io/org/app:1.2.3".into())
        );
        assert_eq!(
            full("localhost:5000/team/db:dev"),
            Ok("localhost:5000/team/db:dev".into()),
            "a port is the registry's, not a tag"
        );
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            full(&format!("postgres:16@{digest}")),
            Ok(format!("docker.io/library/postgres@{digest}")),
            "the digest wins over the tag"
        );
        let parsed = Reference::parse("quay.io/coreos/etcd:v3.5").unwrap();
        assert_eq!(
            (
                parsed.registry.as_str(),
                parsed.repository.as_str(),
                parsed.version.as_str()
            ),
            ("quay.io", "coreos/etcd", ":v3.5")
        );
    }

    /// Nothing but a reference reaches root: no options, no paths, no
    /// shell words, no uppercase name.
    #[test]
    fn anything_else_is_refused() {
        for text in [
            "",
            "--src-creds=x",
            "-v",
            "Postgres",
            "postgres:",
            "postgres:-x",
            "postgres:16 --all",
            "postgres;id",
            "../etc/passwd",
            "docker.io//postgres",
            "docker.io/library/postgres@sha256:abc",
            "docker.io/library/postgres@md5:0123",
            "a..b/c",
            "x/y-",
            &format!("postgres:{}", "1".repeat(129)),
        ] {
            assert!(Reference::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn only_a_public_registrys_images_with_plain_names_are_cached() {
        let cacheable = |text: &str| Reference::parse(text).unwrap().cacheable();
        assert!(cacheable("postgres:16-alpine"));
        assert!(cacheable("mcr.microsoft.com/mssql/server:2022-latest"));
        assert!(
            !cacheable("localhost:5000/team/db:dev"),
            "the user's own registry"
        );
        assert!(
            !cacheable("registry.example.com/private/db:1"),
            "maybe needs a login"
        );
        assert!(!cacheable("my__org/db:1"), "an OCI layout can't name it");
        assert!(!cacheable("postgres:16-"), "nor this tag");
    }

    /// A cache of two images that share a layer, as skopeo writes one.
    fn cache() -> (rovik_core::testing::TempDir, System) {
        let dir = rovik_core::testing::temp_dir();
        let images = dir.0.join(IMAGES.trim_start_matches('/'));
        let blobs = images.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        let put = |name: &str, bytes: &str| {
            let digest = format!("{:0>64}", name);
            std::fs::write(blobs.join(&digest), bytes).unwrap();
            format!("sha256:{digest}")
        };
        let shared = put("1", "shared layer");
        let mut index = Vec::new();
        for (name, own) in [
            ("docker.io/library/redis:7", "2"),
            ("docker.io/library/postgres:16", "3"),
        ] {
            let config = put(&format!("{own}c"), "config");
            let layer = put(&format!("{own}1"), "own layer");
            let manifest = serde_json::json!({
                "schemaVersion": 2,
                "config": {"digest": config, "size": 6},
                "layers": [{"digest": shared, "size": 12}, {"digest": layer, "size": 9}]
            });
            let manifest = put(&format!("{own}f"), &manifest.to_string());
            index.push(serde_json::json!({
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": manifest,
                "annotations": {"org.opencontainers.image.ref.name": name}
            }));
        }
        let index = serde_json::json!({"schemaVersion": 2, "manifests": index});
        std::fs::write(images.join("index.json"), index.to_string()).unwrap();
        let sys = System::new(Some(dir.0.clone()));
        (dir, sys)
    }

    #[test]
    fn pruning_takes_the_images_nobody_asked_for_and_only_their_own_files() {
        let (dir, sys) = cache();
        let images = sys.path(IMAGES);
        let mut asked = Asked::default();
        asked.images.insert(
            "docker.io/library/redis:7".into(),
            Times {
                fetched: now(),
                asked: now(),
            },
        );
        asked.images.insert(
            "docker.io/library/postgres:16".into(),
            Times {
                fetched: 1,
                asked: now() - 40 * 86400,
            },
        );
        asked.write(&sys).unwrap();
        assert!(held(&images, "docker.io/library/postgres:16"));

        prune(&sys, 30).unwrap();
        assert!(held(&images, "docker.io/library/redis:7"));
        assert!(!held(&images, "docker.io/library/postgres:16"));
        let left: HashSet<String> = std::fs::read_dir(images.join("blobs/sha256"))
            .unwrap()
            .map(|f| {
                f.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .trim_start_matches('0')
                    .to_string()
            })
            .collect();
        assert_eq!(
            left,
            ["1", "2c", "21", "2f"].map(String::from).into(),
            "the shared layer stays with redis; postgres's own files go"
        );
        assert!(!Asked::read(&images)
            .images
            .contains_key("docker.io/library/postgres:16"));
        drop(dir);
    }

    /// Without a time asked for, an image is taken as never asked for.
    #[test]
    fn an_image_with_no_time_goes_first() {
        let (_dir, sys) = cache();
        prune(&sys, 365).unwrap();
        let images = sys.path(IMAGES);
        assert!(!held(&images, "docker.io/library/redis:7"));
        assert!(std::fs::read_dir(images.join("blobs/sha256"))
            .unwrap()
            .next()
            .is_none());
    }
}
