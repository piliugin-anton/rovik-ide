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
//! - [`wanted`] is the versions a project's packages ask for, as the
//!   project's one scan reads them (ecosystems/): its `rust-toolchain`
//!   files, its `.nvmrc`, `.node-version` or `engines.node`, its
//!   workspace's members'.
//! - [`env_for`] is the environment every command the agent runs gets
//!   (command_tool.rs): the toolchains' `bin` folders in front of the
//!   PATH, this user's folder of Yarn and pnpm at its end ([`add_shim`]),
//!   and, once the shared Rust is installed, one shared `RUSTUP_HOME` and
//!   `RUSTUP_AUTO_INSTALL=0`, since rustup can't write into the shared
//!   folder. [`env_script`] is the same for a harness's own shell.
//! - [`ensure`] has the versions asked for that the host lacks installed,
//!   through the root command (`rovik-agentd user toolchain …`).
//! - [`seed_crates`] and [`publish_crates`] move crates between the
//!   shared store and the user's own cargo cache, each checked against
//!   the `Cargo.lock` it came from: cargo trusts what's in its cache.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
/// versions' root, the caches the `rovik` group writes, how to install
/// what's missing (None: an administrator installs them,
/// `admin toolchain add`), and this user's folder of what Rovik adds for
/// a Node without it.
#[derive(Clone)]
pub struct Shared {
    /// `/opt/rovik/toolchains`: `rust/` and `node/`, owned by root.
    pub root: PathBuf,
    /// `/var/cache/rovik`, when this daemon may write it: `npm/` and
    /// `crates/`.
    pub caches: Option<PathBuf>,
    pub installer: Option<Arc<Installer>>,
    /// This user's folder, at the end of every command's PATH, of the
    /// package managers Rovik runs through Corepack ([`add_shim`]): the
    /// daemon's `~/.local/share/rovik/bin`, which the agent's sandboxed
    /// commands read but can't change. None: none are added.
    pub bin: Option<PathBuf>,
}

/// What the daemon set, if it did: nothing on the user's own computer.
static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

/// The installs the root command refused lately, by their words
/// ([`Want::words`]): when, and what it said. [`ensure`] answers with that
/// for [`RETRY_AFTER`] instead of running `sudo` again on every failing
/// command, and still picks up an administrator's fix after it.
static REFUSED: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);

/// How long a refused install is answered from [`REFUSED`].
const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);

fn refused() -> std::sync::MutexGuard<'static, Option<HashMap<String, (Instant, String)>>> {
    REFUSED.lock().unwrap_or_else(|e| e.into_inner())
}

/// What this process shares, for a daemon's start (serve.rs). Once set,
/// every command's environment and every install goes through it.
pub fn set(shared: Shared) {
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(shared));
    *refused() = None;
}

/// Forgets what [`set`] set: a test's, and what a daemon's settings change
/// would run.
#[doc(hidden)]
pub fn unset() {
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *refused() = None;
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

/// Whether a missing version can be installed here, by the root command.
pub fn can_install() -> bool {
    shared().is_some_and(|shared| shared.installer.is_some())
}

/*
 * ---------------------------------------------------------------------------
 * What a project asks for
 * ---------------------------------------------------------------------------
 */

/// The versions the project at `root` asks for: those of the ecosystems
/// it has packages of whose toolchain a host installs, as the project's
/// one scan reads them (ecosystems/).
pub fn wanted(root: &Path) -> Vec<Want> {
    crate::ecosystems::wanted(root)
}

/// `stable`, `beta`, `nightly`, `nightly-YYYY-MM-DD`, `1.82` or `1.82.0`:
/// what the root command takes (installer/toolchains.rs).
pub(crate) fn rust_version_ok(version: &str) -> bool {
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
pub(crate) fn node_version_ok(version: &str) -> bool {
    version == "lts" || digits(version) || dotted(version, 3)
}

/// Whether `text` is all digits, and not empty.
pub(crate) fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Exactly `parts` numbers between dots.
fn dotted(version: &str, parts: usize) -> bool {
    let numbers: Vec<&str> = version.split('.').collect();
    numbers.len() == parts && numbers.iter().all(|number| digits(number))
}

/// Whether `name` is a component or target name rustup takes: lowercase
/// letters, digits, dashes and underscores (`x86_64-unknown-linux-gnu`).
pub(crate) fn checked_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
        })
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
        Kind::Rust => {
            rust_installed(root, &want.version) && {
                let (components, targets) =
                    rust_missing(root, &want.version, &want.components, &want.targets);
                components.is_empty() && targets.is_empty()
            }
        }
        Kind::Node => node_installed(root, &want.version).is_some(),
    }
}

/// The host triple rustup names a toolchain's folder with.
fn host_triple() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64-unknown-linux-gnu"),
        "aarch64" => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// The folder rustup installs `version` in under `root`:
/// `rust/rustup/toolchains/<version>-<host triple>`.
fn rust_dir(root: &Path, version: &str) -> Option<PathBuf> {
    let triple = host_triple()?;
    Some(root.join(format!("rust/rustup/toolchains/{version}-{triple}")))
}

/// Whether rustup has `<version>-<host triple>` under its toolchains:
/// exactly that one, since `nightly` isn't `nightly-2024-09-05`, which a
/// prefix would take for it.
fn rust_installed(root: &Path, version: &str) -> bool {
    rust_dir(root, version).is_some_and(|dir| dir.is_dir())
}

/// Of the components and targets a project names, those the installed
/// Rust `version` under `root` lacks: rustup lists a toolchain's components
/// in `lib/rustlib/components` (`clippy-x86_64-unknown-linux-gnu`, or
/// `clippy-preview-…` in older ones) and keeps each target's library in
/// `lib/rustlib/<target>`. All of them when the version isn't there.
pub fn rust_missing(
    root: &Path,
    version: &str,
    components: &[String],
    targets: &[String],
) -> (Vec<String>, Vec<String>) {
    let Some(dir) = rust_dir(root, version).filter(|dir| dir.is_dir()) else {
        return (components.to_vec(), targets.to_vec());
    };
    let rustlib = dir.join("lib/rustlib");
    let listed = std::fs::read_to_string(rustlib.join("components")).unwrap_or_default();
    let triple = host_triple().unwrap_or_default();
    // `rust-src`, `clippy-<triple>`, `clippy-preview-<triple>`: exactly,
    // so `rust` isn't taken for `rust-src`.
    let has_component = |name: &str| {
        let forms = [
            name.to_string(),
            format!("{name}-preview"),
            format!("{name}-{triple}"),
            format!("{name}-preview-{triple}"),
        ];
        listed.lines().map(str::trim).any(|line| forms.iter().any(|form| form == line))
    };
    let components = components
        .iter()
        .filter(|c| !has_component(c))
        .cloned()
        .collect();
    let targets = targets
        .iter()
        .filter(|t| !rustlib.join(t).is_dir())
        .cloned()
        .collect();
    (components, targets)
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

/// Orders versions by their numbers, not their letters: `20.11.0` after
/// `20` and `9.5.1`.
pub(crate) fn version_order(one: &str, other: &str) -> std::cmp::Ordering {
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

/// What a command run in `project` gets besides the environment it has:
/// the folders in front of its PATH (the shared Rust's `bin`, and the
/// `bin` of the Node version the project runs), those at its end (this
/// user's folder of package managers, [`add_shim`], so one installed by
/// the user comes first), and the variables to set. None when nothing is
/// shared.
struct Additions {
    front: Vec<PathBuf>,
    back: Vec<PathBuf>,
    vars: Vec<(&'static str, OsString)>,
}

fn additions(project: &Path) -> Option<Additions> {
    let shared = shared().filter(|shared| shared.root.is_dir())?;
    let mut front: Vec<PathBuf> = Vec::new();
    let cargo_bin = shared.root.join("rust/cargo/bin");
    // The shared rustup's settings only with its programs: a host with no
    // shared Rust yet (the root folder is made empty at install) leaves the
    // user's own rustup, if any, as it is.
    let shared_rust = cargo_bin.is_dir();
    if shared_rust {
        front.push(cargo_bin);
    }
    // The version the project's packages ask for, else the newest there:
    // a project that has none yet (`npx create-vite`) runs Node too.
    let node = crate::ecosystems::toolchain_for("node", project)
        .and_then(|want| node_installed(&shared.root, &want.version))
        .map(|version| shared.root.join("node").join(version).join("bin"))
        .filter(|bin| bin.is_dir());
    front.extend(node);
    let mut vars: Vec<(&'static str, OsString)> = Vec::new();
    if shared_rust {
        vars.push(("RUSTUP_HOME", shared.root.join("rust/rustup").into_os_string()));
        vars.push(("RUSTUP_AUTO_INSTALL", OsString::from("0")));
    }
    Some(Additions {
        front,
        back: shared.bin.iter().cloned().collect(),
        vars,
    })
}

/// What a command run in `project` gets: `PATH`, with the shared Rust's
/// `bin` and the project's Node `bin` in front of the one the app's
/// programs have and this user's folder of package managers after it;
/// and, once the shared Rust is installed, `RUSTUP_HOME`, the one shared
/// and read-only, and `RUSTUP_AUTO_INSTALL=0`, so rustup never tries to
/// install into it. `CARGO_HOME` isn't set: cargo keeps `~/.cargo`, each
/// user's own. Empty when nothing is shared.
pub fn env_for(project: &Path) -> Vec<(&'static str, OsString)> {
    let Some(additions) = additions(project) else {
        return Vec::new();
    };
    let path = crate::process::child_var("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = additions
        .front
        .into_iter()
        .chain(std::env::split_paths(&path))
        .chain(additions.back)
        .collect();
    let mut vars = vec![("PATH", std::env::join_paths(dirs).unwrap_or(path))];
    vars.extend(additions.vars);
    vars
}

/// [`env_for`] as a POSIX shell script, for a harness that runs its own
/// shell: its lines add the shared folders to the PATH that shell has, and
/// set the variables. A harness that runs it before each command (Claude
/// Code's `CLAUDE_ENV_FILE`) finds what was installed after it started.
/// Empty when nothing is shared.
pub fn env_script(project: &Path) -> String {
    let Some(additions) = additions(project) else {
        return String::new();
    };
    let quoted = |path: &Path| shell_quoted(&path.to_string_lossy());
    let mut parts: Vec<String> = additions.front.iter().map(|dir| quoted(dir)).collect();
    parts.push("\"$PATH\"".into());
    parts.extend(additions.back.iter().map(|dir| quoted(dir)));
    let mut lines = vec![format!("export PATH={}", parts.join(":"))];
    for (key, value) in &additions.vars {
        lines.push(format!(
            "export {key}={}",
            shell_quoted(&value.to_string_lossy())
        ));
    }
    lines.join("\n") + "\n"
}

/// `text` in single quotes, as a POSIX shell reads it back unchanged.
fn shell_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Whether `program` is found by a command run in `project`: on the PATH
/// [`env_for`] gives it on a cloud host (the shared versions' folders, the
/// app's PATH, the package managers' folder), on the app's own elsewhere.
pub fn found(project: &Path, program: &str) -> bool {
    finder(project)(program)
}

/// [`found`] for the programs of one project, its folders read once: what
/// asks about many (the sandbox's tool folders, overlay/confine.rs).
pub fn finder(project: &Path) -> impl Fn(&str) -> bool {
    let shared: Vec<PathBuf> = additions(project)
        .map(|additions| additions.front.into_iter().chain(additions.back).collect())
        .unwrap_or_default();
    move |program| {
        shared.iter().any(|dir| dir.join(program).is_file())
            || (!lacks(program) && crate::process::on_path(program))
    }
}

/// Whether a test has this machine's own PATH lack `program`
/// ([`lacking`]); never outside tests.
#[cfg(not(test))]
fn lacks(_program: &str) -> bool {
    false
}

#[cfg(test)]
thread_local! {
    static LACKING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn lacks(program: &str) -> bool {
    LACKING.with(|lacking| lacking.borrow().iter().any(|p| p == program))
}

/// For a test, on its thread until the guard drops: this machine's own
/// PATH lacks `programs`, as a computer or a server without them would,
/// so [`found`] finds them only where a host puts them.
#[cfg(test)]
pub(crate) fn lacking(programs: &[&str]) -> impl Drop {
    struct Restore(Vec<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LACKING.with(|lacking| *lacking.borrow_mut() = std::mem::take(&mut self.0));
        }
    }
    let before = LACKING.with(|lacking| {
        let mut lacking = lacking.borrow_mut();
        let before = lacking.clone();
        lacking.extend(programs.iter().map(|p| p.to_string()));
        before
    });
    Restore(before)
}

/// The package managers Corepack runs for a Node that has none of them:
/// Yarn's programs and pnpm's.
pub const COREPACK_PROGRAMS: &[&str] = &["yarn", "yarnpkg", "pnpm", "pnpx"];

/// `program`, one of [`COREPACK_PROGRAMS`], in this user's folder of
/// package managers ([`Shared::bin`]): a script that runs it through the
/// newest Corepack (npm's `npx` fetches it once), at the version the
/// project's `packageManager` names, and never pins one into its
/// package.json. A shared Node 25 or later has no Corepack of its own,
/// and an older one's may not know the newest pnpm. True once it's
/// there; false when this host has no such folder or the program isn't
/// Corepack's.
pub fn add_shim(program: &str) -> Result<bool, String> {
    let Some(bin) = shared().and_then(|shared| shared.bin.clone()) else {
        return Ok(false);
    };
    if !COREPACK_PROGRAMS.contains(&program) {
        return Ok(false);
    }
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    // `npx` missing (no Node yet) says so as a shell would: a command
    // that fails on it has Node installed for it (agent_run/environment.rs).
    let script = format!(
        "#!/bin/sh\n\
         # Rovik IDE: {program} through Corepack, at the version the project's\n\
         # packageManager names, for a Node without it.\n\
         export COREPACK_ENABLE_AUTO_PIN=0 COREPACK_ENABLE_DOWNLOAD_PROMPT=0\n\
         command -v npx >/dev/null 2>&1 || {{ echo \"sh: 1: npx: not found\" >&2; exit 127; }}\n\
         exec npx --yes --prefer-offline corepack@latest {program} \"$@\"\n"
    );
    let path = bin.join(program);
    // Written once: a command may be running it.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(script.as_str()) {
        crate::atomic_file::write_atomic_private(&path, script.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(true)
}

/*
 * ---------------------------------------------------------------------------
 * Installing what a project asks for
 * ---------------------------------------------------------------------------
 */

/// Of `wants`, the versions the host doesn't have, installed as root, one
/// after another: each with its outcome. Nothing when nothing is shared,
/// when they're all there, or when no administrator lets a chat install
/// them.
pub async fn ensure(wants: &[Want]) -> Vec<(Want, Result<(), String>)> {
    let Some(shared) = shared() else {
        return Vec::new();
    };
    let Some(installer) = shared.installer.clone() else {
        return Vec::new();
    };
    let mut done = Vec::new();
    for want in wants {
        if installed_in(&shared.root, want) {
            continue;
        }
        let key = want.words().join(" ");
        let remembered = refused()
            .as_ref()
            .and_then(|all| all.get(&key))
            .filter(|(at, _)| at.elapsed() < RETRY_AFTER)
            .map(|(_, why)| why.clone());
        if let Some(why) = remembered {
            done.push((want.clone(), Err(why)));
            continue;
        }
        let asked = want.clone();
        let installer = installer.clone();
        let outcome = crate::rt::spawn_blocking(move || installer(asked))
            .await
            .map_err(|e| e.to_string())
            .and_then(|outcome| outcome);
        let mut memo = refused();
        let all = memo.get_or_insert_with(HashMap::new);
        match &outcome {
            Ok(()) => {
                all.remove(&key);
            }
            Err(why) => {
                all.insert(key, (Instant::now(), why.clone()));
            }
        }
        drop(memo);
        done.push((want.clone(), outcome));
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

/// Whether a `Cargo.lock` entry is safe to make a file name of
/// (`{name}-{version}.crate`): a name in crates.io's characters, a version
/// starting with a digit in semver's (no `/`, no `..`), and a SHA-256.
/// The lockfile is the project's, which the agent writes; the files are
/// copied outside its sandbox.
fn crate_id_ok(name: &str, version: &str, checksum: &str) -> bool {
    let name_ok = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let version_ok = version.starts_with(|c: char| c.is_ascii_digit())
        && !version.contains("..")
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-');
    let checksum_ok = checksum.len() == 64 && checksum.bytes().all(|b| b.is_ascii_hexdigit());
    name_ok && version_ok && checksum_ok
}

/// The crates `root`'s `Cargo.lock` names, from crates.io, with the
/// checksums it pins: `(name, version, checksum)`. An entry that isn't
/// safe to make a file name of ([`crate_id_ok`]) is left out.
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
                if crate_id_ok(&name, &version, &checksum) {
                    crates.push((name, version, checksum));
                } else {
                    ::log::warn!("toolchains: Cargo.lock entry {name:?} {version:?} isn't a crate's name: skipped");
                }
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
        let Some(bytes) = read_store_file(&from) else {
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

/// The largest `.crate` file read from the shared store: crates.io takes
/// none over 10 MiB.
const MAX_CRATE_BYTES: u64 = 64 * 1024 * 1024;

/// A file of the shared store, which any `rovik` group member writes: read
/// only when it's a regular file of a crate's size. Opened without waiting
/// (a FIFO planted there would hang the read) and without following a
/// link, then checked on the open file, so nothing swapped in meanwhile is
/// read either.
fn read_store_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_CRATE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_CRATE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_CRATE_BYTES).then_some(bytes)
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
pub(crate) mod tests {
    use super::*;

    /// A Node version in a test host's shared folder, as the root command
    /// leaves it: its `node`, `npm` and `npx` linked to this machine's own
    /// where it has them, so the commands of tests running beside this
    /// one, which may find this folder on their PATH while the host is
    /// set, still run them; empty files where it hasn't.
    pub(crate) fn put_node(root: &Path, version: &str) {
        let bin = root.join("node").join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for program in ["node", "npm", "npx"] {
            let real = crate::process::child_var("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(program))
                    .find(|file| file.is_file())
            });
            let at = bin.join(program);
            match real {
                #[cfg(unix)]
                Some(real) => std::os::unix::fs::symlink(real, &at).unwrap(),
                _ => std::fs::write(&at, "").unwrap(),
            }
        }
    }

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

    /// A Rust version that's there without a component or target the
    /// project names isn't installed for it: the root command adds them.
    #[test]
    fn a_rust_version_lacking_a_component_or_target_isnt_installed_for_it() {
        let Some(triple) = host_triple() else {
            return;
        };
        let dir = crate::testing::temp_dir();
        let rustlib = dir
            .0
            .join(format!("rust/rustup/toolchains/1.82-{triple}/lib/rustlib"));
        std::fs::create_dir_all(rustlib.join(triple)).unwrap();
        std::fs::write(
            rustlib.join("components"),
            format!("cargo-{triple}\nrustc-{triple}\nrust-std-{triple}\nclippy-preview-{triple}\nrust-src\n"),
        )
        .unwrap();
        let want = |components: &[&str], targets: &[&str]| Want {
            kind: Kind::Rust,
            version: "1.82".into(),
            components: components.iter().map(|c| c.to_string()).collect(),
            targets: targets.iter().map(|t| t.to_string()).collect(),
        };
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(installed_in(&dir.0, &want(&[], &[])));
        assert!(installed_in(&dir.0, &want(&["clippy", "rust-src"], &[triple])));
        // `rust` isn't `rust-src`, `rustfmt` isn't there, nor is wasm.
        let (components, targets) = rust_missing(
            &dir.0,
            "1.82",
            &strings(&["rust", "rustfmt", "clippy"]),
            &strings(&["wasm32-unknown-unknown", triple]),
        );
        assert_eq!(components, ["rust", "rustfmt"]);
        assert_eq!(targets, ["wasm32-unknown-unknown"]);
        assert!(!installed_in(&dir.0, &want(&["rustfmt"], &[])));
        assert!(!installed_in(&dir.0, &want(&[], &["wasm32-unknown-unknown"])));
        // Another version: all of it.
        let (components, _) = rust_missing(&dir.0, "1.83", &strings(&["clippy"]), &[]);
        assert_eq!(components, ["clippy"]);
    }

    /// The versions asked for are those of the ecosystems the project has
    /// packages of, as its one scan reads them (ecosystems/ tests each).
    #[test]
    fn a_project_wants_the_toolchains_of_its_packages() {
        let dir = crate::testing::temp_dir();
        let root = &dir.0;
        assert!(wanted(root).is_empty());
        write(root, "Cargo.toml", "[package]\nname = \"t\"\n");
        write(root, "package.json", "{}");
        write(root, ".nvmrc", "20\n");
        let both: Vec<(Kind, String)> = wanted(root)
            .into_iter()
            .map(|w| (w.kind, w.version))
            .collect();
        assert_eq!(
            both,
            [(Kind::Node, "20".into()), (Kind::Rust, "stable".into())]
        );
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
            bin: None,
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
            bin: None,
        });
        let env: std::collections::HashMap<_, _> = env_for(&dir.0).into_iter().collect();
        assert!(!env.contains_key("RUSTUP_HOME"), "{env:?}");
        assert!(!env.contains_key("RUSTUP_AUTO_INSTALL"), "{env:?}");
        assert!(env.contains_key("PATH"));
    }

    /// A project that names no Node version yet (no package.json: a new
    /// app is about to be made) runs the newest Node there; the user's
    /// folder of package managers ends the PATH, so theirs come first.
    #[cfg(unix)]
    #[test]
    fn a_project_without_a_package_runs_the_newest_node_with_the_shims_last() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        let (root, _) = host(&dir.0);
        for version in ["20.11.0", "22.3.0", "9.5.1"] {
            put_node(&root, version);
        }
        let bin = dir.0.join("home-bin");
        set(Shared {
            root: root.clone(),
            caches: None,
            installer: None,
            bin: Some(bin.clone()),
        });
        let project = dir.0.join("app");
        std::fs::create_dir_all(&project).unwrap();
        let env: std::collections::HashMap<_, _> = env_for(&project)
            .into_iter()
            .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
            .collect();
        let path = &env["PATH"];
        let node = root.join("node/22.3.0/bin");
        assert!(path.contains(&format!(":{}:", node.display())), "{path}");
        assert!(path.ends_with(&format!(":{}", bin.display())), "{path}");
    }

    /// The script a harness's own shell runs: the shared folders around
    /// the PATH that shell has, the variables set, each quoted so a shell
    /// reads them back as they are.
    #[cfg(unix)]
    #[test]
    fn the_env_script_adds_the_shared_folders_to_the_shells_own_path() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        assert_eq!(env_script(&dir.0), "", "nothing shared, nothing to say");
        let (root, _) = host(&dir.0);
        let bin = dir.0.join("it's bin");
        set(Shared {
            root: root.clone(),
            caches: None,
            installer: None,
            bin: Some(bin.clone()),
        });
        let script = env_script(&dir.0);
        let cargo = root.join("rust/cargo/bin");
        assert!(
            script.starts_with(&format!(
                "export PATH='{}':\"$PATH\":'{}'\n",
                cargo.display(),
                bin.display().to_string().replace('\'', r"'\''")
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!(
                "export RUSTUP_HOME='{}'\n",
                root.join("rust/rustup").display()
            )),
            "{script}"
        );
        // A shell reads it back as it is.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "PATH=/usr/bin:/bin; {script} printf '%s' \"$PATH\""
            ))
            .output()
            .unwrap();
        let path = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            path,
            format!("{}:/usr/bin:/bin:{}", cargo.display(), bin.display())
        );
    }

    /// A program is found where a command in the project looks: the shared
    /// folders and the package managers' on a host, the app's PATH anywhere.
    #[cfg(unix)]
    #[test]
    fn a_program_is_found_where_the_projects_commands_look() {
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        assert!(found(&dir.0, "sh"), "the app's own PATH");
        assert!(!found(&dir.0, "rovik-no-such-program"));
        let (root, _) = host(&dir.0);
        std::fs::write(root.join("rust/cargo/bin/cargo"), "").unwrap();
        let bin = dir.0.join("bin");
        set(Shared {
            root: root.clone(),
            caches: None,
            installer: None,
            bin: Some(bin.clone()),
        });
        assert!(found(&dir.0, "cargo"));
        assert!(found(&dir.0, "sh"));
        // One of Corepack's this machine doesn't have: found once shimmed.
        if let Some(program) = COREPACK_PROGRAMS
            .iter()
            .find(|p| !crate::process::on_path(p))
        {
            assert!(!found(&dir.0, program));
            assert_eq!(add_shim(program), Ok(true));
            assert!(found(&dir.0, program));
        }
    }

    /// Yarn and pnpm go in the user's folder as scripts that run them
    /// through Corepack; written once, executable, and only for Corepack's
    /// own programs, where a host has the folder.
    #[cfg(unix)]
    #[test]
    fn a_corepack_shim_runs_the_package_manager_the_project_names() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = Guard::new();
        assert_eq!(add_shim("yarn"), Ok(false), "no host, no folder");
        let dir = crate::testing::temp_dir();
        let (root, _) = host(&dir.0);
        let bin = dir.0.join("bin");
        set(Shared {
            root,
            caches: None,
            installer: None,
            bin: Some(bin.clone()),
        });
        assert_eq!(add_shim("make"), Ok(false));
        assert!(!bin.join("make").exists());
        for program in COREPACK_PROGRAMS {
            assert_eq!(add_shim(program), Ok(true), "{program}");
            let script = std::fs::read_to_string(bin.join(program)).unwrap();
            assert!(script.starts_with("#!/bin/sh\n"), "{script}");
            assert!(
                script.contains(&format!(
                    "exec npx --yes --prefer-offline corepack@latest {program} \"$@\""
                )),
                "{script}"
            );
            assert!(script.contains("COREPACK_ENABLE_AUTO_PIN=0"), "{script}");
            let mode = std::fs::metadata(bin.join(program))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{program}");
        }
        // Without npx (no Node yet), it fails as a shell says so.
        let out = std::process::Command::new(bin.join("pnpm"))
            .arg("--version")
            .env("PATH", dir.0.join("nothing-here"))
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(127));
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "sh: 1: npx: not found\n"
        );
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

    /// A lockfile entry that would make a path out of the store, or one
    /// that isn't a crate's name, version or SHA-256, is never read.
    #[test]
    fn a_lockfile_entry_that_isnt_a_crate_is_skipped() {
        let dir = crate::testing::temp_dir();
        let sum = "a".repeat(64);
        let entry = |name: &str, version: &str, checksum: &str| {
            format!(
                "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                 checksum = \"{checksum}\"\n\n"
            )
        };
        let lock = [
            entry("../../x", "1.0.0", &sum),
            entry("a/b", "1.0.0", &sum),
            entry("x", "1.0/../x", &sum),
            entry("x", "..1", &sum),
            entry("x", "-1", &sum),
            entry("x", "1.0.0", &"a".repeat(63)),
            entry("x", "1.0.0", &"g".repeat(64)),
            entry("serde_json", "1.0.0-rc.1+build.5", &sum),
        ]
        .concat();
        write(&dir.0, "Cargo.lock", &lock);
        assert_eq!(
            locked_crates(&dir.0),
            [("serde_json".to_string(), "1.0.0-rc.1+build.5".to_string(), sum)]
        );
    }

    /// What another group member left in the store that isn't a crate's
    /// file, a FIFO above all, is passed over at once, never waited on.
    #[cfg(unix)]
    #[test]
    fn a_fifo_or_link_in_the_store_is_passed_over() {
        let dir = crate::testing::temp_dir();
        let fifo = dir.0.join("serde-1.0.0.crate");
        let path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a FIFO made in the test's own folder.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o666) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let at = fifo.clone();
        std::thread::spawn(move || tx.send(read_store_file(&at)).unwrap());
        let read = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(read, Ok(None), "a FIFO isn't waited on");

        let real = dir.0.join("real.crate");
        std::fs::write(&real, b"a crate").unwrap();
        assert_eq!(read_store_file(&real).as_deref(), Some(&b"a crate"[..]));
        let link = dir.0.join("rand-0.8.0.crate");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read_store_file(&link), None, "a link isn't followed");
        assert_eq!(read_store_file(&dir.0), None, "nor a folder read");
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
            bin: None,
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

    /// A refused install is answered from memory, without the root
    /// command, until the host's settings change; a success clears it.
    #[test]
    fn a_refused_install_isnt_run_again_every_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _guard = Guard::new();
        let dir = crate::testing::temp_dir();
        let (root, _) = host(&dir.0);
        write(&dir.0, "Cargo.toml", "[package]\n");
        write(&dir.0, "rust-toolchain", "1.82\n");
        let calls = Arc::new(AtomicUsize::new(0));
        let refusing = |calls: Arc<AtomicUsize>| -> Arc<Installer> {
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("not allowed".into())
            })
        };
        let host_with = |installer: Arc<Installer>| Shared {
            root: root.clone(),
            caches: None,
            installer: Some(installer),
            bin: None,
        };
        let ensure_now = || crate::rt::block_on(ensure(&wanted(&dir.0)));
        set(host_with(refusing(calls.clone())));
        let first = ensure_now();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, Err("not allowed".to_string()));
        let second = ensure_now();
        assert_eq!(second[0].1, Err("not allowed".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "answered from memory");

        // The settings again (a daemon's restart): it's tried again.
        unset();
        set(host_with(refusing(calls.clone())));
        ensure_now();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // A new installer (the settings changed) that works is called.
        let worked = Arc::new(AtomicUsize::new(0));
        let counted = worked.clone();
        set(host_with(Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })));
        assert_eq!(ensure_now()[0].1, Ok(()));
        assert_eq!(worked.load(Ordering::SeqCst), 1);
        assert!(refused().as_ref().is_none_or(|all| all.is_empty()));
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
