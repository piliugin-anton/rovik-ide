//! The Rust and Node versions every user's chats on this server run, and
//! the package caches they share (docs/cloud-hosts.md, "Shared toolchains
//! and caches"). One copy of each version, under `/opt/rovik/toolchains`,
//! owned by root and readable by everyone; npm's cache and the crates the
//! daemons have checked, under `/var/cache/rovik`, which the `rovik` group
//! writes. `node_modules` and `target/` stay in each chat's own worktree.
//!
//! Installation is root's, and a user asks for it through the sudo rule
//! `user *` already allows: `rovik-agentd user toolchain KIND VERSION
//! [-c COMPONENT] [-t TARGET]`. What root downloads is named by words it
//! checked first ([`parse_version`]: no other text reaches it), from
//! rust-lang.org and nodejs.org only, and is checked against what those
//! servers publish before anything uses it. A `.lock` file holds one
//! install at a time.
//!
//! An administrator can turn user installs off, and name the Rust
//! versions they allow, in `agentd.toml` ([`crate::limits`]); their own
//! `admin toolchain add` works either way, and `admin toolchain prune`
//! removes the versions no `.used` file has touched for some days.

use super::{unit, System, CACHES, SHARE_GROUP, TOOLCHAINS, UNIT};
use rovik_core::pinned_browser::Leftover;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A toolchain: Rust, through one shared rustup, or Node, from the
/// official tarballs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Rust,
    Node,
}

impl Kind {
    /// `rust` or `node`, as a command words it.
    pub(super) fn parse(word: &str) -> Result<Kind, String> {
        match word {
            "rust" => Ok(Kind::Rust),
            "node" => Ok(Kind::Node),
            other => Err(format!("{other} isn't a toolchain: say rust or node.")),
        }
    }

    /// The word a command names it by, and its folder under `TOOLCHAINS`.
    pub(super) fn name(self) -> &'static str {
        match self {
            Kind::Rust => "rust",
            Kind::Node => "node",
        }
    }

    /// For a message.
    fn title(self) -> &'static str {
        match self {
            Kind::Rust => "Rust",
            Kind::Node => "Node",
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * The words root accepts
 * ---------------------------------------------------------------------------
 */

/// The version `words` names, in the form the folders use, or why it isn't
/// one. Everything that reaches root comes through here: a chat can't pass
/// it text of its own, only a version of the shape the kind has.
pub(crate) fn parse_version(kind: Kind, words: &str) -> Result<String, String> {
    let version = words.trim();
    let accepted = match kind {
        Kind::Rust => rust_version(version),
        Kind::Node => node_version(version),
    };
    match accepted {
        true => Ok(version.to_string()),
        false => Err(format!(
            "{version:?} isn't a {} version Rovik installs: {}.",
            kind.name(),
            match kind {
                Kind::Rust => "say stable, beta, nightly, nightly-YYYY-MM-DD, 1.N or 1.N.P",
                Kind::Node => "say lts, a major version such as 20, or X.Y.Z",
            }
        )),
    }
}

/// `stable`, `beta`, `nightly`, `nightly-YYYY-MM-DD`, or a release's
/// numbers (`1.82`, `1.82.0`).
fn rust_version(version: &str) -> bool {
    match version {
        "stable" | "beta" | "nightly" => return true,
        _ => {}
    }
    if let Some(date) = version.strip_prefix("nightly-") {
        let parts: Vec<&str> = date.split('-').collect();
        return parts.len() == 3
            && parts
                .iter()
                .map(|part| part.len())
                .eq([4, 2, 2])
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    }
    dotted(version, 2) || dotted(version, 3)
}

/// `lts`, a major version, or `X.Y.Z`.
fn node_version(version: &str) -> bool {
    version == "lts" || dotted(version, 1) || dotted(version, 3)
}

/// Exactly `parts` numbers between dots, each non-empty: `18` is one,
/// `1.82.0` three.
fn dotted(version: &str, parts: usize) -> bool {
    let numbers: Vec<&str> = version.split('.').collect();
    numbers.len() == parts
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `name` is a component or a target rustup takes: lowercase
/// letters, digits, dashes and underscores, as in `rustfmt` or
/// `x86_64-unknown-linux-gnu`.
fn checked_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
        })
}

/// The `-c COMPONENT` and `-t TARGET` pairs of a `user toolchain` command,
/// each name checked before root sees it.
pub(super) fn flags(rest: &[&str]) -> Result<(Vec<String>, Vec<String>), String> {
    let (mut components, mut targets) = (Vec::new(), Vec::new());
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let Some(name) = it.next() else {
            return Err(format!("{flag} needs a name"));
        };
        let name = name.to_string();
        if !checked_name(&name) {
            return Err(format!("{name:?} isn't a component or target name."));
        }
        match *flag {
            "-c" => components.push(name),
            "-t" => targets.push(name),
            other => {
                return Err(format!(
                    "{other} isn't an option of user toolchain: -c COMPONENT or -t TARGET"
                ))
            }
        }
    }
    Ok((components, targets))
}

/*
 * ---------------------------------------------------------------------------
 * Where they are
 * ---------------------------------------------------------------------------
 */

/// A version's folder: `TOOLCHAINS/node/<version>`; Node's alone, since
/// Rust's are rustup's.
fn version_dir(sys: &System, kind: Kind, version: &str) -> PathBuf {
    sys.path(&format!("{TOOLCHAINS}/{}/{version}", kind.name()))
}

/// What says a version was used, `prune` reads it: `TOOLCHAINS/<kind>/
/// <version>.used`.
fn used_file(sys: &System, kind: Kind, version: &str) -> PathBuf {
    sys.path(&format!("{TOOLCHAINS}/{}/{version}.used", kind.name()))
}

/// The shared rustup's folders: the toolchains are everyone's to run, the
/// cache of what they download is each user's own (`CARGO_HOME`).
fn rustup_home() -> &'static str {
    "/opt/rovik/toolchains/rust/rustup"
}

fn cargo_home() -> &'static str {
    "/opt/rovik/toolchains/rust/cargo"
}

/*
 * ---------------------------------------------------------------------------
 * Installing one, as root
 * ---------------------------------------------------------------------------
 */

/// `user toolchain …` and `admin toolchain add …`: `kind`'s `version`
/// installed, once, for everyone, with `components` and `targets` for
/// Rust. Nothing is downloaded when it's there already; one install runs
/// at a time.
pub(super) fn ensure(
    sys: &System,
    kind: Kind,
    version: &str,
    components: &[String],
    targets: &[String],
) -> Result<(), String> {
    let version = parse_version(kind, version)?;
    // `lts` and a major version are the newest of them, once.
    let version = match kind {
        Kind::Node => resolve_node(sys, &version)?,
        Kind::Rust => version,
    };
    folder(sys, TOOLCHAINS, 0o755, None)?;
    let _lock = lock(sys)?;
    if installed(sys, kind, &version) {
        touch(sys, kind, &version)?;
        println!("{} {version} is installed.", kind.title());
        return Ok(());
    }
    match kind {
        Kind::Rust => install_rust(sys, &version, components, targets)?,
        Kind::Node => install_node(sys, &version)?,
    }
    if !installed(sys, kind, &version) && sys.live() {
        return Err(format!(
            "{} {version} isn't in {TOOLCHAINS} once installed.",
            kind.title()
        ));
    }
    touch(sys, kind, &version)?;
    let asked = match components.is_empty() && targets.is_empty() {
        true => String::new(),
        false => ", as the project asks for it".to_string(),
    };
    println!(
        "{} {version} is installed for everyone on this server{asked}.",
        kind.title()
    );
    Ok(())
}

/// Whether `kind`'s `version` is in `TOOLCHAINS`: a Node version's folder,
/// or the rustup toolchain named `<version>-<host>`.
fn installed(sys: &System, kind: Kind, version: &str) -> bool {
    installed_at(&sys.path(TOOLCHAINS), kind, version)
}

/// `installed` under `root`, the folder of versions as this host has it.
fn installed_at(root: &Path, kind: Kind, version: &str) -> bool {
    match kind {
        Kind::Node => root.join("node").join(version).is_dir(),
        Kind::Rust => rust_dir(root, version).is_some_and(|dir| dir.is_dir()),
    }
}

/// The folder rustup installs `version` in: `<version>-<host triple>`.
/// Exactly that one: `nightly` isn't `nightly-2024-09-05`, which a prefix
/// would take for it.
fn rust_dir(root: &Path, version: &str) -> Option<PathBuf> {
    let triple = target_triple().ok()?;
    Some(root.join(format!("rust/rustup/toolchains/{version}-{triple}")))
}

/// When a version last ran: its `.used` file's time, or its programs' last
/// access, whichever is newer. The root command writes `.used` when it
/// installs or is asked for a version; a chat whose version is there runs
/// it without asking, and only the access time says so.
fn last_used(root: &Path, kind: Kind, version: &str, used: SystemTime) -> SystemTime {
    let programs: Vec<PathBuf> = match kind {
        Kind::Node => vec![root.join("node").join(version).join("bin/node")],
        Kind::Rust => rust_dir(root, version)
            .map(|dir| vec![dir.join("bin/rustc"), dir.join("bin/cargo")])
            .unwrap_or_default(),
    };
    programs
        .iter()
        .filter_map(|path| std::fs::metadata(path).and_then(|m| m.accessed()).ok())
        .fold(used, SystemTime::max)
}

/// One install at a time: an exclusive `flock` on `TOOLCHAINS/.lock`.
fn lock(sys: &System) -> Result<std::fs::File, String> {
    use std::os::unix::io::AsRawFd;
    // `remove` and `prune` on a server whose folder isn't made yet.
    folder(sys, TOOLCHAINS, 0o755, None)?;
    let path = sys.path(&format!("{TOOLCHAINS}/.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o644)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // SAFETY: flock on a descriptor this function owns, held until the
    // file is closed.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

/// The timestamp `prune` reads, now.
fn touch(sys: &System, kind: Kind, version: &str) -> Result<(), String> {
    let path = used_file(sys, kind, version);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, format!("{now}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// Rust: one shared rustup, which installs the version into its
/// read-only home, and the components and targets the project names.
fn install_rust(
    sys: &System,
    version: &str,
    components: &[String],
    targets: &[String],
) -> Result<(), String> {
    let homes = [("RUSTUP_HOME", rustup_home()), ("CARGO_HOME", cargo_home())];
    let rustup = format!("{}/bin/rustup", cargo_home());
    if !sys.path(&rustup).is_file() {
        let target = target_triple()?;
        let base = format!("https://static.rust-lang.org/rustup/dist/{target}");
        let init = format!("{TOOLCHAINS}/.rustup-init");
        let path = sys.path(&init);
        if sys.live() {
            let url = format!("{base}/rustup-init");
            let expected = fetch_text(&format!("{url}.sha256"))?;
            download(&url, &path, &expected)?;
            std::fs::set_permissions(&path, PermissionsExt::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        } else {
            println!(
                "would download {base}/rustup-init (checked against its SHA-256) \
                 and run it with RUSTUP_HOME={} CARGO_HOME={}",
                homes[0].1, homes[1].1
            );
        }
        let run = sys.run_with(
            &homes,
            &path.display().to_string(),
            &[
                "--no-modify-path",
                "-y",
                "--profile",
                "minimal",
                "--default-toolchain",
                "stable",
            ],
        );
        if sys.live() {
            let _ = std::fs::remove_file(&path);
        }
        run?;
    }
    let mut args: Vec<&str> = vec!["toolchain", "install", version, "--profile", "minimal"];
    for component in components {
        args.push("-c");
        args.push(component);
    }
    for target in targets {
        args.push("-t");
        args.push(target);
    }
    sys.run_with(&homes, &rustup, &args).map(|_| ())
}

/// The architecture rustup's downloads are named for.
fn target_triple() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64-unknown-linux-gnu"),
        "aarch64" => Ok("aarch64-unknown-linux-gnu"),
        other => Err(format!(
            "There are no Rust builds for {other}: Rust installs on x86_64 and aarch64 \
             servers."
        )),
    }
}

/// Node: the official tarball of the exact version, checked against
/// `SHASUMS256.txt`, unpacked beside the versions and then given the
/// version's name, so a folder is there only once all of it is.
fn install_node(sys: &System, version: &str) -> Result<(), String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "There are no Node builds for {other}: Node installs on x86_64 and aarch64 \
                 servers."
            ))
        }
    };
    folder(sys, &format!("{TOOLCHAINS}/node"), 0o755, None)?;
    let name = format!("node-v{version}-linux-{arch}");
    let file_name = format!("{name}.tar.gz");
    let base = format!("https://nodejs.org/dist/v{version}");
    let url = format!("{base}/{file_name}");
    let dir = version_dir(sys, Kind::Node, version);
    if !sys.live() {
        println!(
            "would download {url} (checked against {base}/SHASUMS256.txt) and unpack it in {}",
            dir.display()
        );
        return Ok(());
    }
    let pid = std::process::id();
    let archive = sys.path(&format!("{TOOLCHAINS}/.download-{pid}.tar.gz"));
    let unpacked = sys.path(&format!("{TOOLCHAINS}/.unpack-{pid}"));
    // Both go whatever happens next: an install that stopped leaves
    // nothing behind.
    let _leftovers = Leftover(archive.clone());
    let _unpacked = Leftover(unpacked.clone());
    let expected = fetch_text(&format!("{base}/SHASUMS256.txt"))?;
    let line = expected
        .lines()
        .find(|line| line.split_whitespace().last() == Some(file_name.as_str()))
        .ok_or_else(|| {
            format!("{base}/SHASUMS256.txt names no {file_name}: the download stopped there.")
        })?;
    download(&url, &archive, line)?;
    unpack(&archive, &unpacked)?;
    let from = unpacked.join(&name);
    if !from.is_dir() {
        return Err(format!(
            "{} holds no {name} folder: the archive isn't Node's.",
            unpacked.display()
        ));
    }
    readable_at_most(&from)?;
    match std::fs::rename(&from, &dir) {
        Ok(()) => {}
        // Another administrator's install got there first.
        Err(_) if dir.is_dir() => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    Ok(())
}

/// Everything in `dir` readable by all, executable where the archive's
/// files were: the daemons run them as their own users.
fn readable_at_most(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if meta.is_symlink() {
        // Node's `bin/npm` and its siblings: relative, and inside.
        return Ok(());
    }
    let mode = match meta.is_dir() {
        true => 0o755,
        false => match meta.permissions().mode() & 0o111 {
            0 => 0o644,
            _ => 0o755,
        },
    };
    std::fs::set_permissions(dir, PermissionsExt::from_mode(mode))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if meta.is_dir() {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            readable_at_most(&entry.path())?;
        }
    }
    Ok(())
}

/// A `.tar.gz` unpacked into the new folder `to`. Nothing may lead out of
/// it: the tar crate refuses an absolute path or a `..`.
fn unpack(archive: &Path, to: &Path) -> Result<(), String> {
    let file =
        std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    tar.unpack(to)
        .map_err(|e| format!("{}: {e}", archive.display()))
}

/*
 * ---------------------------------------------------------------------------
 * Downloading what those servers publish
 * ---------------------------------------------------------------------------
 */

/// `url` read as text: a checksum list, or `rustup-init`'s SHA-256.
fn fetch_text(url: &str) -> Result<String, String> {
    runtime()?.block_on(async {
        let response = get(url).await?;
        response
            .text()
            .await
            .map_err(|e| format!("{url}: {e}"))
    })
}

/// `url` downloaded into the new file `to`, refused unless its SHA-256 is
/// the one `expected` names: the first hex word of a checksum list's line,
/// or of `rustup-init.sha256`.
fn download(url: &str, to: &Path, expected: &str) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let wanted = expected
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("{url}: no SHA-256 to check it against"))?
        .to_ascii_lowercase();
    runtime()?.block_on(async {
        let response = get(url).await?;
        let path = to.to_path_buf();
        let mut file = new_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hash = Sha256::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                let _ = std::fs::remove_file(&path);
                format!("{url}: {e}")
            })?;
            hash.update(&chunk);
            file.write_all(&chunk).map_err(|e| {
                let _ = std::fs::remove_file(&path);
                format!("{}: {e}", path.display())
            })?;
        }
        let found: String = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if found != wanted {
            let _ = std::fs::remove_file(&path);
            return Err(format!(
                "{url} isn't what the server publishes: it hashes to {found}, not {wanted}."
            ));
        }
        Ok(())
    })
}

/// A GET, with Settings → Network's timeout and proxy, refused unless the
/// server had it.
async fn get(url: &str) -> Result<reqwest::Response, String> {
    let client = rovik_core::net::builder()
        .await
        .user_agent(concat!("rovik/", env!("CARGO_PKG_VERSION")))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{url}: {e}"))
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

/// A new file only root can read until it's checked.
fn new_file(path: &Path) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Node's LTS or major version as the exact one `index.json` names: the
/// newest of them.
fn resolve_node(sys: &System, version: &str) -> Result<String, String> {
    if dotted(version, 3) {
        return Ok(version.to_string());
    }
    // A dry run downloads nothing: it says what would be picked, as a
    // stand-in for what nodejs.org lists.
    if !sys.live() {
        return Ok(match version {
            "lts" => "22.11.0".to_string(),
            major => format!("{major}.11.0"),
        });
    }
    let index = fetch_text("https://nodejs.org/dist/index.json")?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_str(&index).map_err(|e| format!("nodejs.org/dist/index.json: {e}"))?;
    let matching = |release: &serde_json::Value| -> Option<String> {
        let number = release["version"].as_str()?.strip_prefix('v')?.to_string();
        let wanted = match version {
            "lts" => release["lts"].as_str().is_some(),
            major => number.split('.').next() == Some(major),
        };
        wanted.then_some(number)
    };
    releases
        .iter()
        .find_map(matching)
        .ok_or_else(|| format!("nodejs.org lists no Node {version}."))
}

/*
 * ---------------------------------------------------------------------------
 * What's there, and what goes
 * ---------------------------------------------------------------------------
 */

/// `admin toolchain list`: the versions installed, and how long ago each
/// was used.
pub(super) fn list(sys: &System) -> Result<(), String> {
    let mut any = false;
    for kind in [Kind::Rust, Kind::Node] {
        let versions = versions(sys, kind);
        if versions.is_empty() {
            println!("{}: none", kind.name());
            continue;
        }
        any = true;
        let named: Vec<String> = versions
            .iter()
            .map(|(version, used)| format!("{version} (last used {})", age(*used)))
            .collect();
        println!("{}: {}", kind.name(), named.join(", "));
    }
    if !any {
        println!(
            "No shared toolchains are installed: a chat installs one when its project needs it."
        );
    }
    Ok(())
}

/// Each version installed under `kind`'s folder with the time it was last
/// used: its `.used` file's.
pub(super) fn versions(sys: &System, kind: Kind) -> Vec<(String, SystemTime)> {
    versions_at(&sys.path(TOOLCHAINS), kind)
}

/// `versions` under `root`, the folder of versions as this host has it.
fn versions_at(root: &Path, kind: Kind) -> Vec<(String, SystemTime)> {
    let dir = root.join(kind.name());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut versions: Vec<(String, SystemTime)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_suffix(".used")?.to_string();
            if !installed_at(root, kind, &version) {
                return None;
            }
            let used = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            let used = last_used(root, kind, &version, used);
            Some((version, used))
        })
        .collect();
    versions.sort();
    versions
}

/// The versions installed under `root`, for `doctor`: Rust's and Node's,
/// as this host has them.
pub(crate) fn installed_here(root: &Path) -> (Vec<String>, Vec<String>) {
    let names = |kind: Kind| -> Vec<String> {
        versions_at(root, kind)
            .into_iter()
            .map(|(version, _)| version)
            .collect()
    };
    (names(Kind::Rust), names(Kind::Node))
}

/// How long ago `time` was, in words.
fn age(time: SystemTime) -> String {
    let seconds = SystemTime::now()
        .duration_since(time)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    match seconds {
        0..=3600 => "within the hour".into(),
        _ if seconds < 2 * 86400 => format!("{} hours ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86400),
    }
}

/// `admin toolchain remove KIND VERSION`: the version gone, and what
/// installed it left behind. Node's `20` is the newest `20.x.y` there, as
/// `admin toolchain add node 20` installed it.
pub(super) fn remove(sys: &System, kind: Kind, version: &str) -> Result<(), String> {
    let version = parse_version(kind, version)?;
    let _lock = lock(sys)?;
    let version = match kind {
        Kind::Node if !dotted(&version, 3) => match node_matching(sys, &version) {
            Some(exact) => exact,
            None => {
                println!("There's no {} {version} here.", kind.title());
                return Ok(());
            }
        },
        _ => version,
    };
    remove_locked(sys, kind, &version)
}

/// The installed Node version `version` names: the newest `20.x.y` for
/// `20`, the newest of all for `lts`.
fn node_matching(sys: &System, version: &str) -> Option<String> {
    let mut installed: Vec<String> = versions(sys, Kind::Node)
        .into_iter()
        .map(|(version, _)| version)
        .collect();
    installed.sort_by(|one, other| version_order(one, other));
    match version {
        "lts" => installed.pop(),
        major => installed
            .into_iter()
            .rfind(|found| found.split('.').next() == Some(major)),
    }
}

/// Orders version folders by their numbers, not their letters.
fn version_order(one: &str, other: &str) -> std::cmp::Ordering {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.parse().unwrap_or_default())
            .collect()
    };
    parts(one).cmp(&parts(other))
}

/// `remove`, the lock already held.
fn remove_locked(sys: &System, kind: Kind, version: &str) -> Result<(), String> {
    let mut gone = false;
    match kind {
        Kind::Node => {
            let dir = version_dir(sys, Kind::Node, version);
            if dir.is_dir() {
                remove_tree(&dir)?;
                gone = true;
            }
        }
        Kind::Rust => {
            if let Some(dir) = rust_dir(&sys.path(TOOLCHAINS), version).filter(|d| d.is_dir()) {
                remove_tree(&dir)?;
                gone = true;
            }
        }
    }
    let used = used_file(sys, kind, version);
    if used.exists() {
        let _ = std::fs::remove_file(&used);
    }
    match gone {
        true => println!("{} {version} is gone.", kind.title()),
        false => println!("There's no {} {version} here.", kind.title()),
    }
    Ok(())
}

/// `admin toolchain prune DAYS`: the versions no chat has used for that
/// many days, gone. An install, or an `add` of one that's there, makes its
/// `.used` file new again; a chat running it, its programs' access time
/// (`last_used`).
pub(super) fn prune(sys: &System, days: u64) -> Result<(), String> {
    let limit = Duration::from_secs(days.saturating_mul(86400));
    let _lock = lock(sys)?;
    let mut pruned = 0usize;
    for kind in [Kind::Rust, Kind::Node] {
        for (version, used) in versions(sys, kind) {
            let old = SystemTime::now()
                .duration_since(used)
                .map(|since| since > limit)
                .unwrap_or(false);
            if old {
                remove_locked(sys, kind, &version)?;
                pruned += 1;
            }
        }
    }
    println!("{pruned} toolchain(s) gone: what no chat used for {days} day(s) is removed.");
    Ok(())
}

/// A version's files; under another root too, where the dry run's files
/// are really there.
fn remove_tree(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/*
 * ---------------------------------------------------------------------------
 * The server's shared parts
 * ---------------------------------------------------------------------------
 */

/// `install`, `admin bubblewrap`, `admin toolchains setup`, and each
/// upgrade: the `rovik` group, the shared folders, and the unit that puts
/// each daemon in the group, in that order: systemd won't start a unit
/// naming a group that isn't there. What's in place is left as it is.
pub(super) fn setup(sys: &System) -> Result<(), String> {
    group(sys)?;
    folder(sys, TOOLCHAINS, 0o755, None)?;
    folder(sys, CACHES, 0o750, Some(SHARE_GROUP))?;
    for cache in ["npm", "crates"] {
        folder(sys, &format!("{CACHES}/{cache}"), 0o2770, Some(SHARE_GROUP))?;
    }
    if sys.systemd() {
        // The unit, for whoever installed before it named the group.
        let path = sys.path(UNIT);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(&unit()) {
            sys.write(UNIT, unit().as_bytes(), 0o644)?;
        }
        sys.run("systemctl", &["daemon-reload"])?;
        let run: Vec<String> = running(sys);
        if !run.is_empty() {
            println!(
                "The daemons now run in the {SHARE_GROUP} group when they next start: {}.",
                run.join(", ")
            );
        }
    }
    Ok(())
}

/// The daemons that run now, by instance: their units are in systemd's
/// wants folder, and `is-active` says which of them run.
fn running(sys: &System) -> Vec<String> {
    if !sys.live() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(sys.path(super::WANTS)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("rovik-agentd@") && name.ends_with(".service"))
        .collect()
}

/// What's said when the shared parts couldn't be put in place: an install
/// and an upgrade carry on without them.
pub(super) fn not_set_up(why: &str) -> String {
    format!(
        "The shared toolchains and caches aren't set up: {why}\nThe chats install their own \
         Rust and Node until they are. To try again: sudo {} admin toolchains setup",
        super::INSTALLED
    )
}

/// The `rovik` group, which creates nothing when it's there.
fn group(sys: &System) -> Result<(), String> {
    let exists = sys.live()
        && Command::new("getent")
            .args(["group", SHARE_GROUP])
            .output()
            .is_ok_and(|out| out.status.success());
    if exists {
        return Ok(());
    }
    match sys.which(&["groupadd", "addgroup"]) {
        Some("groupadd") => sys.run("groupadd", &["--system", SHARE_GROUP]).map(|_| ()),
        Some(tool) => sys.run(tool, &["-S", SHARE_GROUP]).map(|_| ()),
        None => Err(format!(
            "There's no groupadd or addgroup to make the {SHARE_GROUP} group."
        )),
    }
}

/// The folder `absolute` with `mode`, owned by root and `group`, made when
/// it's missing; one that's there is left as it is.
fn folder(sys: &System, absolute: &str, mode: u32, group: Option<&str>) -> Result<(), String> {
    let path = sys.path(absolute);
    if path.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // The group first, then the mode: a chown may clear the setgid bit
    // the shared caches need, so new files stay the group's.
    if let Some(group) = group {
        let owner = format!("root:{group}");
        sys.run("chown", &[&owner, absolute])?;
    }
    std::fs::set_permissions(&path, PermissionsExt::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_of_the_shapes_each_kind_has_are_taken() {
        for rust in ["stable", "beta", "nightly", "nightly-2024-09-05", "1.82", "1.82.0"] {
            assert_eq!(parse_version(Kind::Rust, rust), Ok(rust.to_string()), "{rust}");
        }
        for node in ["lts", "20", "20.11.0", "v?"] {
            let parsed = parse_version(Kind::Node, node);
            assert_eq!(parsed.is_ok(), node != "v?", "{node}");
        }
        assert_eq!(parse_version(Kind::Node, " 20 "), Ok("20".into()));
    }

    /// What a chat can send root is only ever one of these.
    #[test]
    fn anything_else_is_refused() {
        for rust in [
            "",
            " ",
            "1.82; rm -rf /",
            "../x",
            "1",
            "1.82.0.1",
            "nightly-24-9-5",
            "nightly-2024-9-5",
            "STABLE",
            "x86_64-unknown-linux-gnu",
            "1.82-unknown-linux-gnu",
        ] {
            assert!(
                parse_version(Kind::Rust, rust).is_err(),
                "rust {rust:?} was taken"
            );
        }
        for node in ["", "20.x", "20.11", "lts-iron", "20.11.0.1", "$(id)"] {
            assert!(
                parse_version(Kind::Node, node).is_err(),
                "node {node:?} was taken"
            );
        }
        assert!(parse_version(Kind::Rust, "1.0.0-beta").is_err());
    }

    #[test]
    fn components_and_targets_are_names_rustup_takes() {
        let (components, targets) =
            flags(&["-c", "rustfmt", "-t", "x86_64-unknown-linux-gnu"]).unwrap();
        assert_eq!(components, ["rustfmt"]);
        assert_eq!(targets, ["x86_64-unknown-linux-gnu"]);
        assert!(flags(&["-c", "Rustfmt"]).is_err());
        assert!(flags(&["-c", "--profile"]).is_err());
        assert!(flags(&["-c"]).is_err());
        assert!(flags(&["--offline"]).is_err());
    }

    /// `prune` takes what no chat used in the days it's given, and keeps
    /// what one did: a version's `.used` file is made new when a project
    /// needs it again.
    #[test]
    fn prune_keeps_what_a_chat_used_recently() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        for version in ["20.11.0", "18.20.0"] {
            std::fs::create_dir_all(version_dir(&sys, Kind::Node, version)).unwrap();
            touch(&sys, Kind::Node, version).unwrap();
        }
        // One used long ago.
        let old = std::time::SystemTime::now() - Duration::from_secs(30 * 86400);
        std::fs::File::options()
            .write(true)
            .open(used_file(&sys, Kind::Node, "18.20.0"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(installed_here(&sys.path(TOOLCHAINS)).1, ["18.20.0", "20.11.0"]);

        prune(&sys, 7).unwrap();
        assert!(version_dir(&sys, Kind::Node, "20.11.0").is_dir());
        assert!(!version_dir(&sys, Kind::Node, "18.20.0").exists());
        assert!(!used_file(&sys, Kind::Node, "18.20.0").exists());
        assert_eq!(installed_here(&sys.path(TOOLCHAINS)).1, ["20.11.0"]);
    }

    /// `node 20` is the newest `20.x.y` there, for `remove` as for `add`.
    #[test]
    fn a_node_major_version_names_the_one_installed() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        for version in ["20.11.0", "20.9.0", "18.20.0"] {
            std::fs::create_dir_all(version_dir(&sys, Kind::Node, version)).unwrap();
            touch(&sys, Kind::Node, version).unwrap();
        }
        assert_eq!(node_matching(&sys, "20"), Some("20.11.0".into()));
        assert_eq!(node_matching(&sys, "18"), Some("18.20.0".into()));
        assert_eq!(node_matching(&sys, "lts"), Some("20.11.0".into()));
        assert_eq!(node_matching(&sys, "22"), None);
    }

    /// The group, then the folders, then the unit: and a second run adds
    /// nothing.
    #[test]
    fn setup_puts_the_shared_parts_in_place_once() {
        let dir = rovik_core::testing::temp_dir();
        std::fs::create_dir_all(dir.0.join("run/systemd/system")).unwrap();
        let sys = System::new(Some(dir.0.clone()));
        setup(&sys).unwrap();
        assert!(sys.path(TOOLCHAINS).is_dir());
        for cache in ["npm", "crates"] {
            let path = sys.path(&format!("{CACHES}/{cache}"));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o2770,
                "{cache}"
            );
        }
        assert_eq!(
            std::fs::metadata(sys.path(TOOLCHAINS))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let unit = std::fs::read_to_string(sys.path(UNIT)).unwrap();
        assert!(unit.contains("SupplementaryGroups=rovik"), "{unit}");
        // A second run changes nothing, and its unit is the same one.
        setup(&sys).unwrap();
        assert_eq!(std::fs::read_to_string(sys.path(UNIT)).unwrap(), unit);
    }
}
