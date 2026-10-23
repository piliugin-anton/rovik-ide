//! The Rust and Node versions a project needs, and the environment that
//! runs them on a cloud host that shares one copy of each
//! (docs/cloud-hosts.md, "Shared toolchains and caches").
//!
//! On the user's own computer nothing here does anything: no daemon has
//! called [`set`], so every function is empty, and a command runs with the
//! user's own toolchains. A server's daemon calls it once at its start
//! with where the versions are (`/opt/rovik/toolchains`), the caches the
//! `rovik` group shares (`/var/cache/rovik`), and how to have a missing
//! version installed as root.
//!
//! - [`wanted`] reads a project's `rust-toolchain.toml`, `rust-toolchain`,
//!   `.nvmrc`, `.node-version` or `engines.node`.
//! - [`env_for`] is the environment every command the agent runs gets
//!   (command_tool.rs): the toolchains' `bin` folders in front of the
//!   PATH and, once the shared Rust is installed, one shared
//!   `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`, since rustup can't write
//!   into the shared folder.
//! - [`ensure`] has the versions a project asks for and the host lacks
//!   installed, through the root command (`rovik-agentd user toolchain …`).
//! - [`seed_crates`] and [`publish_crates`] move crates between the
//!   shared store and the user's own cargo cache, each checked against
//!   the `Cargo.lock` it came from: cargo trusts what's in its cache.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A toolchain: Rust, through one shared rustup, or Node, from the
/// official tarballs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rust,
    Node,
}

impl Kind {
    /// The name Rovik and rustup give it: the `user toolchain` command's
    /// first word, and the folder under the shared root.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rust => "rust",
            Kind::Node => "node",
        }
    }

    /// For a message: `Rust`, `Node`.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Rust => "Rust",
            Kind::Node => "Node",
        }
    }
}

/// A version a project asks for: what the root command installs, and what
/// the environment gives the project's commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub kind: Kind,
    /// As the project names it: `stable`, `1.82`, `lts`, `20`, `20.11.0`.
    pub version: String,
    /// Rust only: rustup's components and targets the project names.
    pub components: Vec<String>,
    pub targets: Vec<String>,
}

impl Want {
    /// For a message: `Rust 1.82`, `Node 20`.
    pub fn describe(&self) -> String {
        format!("{} {}", self.kind.title(), self.version)
    }

    /// What the root command's words are: the kind, the version, then the
    /// components and targets.
    pub fn words(&self) -> Vec<String> {
        let mut words = vec![self.kind.name().to_string(), self.version.clone()];
        for component in &self.components {
            words.push("-c".into());
            words.push(component.clone());
        }
        for target in &self.targets {
            words.push("-t".into());
            words.push(target.clone());
        }
        words
    }
}

/// How a missing version is installed as root: the daemon runs the
/// installed binary's `user toolchain` through sudo, and gets the failure
/// in words.
pub type Installer = dyn Fn(Want) -> Result<(), String> + Send + Sync;

/// What a cloud host shares, which the daemon calls [`set`] with: the
/// versions' root, the caches the `rovik` group writes, and how to install
/// what's missing (None: an administrator installs them,
/// `admin toolchain add`).
#[derive(Clone)]
pub struct Shared {
    /// `/opt/rovik/toolchains`: `rust/` and `node/`, owned by root.
    pub root: PathBuf,
    /// `/var/cache/rovik`, when this daemon may write it: `npm/` and
    /// `crates/`.
    pub caches: Option<PathBuf>,
    pub installer: Option<Arc<Installer>>,
}

/// What the daemon set, if it did: nothing on the user's own computer.
static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

/// What this process shares, for a daemon's start (serve.rs). Once set,
/// every command's environment and every install goes through it.
pub fn set(shared: Shared) {
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(shared));
}

/// Forgets what [`set`] set: a test's, and what a daemon's settings change
/// would run.
#[doc(hidden)]
pub fn unset() {
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Tests that `set` the process's one `Shared` hold this, so they don't
/// see each other's.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn shared() -> Option<Arc<Shared>> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether a cloud host's toolchains are set up here.
pub fn on_host() -> bool {
    shared().is_some()
}

/// Whether the shared Rust is in place: a chat's `cargo` commands run with
/// it even where cargo isn't on this account's PATH.
pub fn shared_rust() -> bool {
    shared().is_some_and(|shared| shared.root.join("rust/cargo/bin/cargo").is_file())
}

/// Whether a missing version can be installed here, by the root command.
pub fn can_install() -> bool {
    shared().is_some_and(|shared| shared.installer.is_some())
}

/*
 * ---------------------------------------------------------------------------
 * What a project asks for
 * ---------------------------------------------------------------------------
 */

/// The versions the project at `root` asks for: Rust when it has a
/// `Cargo.toml`, Node when it has a `package.json`.
pub fn wanted(root: &Path) -> Vec<Want> {
    let mut wanted = Vec::new();
    if root.join("Cargo.toml").is_file() {
        let (version, components, targets) = rust_toolchain(root);
        wanted.push(Want {
            kind: Kind::Rust,
            version,
            components,
            targets,
        });
    }
    if root.join("package.json").is_file() {
        wanted.push(Want {
            kind: Kind::Node,
            version: node_version(root),
            components: Vec::new(),
            targets: Vec::new(),
        });
    }
    wanted
}

/// The Rust version `root` names: `rust-toolchain.toml`'s `channel`, with
/// its `components` and `targets`; else `rust-toolchain`'s; else `stable`.
/// What isn't a version Rovik installs is left to `stable`: a file that
/// names one (a date, a custom toolchain) is the project's own.
fn rust_toolchain(root: &Path) -> (String, Vec<String>, Vec<String>) {
    let toml = std::fs::read_to_string(root.join("rust-toolchain.toml"))
        .or_else(|_| std::fs::read_to_string(root.join("rust-toolchain")));
    let Ok(text) = toml else {
        return ("stable".into(), Vec::new(), Vec::new());
    };
    let channel = setting(&text, "channel").filter(|c| rust_version_ok(c));
    let version = match channel {
        Some(channel) => channel,
        // A plain `rust-toolchain` is the channel alone.
        None => match text.lines().map(str::trim).find(|line| {
            !line.is_empty() && !line.starts_with('#') && !line.contains('=') && !line.starts_with('[')
        }) {
            Some(line) if rust_version_ok(line) => line.to_string(),
            _ => "stable".into(),
        },
    };
    (
        version,
        setting_list(&text, "components"),
        setting_list(&text, "targets"),
    )
}

/// One `key = "value"` of a toolchain file, with the comments taken off.
fn setting(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// One `key = ["a", "b"]` of a toolchain file.
fn setting_list(text: &str, key: &str) -> Vec<String> {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim().trim_start_matches('[').trim_end_matches(']');
        return value
            .split(',')
            .map(|item| item.trim().trim_matches(['"', '\'']).to_string())
            .filter(|item| checked_name(item))
            .collect();
    }
    Vec::new()
}

/// `stable`, `beta`, `nightly`, `nightly-YYYY-MM-DD`, `1.82` or `1.82.0`:
/// what the root command takes (installer/toolchains.rs).
fn rust_version_ok(version: &str) -> bool {
    match version {
        "stable" | "beta" | "nightly" => return true,
        _ => {}
    }
    if let Some(date) = version.strip_prefix("nightly-") {
        let parts: Vec<&str> = date.split('-').collect();
        return parts.len() == 3
            && parts.iter().map(|part| part.len()).eq([4, 2, 2])
            && parts.iter().all(|part| digits(part));
    }
    dotted(version, 2) || dotted(version, 3)
}

/// `lts`, a major version, or `X.Y.Z`.
fn node_version_ok(version: &str) -> bool {
    version == "lts" || digits(version) || dotted(version, 3)
}

/// Whether `text` is all digits, and not empty.
fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Exactly `parts` numbers between dots.
fn dotted(version: &str, parts: usize) -> bool {
    let numbers: Vec<&str> = version.split('.').collect();
    numbers.len() == parts && numbers.iter().all(|number| digits(number))
}

/// Whether `name` is a component or target name rustup takes: lowercase
/// letters, digits, dashes and underscores (`x86_64-unknown-linux-gnu`).
fn checked_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
        })
}

/// The Node version `root` names: `.nvmrc`, `.node-version`, the major
/// version in `engines.node`, else `lts`.
fn node_version(root: &Path) -> String {
    let written = [".nvmrc", ".node-version"].iter().find_map(|name| {
        let text = std::fs::read_to_string(root.join(name)).ok()?;
        let version = text.lines().next()?.trim().trim_start_matches('v');
        (!version.is_empty()).then(|| version.to_string())
    });
    if let Some(version) = written.filter(|v| node_version_ok(v)) {
        return version;
    }
    engines_major(root).unwrap_or_else(|| "lts".into())
}

/// The first version `package.json`'s `engines.node` allows (`>=18 <21`
/// is `18`), where it names one.
fn engines_major(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&text).ok()?;
    let range = package["engines"]["node"].as_str()?;
    let start = range.find(|c: char| c.is_ascii_digit())?;
    let major: String = range[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits(&major).then_some(major)
}

/*
 * ---------------------------------------------------------------------------
 * What the host has
 * ---------------------------------------------------------------------------
 */

/// Whether `want`'s version is in the shared root.
pub fn installed(want: &Want) -> bool {
    shared().is_some_and(|shared| installed_in(&shared.root, want))
}

/// `installed` under `root`.
pub fn installed_in(root: &Path, want: &Want) -> bool {
    match want.kind {
        Kind::Rust => rust_installed(root, &want.version),
        Kind::Node => node_installed(root, &want.version).is_some(),
    }
}

/// Whether rustup has `<version>-<host triple>` under its toolchains:
/// exactly that one, since `nightly` isn't `nightly-2024-09-05`, which a
/// prefix would take for it.
fn rust_installed(root: &Path, version: &str) -> bool {
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => return false,
    };
    root.join(format!("rust/rustup/toolchains/{version}-{triple}"))
        .is_dir()
}

/// The version `want`'s own word names, among those installed: `20` is the
/// newest `20.x.y` there, `lts` the newest of all, `20.11.0` itself.
pub fn node_installed(root: &Path, version: &str) -> Option<String> {
    let dir = root.join("node");
    let mut installed: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("bin/node").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    installed.sort_by(|one, other| version_order(one, other));
    match version {
        "lts" => installed.pop(),
        major if digits(major) => installed
            .into_iter()
            .rfind(|found| found.split('.').next() == Some(major)),
        exact => installed.into_iter().rfind(|found| found == exact),
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

/*
 * ---------------------------------------------------------------------------
 * The environment a project's commands run with
 * ---------------------------------------------------------------------------
 */

/// What a command run in `project` gets: `PATH`, with the shared Rust's
/// `bin` and the project's Node `bin` in front of the one the app's
/// programs have; and, once the shared Rust is installed, `RUSTUP_HOME`,
/// the one shared and read-only, and `RUSTUP_AUTO_INSTALL=0`, so rustup
/// never tries to install into it. `CARGO_HOME` isn't set: cargo keeps `~/.cargo`, each user's own. Empty
/// when nothing is shared.
pub fn env_for(project: &Path) -> Vec<(&'static str, OsString)> {
    let Some(shared) = shared() else {
        return Vec::new();
    };
    if !shared.root.is_dir() {
        return Vec::new();
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let cargo_bin = shared.root.join("rust/cargo/bin");
    // The shared rustup's settings only with its programs: a host with no
    // shared Rust yet (the root folder is made empty at install) leaves the
    // user's own rustup, if any, as it is.
    let shared_rust = cargo_bin.is_dir();
    if shared_rust {
        dirs.push(cargo_bin);
    }
    let node = wanted(project)
        .iter()
        .find(|want| want.kind == Kind::Node)
        .and_then(|want| node_installed(&shared.root, &want.version))
        .map(|version| shared.root.join("node").join(version).join("bin"))
        .filter(|bin| bin.is_dir());
    dirs.extend(node);
    let path = crate::process::child_var("PATH").unwrap_or_default();
    dirs.extend(std::env::split_paths(&path));
    let mut vars: Vec<(&'static str, OsString)> = vec![("PATH", std::env::join_paths(dirs).unwrap_or(path))];
    if shared_rust {
        vars.push(("RUSTUP_HOME", shared.root.join("rust/rustup").into_os_string()));
        vars.push(("RUSTUP_AUTO_INSTALL", OsString::from("0")));
    }
    vars
}

/// Whether `program` is one of the toolchains' own: a Rust or Node program
/// whose missing version a host installs, rather than one its setup script
/// does.
pub fn toolchain_program(program: &str) -> bool {
    matches!(
        program,
        "cargo" | "rustc" | "rustup" | "node" | "npm" | "npx"
    )
}

/// Whether `program` is in the environment [`env_for`] gives a command run
/// in `project`: the shared versions' folders first, then the PATH's own.
pub fn on_path(project: &Path, program: &str) -> bool {
    let path = env_for(project)
        .into_iter()
        .find(|(key, _)| *key == "PATH")
        .map(|(_, value)| value);
    match path {
        Some(path) => std::env::split_paths(&path).any(|dir| dir.join(program).is_file()),
        None => false,
    }
}

/*
 * ---------------------------------------------------------------------------
 * Installing what a project asks for
 * ---------------------------------------------------------------------------
 */

/// The versions `root` asks for and the host doesn't have, installed as
/// root, one after another: each with its outcome. Nothing when nothing is
/// shared, when they're all there, or when no administrator lets a chat
/// install them.
pub async fn ensure(root: &Path) -> Vec<(Want, Result<(), String>)> {
    let Some(shared) = shared() else {
        return Vec::new();
    };
    let Some(installer) = shared.installer.clone() else {
        return Vec::new();
    };
    let mut done = Vec::new();
    for want in wanted(root) {
        if installed_in(&shared.root, &want) {
            continue;
        }
        let asked = want.clone();
        let installer = installer.clone();
        let outcome = crate::rt::spawn_blocking(move || installer(asked))
            .await
            .map_err(|e| e.to_string())
            .and_then(|outcome| outcome);
        done.push((want, outcome));
    }
    done
}

/*
 * ---------------------------------------------------------------------------
 * The caches the daemons share
 * ---------------------------------------------------------------------------
 */

/// The cache folders this daemon shares: npm's and the crates', when its
/// host has them and it may write them. Empty on a computer, and for a
/// daemon whose settings turn them off.
pub fn writable() -> Vec<PathBuf> {
    let Some(caches) = shared().and_then(|shared| shared.caches.clone()) else {
        return Vec::new();
    };
    vec![caches.join("npm"), caches.join("crates")]
}

/// The npm cache the daemons share, when this one has it: `npm ci` with a
/// lockfile writes the packages it downloads there.
pub fn npm_cache() -> Option<PathBuf> {
    writable().into_iter().next()
}

/// The store of crates the daemons share, when this one has it: each one
/// checked against a `Cargo.lock` before it's used.
pub fn crate_cache() -> Option<PathBuf> {
    writable().into_iter().nth(1)
}

/// Whether `dir` is one of the caches [`writable`] names.
pub fn is_cache(dir: &Path) -> bool {
    writable().iter().any(|cache| cache == dir)
}

/// The crates `root`'s `Cargo.lock` names, from crates.io, with the
/// checksums it pins: `(name, version, checksum)`.
fn locked_crates(root: &Path) -> Vec<(String, String, String)> {
    let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return Vec::new();
    };
    let mut crates = Vec::new();
    let (mut name, mut version, mut source, mut checksum) = (None, None, None, None);
    let mut flush = |name: &mut Option<String>,
                     version: &mut Option<String>,
                     source: &mut Option<String>,
                     checksum: &mut Option<String>| {
        if let (Some(name), Some(version), Some(checksum)) = (name.take(), version.take(), checksum.take()) {
            if source.take().is_some_and(|s| s.contains("crates.io")) {
                crates.push((name, version, checksum));
            }
        }
        source.take();
    };
    for line in lock.lines() {
        let line = line.trim();
        if line.starts_with("[[package]]") {
            flush(&mut name, &mut version, &mut source, &mut checksum);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "name" => name = Some(value),
            "version" => version = Some(value),
            "source" => source = Some(value),
            "checksum" => checksum = Some(value),
            _ => {}
        }
    }
    flush(&mut name, &mut version, &mut source, &mut checksum);
    crates
}

/// Cargo's cache folders for the crates.io index: `CARGO_HOME`'s
/// (`~/.cargo` unless it's set), the ones there, else the name for the
/// cargo installed here.
fn index_dirs() -> Vec<PathBuf> {
    let Some(cargo_home) = cargo_home() else {
        return Vec::new();
    };
    let cache = cargo_home.join("registry/cache");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("index.crates.io-")
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    match found.is_empty() {
        // Cargo names the folder after its own way of hashing the index's
        // URL, which changed in 1.85.
        true => vec![cache.join(index_name())],
        false => found,
    }
}

/// Where cargo keeps what it downloads: `CARGO_HOME`, the user's `~/.cargo`
/// unless the daemon or the chat set it.
fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".cargo")))
}

/// The cache folder's name for the cargo installed here: the newer one
/// where its version can't be read.
fn index_name() -> &'static str {
    const NEW: &str = "index.crates.io-1949cf8c6b5b557f";
    const OLD: &str = "index.crates.io-6f17d22bba15001f";
    let out = crate::process::command("cargo")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success());
    let version = out
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let number = version
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let minor = number
        .split('.')
        .nth(1)
        .and_then(|minor| minor.parse::<u64>().ok());
    match minor {
        Some(minor) if number.starts_with("1.") && minor < 85 => OLD,
        _ => NEW,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// The crate files' hash, in hex.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The shared store's file for a crate.
fn crate_file(caches: &Path, name: &str, version: &str) -> PathBuf {
    caches.join("crates").join(format!("{name}-{version}.crate"))
}

/// The crates `root`'s `Cargo.lock` names, from the shared store into this
/// user's cargo cache: each copied only when its SHA-256 is the checksum
/// the lockfile pins, since cargo is given its cache as it is. The number
/// copied. What another user left there that doesn't match is skipped.
pub fn seed_crates(root: &Path) -> usize {
    let Some(caches) = shared().and_then(|shared| shared.caches.clone()) else {
        return 0;
    };
    let dirs = index_dirs();
    if dirs.is_empty() {
        return 0;
    }
    let mut copied = 0;
    for (name, version, checksum) in locked_crates(root) {
        let from = crate_file(&caches, &name, &version);
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        if sha256_hex(&bytes) != checksum.to_ascii_lowercase() {
            ::log::warn!(
                "toolchains: {} isn't what {} pins: it isn't used",
                from.display(),
                checksum
            );
            continue;
        }
        for dir in &dirs {
            let to = dir.join(format!("{name}-{version}.crate"));
            if to.exists() {
                continue;
            }
            if let Err(why) = write_atomically(&to, &bytes, 0o644) {
                ::log::warn!("toolchains: {why}");
                continue;
            }
            copied += 1;
        }
    }
    copied
}

/// This user's checked crates into the shared store, for the others on the
/// host: each written once, through a temporary file and a rename, so a
/// half-written crate is never read.
pub fn publish_crates(root: &Path) {
    let Some(caches) = shared().and_then(|shared| shared.caches.clone()) else {
        return;
    };
    let dirs = index_dirs();
    for (name, version, checksum) in locked_crates(root) {
        let to = crate_file(&caches, &name, &version);
        if to.exists() {
            continue;
        }
        let from = dirs
            .iter()
            .map(|dir| dir.join(format!("{name}-{version}.crate")))
            .find(|path| path.is_file());
        let Some(from) = from else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        if sha256_hex(&bytes) != checksum.to_ascii_lowercase() {
            continue;
        }
        if let Err(why) = write_atomically(&to, &bytes, 0o660) {
            ::log::warn!("toolchains: {why}");
        }
    }
}

/// `bytes` in `to`, which is the file only once it's whole: a temporary
/// file beside it, then a rename.
///
/// The shared store is a folder every `rovik` group member writes, without
/// the sticky bit, so the temporary file is made new (never opened if
/// something is there, a planted symlink included) under a random name,
/// and its mode is set on the open file, never through its path.
fn write_atomically(to: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    let Some(dir) = to.parent() else {
        return Err(format!("{} has no folder", to.display()));
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let (temp, mut file) = (|| {
        let mut tries = 0;
        loop {
            let temp = dir.join(format!(".{name}.{:016x}", rand::random::<u64>()));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match options.open(&temp) {
                Ok(file) => return Ok((temp, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
                Err(e) => return Err(format!("{}: {e}", temp.display())),
            }
        }
    })()?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        let _ = mode;
        drop(file);
        std::fs::rename(&temp, to)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {e}", to.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared root in a temporary folder, with what the daemon sets.
    fn host(dir: &Path) -> (PathBuf, PathBuf) {
        let root = dir.join("toolchains");
        let caches = dir.join("caches");
        for folder in [
            root.join("rust/cargo/bin"),
            root.join("rust/rustup/toolchains"),
            root.join("node"),
            caches.join("npm"),
            caches.join("crates"),
        ] {
            std::fs::create_dir_all(folder).unwrap();
        }
        (
            root,
            caches,
        )
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn a_project_names_its_versions() {
        let dir = crate::testing::temp_dir();
        let root = &dir.0;
        assert!(wanted(root).is_empty());

        write(root, "Cargo.toml", "[package]\nname = \"t\"\n");
        write(root, "package.json", "{}");
        let plain = wanted(root);
        assert_eq!(plain[0].version, "stable");
        assert_eq!(plain[1].version, "lts");

        write(
            root,
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"1.82\"\ncomponents = [\"rustfmt\", \"clippy\"]\n\
             targets = [\"x86_64-unknown-linux-gnu\"]\n",
        );
        write(root, ".nvmrc", "v20.11.0\n");
        let named = wanted(root);
        assert_eq!(named[0].version, "1.82");
        assert_eq!(named[0].components, ["rustfmt", "clippy"]);
        assert_eq!(named[0].targets, ["x86_64-unknown-linux-gnu"]);
        assert_eq!(named[1].version, "20.11.0");

        // A plain `rust-toolchain` is the channel alone; `.node-version`
        // stands in for `.nvmrc`; `engines.node` names a major version.
        std::fs::remove_file(root.join("rust-toolchain.toml")).unwrap();
        write(root, "rust-toolchain", "nightly-2024-09-05\n");
        std::fs::remove_file(root.join(".nvmrc")).unwrap();
        write(root, ".node-version", "22\n");
        assert_eq!(wanted(root)[0].version, "nightly-2024-09-05");
        assert_eq!(wanted(root)[1].version, "22");
        std::fs::remove_file(root.join(".node-version")).unwrap();
        write(
            root,
            "package.json",
            r#"{ "engines": { "node": ">=18 <21" } }"#,
        );
        assert_eq!(wanted(root)[1].version, "18");
    }

    /// A version Rovik doesn't install is the project's own: `stable`.
    #[test]
    fn a_toolchain_file_naming_another_version_is_left_alone() {
        let dir = crate::testing::temp_dir();
        write(&dir.0, "Cargo.toml", "[package]\n");
        write(
            &dir.0,
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"my-custom-toolchain\"\n",
        );
        assert_eq!(wanted(&dir.0)[0].version, "stable");
    }

    // The PATH's `:`: cloud hosts are Linux.
    #[cfg(unix)]
    #[test]
    fn the_environment_puts_the_shared_ones_in_front_of_the_path() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        let (root, _) = host(&dir.0);
        std::fs::create_dir_all(root.join("node/20.11.0/bin")).unwrap();
        std::fs::write(root.join("node/20.11.0/bin/node"), "").unwrap();
        write(&dir.0, "package.json", "{}");
        write(&dir.0, ".nvmrc", "20.11.0");
        set(Shared {
            root: root.clone(),
            caches: None,
            installer: None,
        });
        let env: std::collections::HashMap<_, _> = env_for(&dir.0)
            .into_iter()
            .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
            .collect();
        let path = &env["PATH"];
        let cargo = root.join("rust/cargo/bin");
        let node = root.join("node/20.11.0/bin");
        assert!(
            path.starts_with(&format!("{}:{}:", cargo.display(), node.display())),
            "{path}"
        );
        assert_eq!(
            env["RUSTUP_HOME"],
            root.join("rust/rustup").to_string_lossy()
        );
        assert_eq!(env["RUSTUP_AUTO_INSTALL"], "0");
        assert!(!env.contains_key("CARGO_HOME"));
    }

    /// A host whose shared folder has no Rust yet (made empty at install)
    /// leaves the user's own rustup as it is: no `RUSTUP_HOME` pointing
    /// at a folder that isn't there.
    #[cfg(unix)]
    #[test]
    fn without_the_shared_rust_rustups_settings_are_the_users() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        let root = dir.0.join("toolchains");
        std::fs::create_dir_all(&root).unwrap();
        set(Shared {
            root: root.clone(),
            caches: None,
            installer: None,
        });
        let env: std::collections::HashMap<_, _> = env_for(&dir.0).into_iter().collect();
        assert!(!env.contains_key("RUSTUP_HOME"), "{env:?}");
        assert!(!env.contains_key("RUSTUP_AUTO_INSTALL"), "{env:?}");
        assert!(env.contains_key("PATH"));
    }

    /// The shared store is written by every group member: a symlink one of
    /// them left where a crate goes, or beside it, is never written
    /// through, and nothing temporary is left behind.
    #[cfg(unix)]
    #[test]
    fn a_crate_is_never_written_through_a_planted_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::testing::temp_dir();
        let store = dir.0.join("crates");
        std::fs::create_dir_all(&store).unwrap();
        let victim = dir.0.join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
        let to = store.join("serde-1.0.0.crate");
        std::os::unix::fs::symlink(&victim, &to).unwrap();
        // The old, predictable temporary name, too.
        let old_temp = store.join(format!(".serde-1.0.0.crate.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &old_temp).unwrap();

        write_atomically(&to, b"a crate", 0o660).unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the victim's mode is its own");
        let meta = std::fs::symlink_metadata(&to).unwrap();
        assert!(meta.file_type().is_file(), "the crate is a file, not the link");
        assert_eq!(meta.permissions().mode() & 0o777, 0o660);
        assert_eq!(std::fs::read(&to).unwrap(), b"a crate");
        let planted = old_temp.file_name().unwrap().to_string_lossy().into_owned();
        let left: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.') && *name != planted)
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// Nothing changes on a computer the daemon didn't set anything on.
    #[test]
    fn nothing_is_set_without_a_host() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        write(&dir.0, "Cargo.toml", "[package]\n");
        assert!(env_for(&dir.0).is_empty());
        assert!(!installed(&wanted(&dir.0)[0]));
        assert!(writable().is_empty());
        assert_eq!(seed_crates(&dir.0), 0);
    }

    /// A crate from the store is copied only when it's the one the
    /// lockfile pins.
    #[test]
    fn only_the_pinned_crates_are_copied_into_the_cache() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        let (root, caches) = host(&dir.0);
        // Cargo's cache, where a chat's cargo would keep it.
        let home = dir.0.join("home");
        std::fs::create_dir_all(home.join("registry/cache/index.crates.io-1949cf8c6b5b557f"))
            .unwrap();
        std::env::set_var("CARGO_HOME", &home);
        let good = b"a crate";
        let bad = b"not that crate";
        std::fs::write(caches.join("crates/serde-1.0.0.crate"), good).unwrap();
        std::fs::write(caches.join("crates/rand-0.8.0.crate"), bad).unwrap();
        write(
            &dir.0,
            "Cargo.lock",
            &format!(
                "version = 3\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{}\"\n\n[[package]]\nname = \"rand\"\nversion = \"0.8.0\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{}\"\n",
                sha256_hex(good),
                "0".repeat(64)
            ),
        );
        set(Shared {
            root: root.clone(),
            caches: Some(caches.clone()),
            installer: None,
        });
        assert_eq!(seed_crates(&dir.0), 1);
        let cache = home.join("registry/cache/index.crates.io-1949cf8c6b5b557f");
        assert!(cache.join("serde-1.0.0.crate").is_file());
        assert!(!cache.join("rand-0.8.0.crate").exists(), "not the pinned one");

        // And back: what the user's cache holds goes to the store, group
        // readable and writable.
        std::fs::write(cache.join("rand-0.8.0.crate"), good).unwrap();
        write(
            &dir.0,
            "Cargo.lock",
            &format!(
                "[[package]]\nname = \"rand\"\nversion = \"0.8.0\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{}\"\n",
                sha256_hex(good)
            ),
        );
        std::fs::remove_file(caches.join("crates/rand-0.8.0.crate")).unwrap();
        publish_crates(&dir.0);
        let published = caches.join("crates/rand-0.8.0.crate");
        assert_eq!(std::fs::read(&published).unwrap(), good);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&published).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o660);
        }
        assert_eq!(
            std::fs::metadata(&published).unwrap().len(),
            good.len() as u64
        );
    }

    /// `set` and cargo's folder are the process's, so each test that
    /// sets them puts them back.
    struct Guard(Option<OsString>, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Guard {
        fn new() -> Guard {
            let lock = test_lock();
            let cargo_home = std::env::var_os("CARGO_HOME");
            unset();
            Guard(cargo_home, lock)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            unset();
            match &self.0 {
                Some(home) => std::env::set_var("CARGO_HOME", home),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }
}
