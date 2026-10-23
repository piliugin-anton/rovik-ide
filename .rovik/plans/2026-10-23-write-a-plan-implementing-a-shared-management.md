# Shared Rust and Node toolchains and package caches on cloud hosts

Each Rust and Node version is installed once per Rovik cloud host, as root, in folders users can read but not change. Any user's chat can ask for a missing version. Package downloads (npm tarballs and crates.io crates) go into shared caches, so the same package is downloaded once for every user on the host. `node_modules` and `target/` stay in each chat's own worktree.

How it works:

- **Installing.** A new root command, `rovik-agentd user toolchain …`, installs a toolchain. The sudo rule already lets every user run `rovik-agentd user *`. The command only accepts strict version formats, downloads only from the official servers, checks what it downloads, and holds a lock while it installs. An administrator can turn this off in `agentd.toml`.
- **Using the toolchains.** A new `rovik_core::toolchains` module works out which Rust and Node a project needs. It gives every command the agent's runs start (the agent's `bash`, the checks, the installs) the right `PATH` and `RUSTUP_HOME`. Missing versions are installed in two places: before the installs that prepare a project, and when a command fails with "command not found".
- **Sharing downloads safely.**
  - npm's cache lives in a folder shared by a new `rovik` group. It is only used by `npm ci` with a lockfile, which checks every package's hash against the lockfile.
  - cargo doesn't check files already in its cache, so Rovik checks them itself. It copies crates from the shared store into the user's own cargo cache only when they match `Cargo.lock`'s checksums. After a fetch, it adds the user's checked crates to the shared store.

## Steps

1. **Settings in agentd.toml**
   - Files: `src-tauri/crates/rovik-agentd/src/limits.rs`, `src-tauri/crates/rovik-agentd/src/installer.rs`
   - Add `pub toolchains: ToolchainSettings` to `AdminFile` with `#[serde(default)]`. It holds `user_install: bool` (default true), `shared_caches: bool` (default true) and `allowed_rust: Vec<String>` (empty means any valid version).
   - `to_toml` writes a `[toolchains]` table. `check` checks each `allowed_rust` entry with the same version check as step 2.
   - These aren't limits. They are changed by `admin toolchains user-install on|off` and `admin toolchains shared-caches on|off` (step 3), which save them with `write_settings`.
   - Add `pub const TOOLCHAINS: &str = "/opt/rovik/toolchains"`, `pub const CACHES: &str = "/var/cache/rovik"` and `pub const SHARE_GROUP: &str = "rovik"` next to `BIN_DIR`.

2. **Installing a toolchain as root**
   - Files: `src-tauri/crates/rovik-agentd/src/installer/toolchains.rs` (new), `src-tauri/crates/rovik-agentd/src/installer.rs`, `src-tauri/crates/rovik-core/src/pinned_browser/archive.rs` (read)
   - New module, following `browser.rs`: functions take `&System` and only print what they would do when `--root` is given.
   - `pub(super) enum Kind { Rust, Node }`.
   - `pub(super) fn parse_version(kind, &str) -> Result<String, String>`:
     - Rust accepts `stable`, `beta`, `nightly` or `nightly-YYYY-MM-DD`, and `1.N` or `1.N.P`.
     - Node accepts `lts`, a major version, or `X.Y.Z`.
     - Anything else is refused, so a user can't pass arbitrary text to root.
   - `pub(super) fn ensure(sys, kind, version, components: &[String], targets: &[String])`:
     - Takes an exclusive `flock` on `TOOLCHAINS/.lock`. It's a new helper; the only existing lock is the daemon's own in `home.rs`.
     - Returns at once when the version is already installed.
     - **Rust:**
       - The first time, downloads `rustup-init` from `https://static.rust-lang.org/rustup/dist/<target>/` and checks it against the published `.sha256`.
       - Runs it with `RUSTUP_HOME=TOOLCHAINS/rust/rustup`, `CARGO_HOME=TOOLCHAINS/rust/cargo`, `--no-modify-path -y --profile minimal --default-toolchain stable`.
       - Then runs `rustup toolchain install <version> --profile minimal`, adding `-c`/`-t` only for components and targets matching `[a-z0-9-]+`.
     - **Node:**
       - Resolves `lts` or a major version to an exact version from `https://nodejs.org/dist/index.json`.
       - Downloads `node-vX.Y.Z-linux-<arch>.tar.xz`, checks it against `SHASUMS256.txt`, and unpacks it into a temporary folder.
       - Renames the folder to `TOOLCHAINS/node/<version>`, as `browser::place` does, so a version's folder only exists once it's complete.
     - Everything installed is set to 0755/0644, owned by root.
     - Writes `TOOLCHAINS/<kind>/<version>.used`, a timestamp that `prune` reads.
   - `pub(super) fn list(sys)`, `remove(sys, kind, version)`, and `prune(sys, days)`. `prune` skips any version whose `.used` file is newer than `days`.
   - `pub(super) fn setup(sys)` puts the shared parts in place, in this order (systemd won't start a unit whose group doesn't exist):
     1. creates the `rovik` group (`groupadd --system`, as `admin_group` does)
     2. creates `CACHES/npm` and `CACHES/crates` as `root:rovik` with mode 2770, and `TOOLCHAINS` with mode 0755
     3. when `sys.systemd()`, rewrites `UNIT` from `unit()` (step 4) and runs `systemctl daemon-reload`.
        It changes nothing that's already in place.

3. **CLI and dispatch**
   - Files: `src-tauri/crates/rovik-agentd/src/cli.rs`, `src-tauri/crates/rovik-agentd/src/installer.rs`
   - `installer::user` stops requiring exactly one argument (`let [action] = args`). It matches `["toolchain", kind, version, rest @ ..]` (where `rest` holds `-c NAME` and `-t TARGET` pairs) **before** the `sys.systemd()` check, so toolchains install on hosts without systemd too. The systemd check and the one-word `let [action]` form stay for `start|stop|restart|status`.
     - The arm reads `AdminFile` and refuses when `user_install` is off. The message names the admin command to use instead: `sudo rovik-agentd admin toolchain add rust 1.82`.
     - It also refuses a Rust version outside `allowed_rust`, when that list isn't empty.
     - Then it calls `toolchains::ensure`.
   - `installer::admin` gains these arms:
     - `["toolchain", "list"]`
     - `["toolchain", "add", kind, version]`, which works even when `user_install` is off
     - `["toolchain", "remove", kind, version]`
     - `["toolchain", "prune", days]`
     - `["toolchains", "user-install" | "shared-caches", "on" | "off"]`
     - `["toolchains", "setup"]`, which calls `toolchains::setup`.
   - The "unknown command" error lists the new arms.
   - `install` calls `toolchains::setup` after `write_sudoers`, before the daemon's `systemctl enable --now`. The shared folders stay unused while `shared_caches` is off.
   - **Upgrades.** Existing servers get the shared parts through the new binary, not through `upgrade()`, because the upgrade runs in the old one. The old `upgrade()` already runs the new build's `INSTALLED admin bubblewrap` before `systemctl try-restart`. So the new binary's `["bubblewrap"]` arm calls `toolchains::setup(sys)` after `bubblewrap::ensure`, and only reports its failure. The daemons then restart already in the group, with the folders in place. `upgrade()` in the new binary gets the same call, for the upgrades after that.
   - `uninstall` already removes `/opt/rovik` whole, which takes `TOOLCHAINS` with it. It only adds `CACHES` to its folders, and runs the existing `sys.which(&["groupdel", "delgroup"])` loop for `SHARE_GROUP` as well as `ADMIN_GROUP`.
   - Update `USAGE` with the `user toolchain …` and `admin toolchain …` / `admin toolchains …` forms.

4. **Daemons join the shared group**
   - Files: `src-tauri/crates/rovik-agentd/src/installer.rs`
   - `unit()` adds `SupplementaryGroups=rovik`, so every user's daemon can write the shared caches.
   - `upgrade()` doesn't rewrite `UNIT`. `toolchains::setup` (step 2) rewrites it after the group exists and runs `daemon-reload`, at install and during upgrades through `admin bubblewrap` (step 3). `write_settings` keeps writing the same unit text.
   - On hosts without systemd, the daemon the app starts isn't in the `rovik` group. The shared caches then aren't writable to it, and step 7 leaves them out.

5. **Core: which toolchain a project uses, and the environment for it**
   - Files: `src-tauri/crates/rovik-core/src/toolchains.rs` (new), `src-tauri/crates/rovik-core/src/lib.rs`
   - `pub struct Shared { pub root: PathBuf, pub caches: Option<PathBuf>, pub installer: Option<Arc<dyn Fn(Want) -> Result<(), String> + Send + Sync>> }`, stored by `pub fn set(shared: Shared)`. When nothing has been set (on the user's own computer), every function does nothing.
   - `pub struct Want { kind: Kind, version: String, components: Vec<String>, targets: Vec<String> }`.
   - `pub fn wanted(root: &Path) -> Vec<Want>`:
     - **Rust**, when there's a `Cargo.toml`: the version comes from `rust-toolchain.toml` (`channel`, `components`, `targets`) or `rust-toolchain`, else `stable`.
     - **Node**, when there's a `package.json`: the version comes from `.nvmrc`, `.node-version`, or the major version in `engines.node`, else `lts`.
     - The checks match `parse_version` in step 2.
   - `pub fn installed(want: &Want) -> bool`: whether the version is in `root`.
     - Rust: `rust/rustup/toolchains/<version>-*`.
     - Node: `node/<version>`. `lts` and major versions resolve to the newest installed version that matches.
   - `pub fn env_for(project: &Path) -> Vec<(&'static str, OsString)>` gives:
     - `PATH`, with `rust/cargo/bin` and the project's Node `bin` folder in front of the PATH from `process::child_var("PATH")`
     - `RUSTUP_HOME`
     - `RUSTUP_AUTO_INSTALL=0`, so rustup never tries to install into the shared, read-only folder
     - no `CARGO_HOME`: cargo keeps its default, `~/.cargo`.
   - `pub async fn ensure(root: &Path) -> Vec<(Want, Result<(), String>)>`: calls `installer` on a blocking thread for each wanted version that isn't installed.
   - `pub fn writable() -> Vec<PathBuf>`: the cache folders, when `caches` is set.

6. **Every command gets the toolchain environment, sandboxed or not**
   - Files: `src-tauri/crates/rovik-core/src/command_tool.rs`, `src-tauri/crates/rovik-core/src/overlay/confine.rs`
   - The environment is added in `command_tool::run_blocking`, not only in `confine::command`'s sandboxed branch. After `shell_command(command, confine.as_ref())`, it sets each `crate::toolchains::env_for(root)` entry on `cmd`, next to the fixed `CI`/`NO_COLOR` variables. That covers both branches of `confine::command`, and the case where `run_blocking` gets no `Confine` at all.
   - `writable_dirs` adds `crate::toolchains::writable()`.
   - `make_missing` creates `~/.cargo` when the shared Rust toolchain exists, not only when `cargo` is already on the PATH.

7. **Daemon: connecting the core to the root command**
   - Files: `src-tauri/crates/rovik-agentd/src/serve.rs`, `src-tauri/crates/rovik-agentd/src/limits.rs` (read)
   - In `run`, next to `set_copies`, read `AdminFile` from `ADMIN_FILE` and call `rovik_core::toolchains::set(Shared { … })`:
     - `root` is `TOOLCHAINS`, when that folder exists.
     - `caches` is set when `shared_caches` is on and the daemon can actually write `CACHES/npm` and `CACHES/crates` (an access check). A daemon outside the `rovik` group, such as one on a host without systemd, gets `None` and keeps its own caches.
     - `installer` is set when `user_install` is on. It runs `sudo -n /opt/rovik/bin/rovik-agentd user toolchain <kind> <version> [-c …] [-t …]` and returns the last lines of its output as the error.
   - Root's process gets nothing from the user except those already-checked words.

8. **Installing toolchains and reusing caches before the project's installs**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/prepare.rs`, `src-tauri/crates/rovik-core/src/toolchains.rs`
   - `find_programs` becomes `find_programs(root, installs)`. It first calls `toolchains::ensure(root)`, so the Prepare stage, `prepare_copy` and `environment_round` all install missing toolchains. `installs_line` names each toolchain installed or refused.
   - **npm cache.** In `install_in`, when `toolchains::writable()` includes the npm cache, the install is `npm ci` with a lockfile, and no `.npmrc` (the project's or `~/.npmrc`) holds `_authToken`/`_auth`:
     - The command runs as `umask 0007 && npm ci …` with `npm_config_cache=<caches>/npm`, using `Queued`'s environment as other installs do.
     - Any other install keeps the user's own npm cache.
   - **Crate cache.** Add to `toolchains.rs`:
     - `pub fn seed_crates(root: &Path) -> usize`: for each `[[package]]` in `Cargo.lock` from crates.io that has a `checksum`, it reads `<caches>/crates/<name>-<version>.crate`. It copies the file into `~/.cargo/registry/cache/<index-dir>/` only when its SHA-256 matches the checksum.
     - `pub fn publish_crates(root: &Path)`: the reverse, writing through a temporary file and a rename with mode 0660.
     - `<index-dir>` is each existing `registry/index/index.crates.io-*` folder. When there's none, it's the name for the cargo version installed (`index.crates.io-1949cf8c6b5b557f` for 1.85+, else `index.crates.io-6f17d22bba15001f`).
   - **Where they're called.** `install_in` checks whether `install.command` starts with `cargo fetch` (the command `installs_in` emits for a `Cargo.lock`). If so, and the crate cache is writable:
     - before `run_command`, it calls `seed_crates(<root>/<install.folder>)` on a blocking thread
     - after a run that exits with code 0, it calls `publish_crates` for the same folder.
       Cargo commands the agent runs itself (`cargo test` fetching on its own) aren't seeded. After a Prepare install, the crates are already in the user's cache.

9. **Install the toolchain when the agent's command isn't found**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/tools.rs`
   - In `missing_program_note`, when the missing program is `cargo`, `rustc`, `rustup`, `node`, `npm` or `npx` and toolchains are set up:
     - Call `toolchains::ensure(root)`.
     - If the program is on the toolchain PATH afterwards, say it was installed and to run the command again.
     - If it isn't, give the root command's error, or the admin command when user installs are off.
   - The setup-script hint stays for any other program.
   - The function gains a `root: &str` parameter; its one caller is the `bash` arm in `run`.

10. **Reporting it in doctor, and docs**
    - Files: `src-tauri/crates/rovik-agentd/src/doctor.rs`, `docs/cloud-hosts.md`, `src/components/CloudHostsSettings.tsx`
    - `gather` adds an `ok`/`warn` line for the shared toolchains (the versions installed, and whether user installs and shared caches are on), and the same in `--json`.
    - Docs:
      - the folders and the `rovik` group under "What installs where" and "What can read what"
      - the new `user toolchain`, `admin toolchain` and `admin toolchains` commands
      - the trust model: caches are shared within the `rovik` group, and packages are checked against lockfiles.
    - The setup script's placeholder says that Rust and Node versions install on their own, and that the script is for anything else.

## Decisions

- **Any user can install on demand (your answer).** This goes through the existing `rovik-agentd user *` sudo rule, so no new sudoers line is needed. An administrator can turn it off with `user_install = false` and add versions with `admin toolchain add`.
- **Shared runtimes and download caches, but `node_modules` stays per worktree (your answer).**
- **Rust uses one shared rustup.** All toolchains live under one read-only `RUSTUP_HOME`; cargo keeps a per-user `CARGO_HOME` (the default). Users share the large part (about 1 GB per toolchain), and each keeps the small writable part to themselves. `RUSTUP_AUTO_INSTALL=0` turns off rustup's own install of a missing toolchain, which would fail on the read-only folder.
- **Node comes from the official tarballs**, checked against SHASUMS256.txt and unpacked one folder per version. That's simpler than shipping nvm or fnm.
- **How the caches stay safe:**
  - The caches can only be read by daemon users, through the `rovik` group with folder mode 2770.
  - npm's shared cache is only used for `npm ci` with a lockfile, because npm checks every tarball against the lockfile's `integrity`.
  - It's not used when the project or user has registry credentials, so private packages never land in the shared cache.
  - cargo doesn't check files already in its cache, so `seed_crates` checks each crate against `Cargo.lock` before copying it. A bad entry from another user is skipped, never used.
- **The rules are enforced where root runs.** Root only gets checked words (the kind, the version, component and target names), never a user's file or command. Downloads go only to rust-lang.org and nodejs.org.
- **Upgrades go through the new binary.** The shared group, folders and unit change go in through `admin bubblewrap`, which the old binary's upgrade already runs, so existing servers get them on the upgrade that ships this.
- **On the user's own computer nothing changes.** All of this only takes effect when the daemon calls `toolchains::set`.

## Validation

- `cd src-tauri && cargo test -p rovik-core -p rovik-agentd` and `cargo clippy --workspace --all-targets`.
- New tests:
  - `installer/toolchains.rs` unit tests: `parse_version` accepts and refuses the right forms (including `1.82; rm -rf /`, `../x`, empty); `prune` keeps recently used versions.
  - `tests/install.rs` (the `--root` dry-run `Server` fixture):
    - `install` creates the group, then the shared folders, then writes the unit with `SupplementaryGroups=rovik`.
    - `admin bubblewrap` runs the same setup, so an upgrade does too.
    - `user toolchain rust 1.82` prints the rustup steps, including on a fixture without systemd.
    - `user toolchain` is refused when `user_install` is off.
    - `admin toolchain add`, `list`, `remove` and `prune` work for administrators only.
    - `uninstall` removes `CACHES` and deletes both groups.
  - `limits.rs` tests: `[toolchains]` is read back as written, and a bad `allowed_rust` entry is refused.
  - `toolchains.rs` tests, in temporary folders:
    - `wanted` reads `rust-toolchain.toml`, `rust-toolchain`, `.nvmrc`, `.node-version` and `engines.node`.
    - `env_for` puts the right folders in front of PATH and sets `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`.
    - `seed_crates` copies a crate whose checksum matches and skips one that doesn't.
    - `publish_crates` writes files with mode 0660.
  - `command_tool.rs`: a command run with `Shared` set sees `RUSTUP_HOME` and the toolchain PATH, both with a `Confine` and without one. Nothing changes when `Shared` isn't set.
  - `serve.rs`, or a `toolchains.rs` unit test of the access check: when the cache folders aren't writable, `caches` is `None`.
  - `prepare.rs`:
    - An `npm ci` install with a lockfile gets `npm_config_cache` and `umask 0007`; one with `.npmrc` credentials doesn't.
    - A `cargo fetch` install calls `seed_crates` before it runs and `publish_crates` only when it succeeds.
  - `tools.rs`: `missing_program_note` for `cargo` calls the installer when `Shared` is set, and keeps the setup-script hint when it isn't.
- By hand, on a real host with two users:
  - User A's chat runs `cargo test` in a fresh project. Rust installs, then the test runs.
  - User B's chat in a project needing the same version starts without downloading the toolchain, and `cargo fetch` reuses A's crates.
  - The same with `npm ci` and a project's `.nvmrc`.
  - `admin toolchains user-install off` makes B's chat name the admin command instead.
  - Upgrading an existing server leaves the daemons running in the `rovik` group.

## Risks

- The `index.crates.io-*` folder names come from cargo's internal hashing. If a future cargo version changes them, `seed_crates` copies nothing (no harm, just no reuse) until the table is updated. It uses the existing folders first.
- npm or pacote could change how they use a cache shared between users (permissions of files written by another group member). Installs fall back to downloading, but this needs testing on a real host.
- Installing a toolchain takes minutes and a Rust toolchain is about 1 GB on disk. A chat waits while the root command runs, and disk use grows until `admin toolchain prune` runs.
- Root downloading at any user's request widens what a user can make root do. The strict version checks, the fixed download servers and `user_install = false` limit it. Administrators who don't want it should turn it off.
- Hosts without internet access to rust-lang.org, nodejs.org or the registries still need an administrator or the setup script.
- Running the shared setup from the `admin bubblewrap` arm piggybacks on an unrelated command, so that the first upgrade can do it. A later cleanup could move it to an explicit upgrade hook.

## Open review findings

The plan reviewer still reported these after its last round. Check them before implementing.

- **[low] Step 3 — a bubblewrap failure could skip the shared setup on upgrade**: `bubblewrap::ensure` returns `Err` when `bwrap` is missing and no package manager is found (`PackageManager::find(sys).ok_or_else(...)`). If the new `["bubblewrap"]` arm is written as `bubblewrap::ensure(sys)?; toolchains::setup(sys)`, `setup` never runs on such hosts. The old `upgrade()` only prints the error and goes on to `try-restart`. The daemons then restart with no `rovik` group, no shared folders and the old unit. The plan says the arm "only reports its failure", but it doesn't say which failure that is. Suggested fix: State that the arm runs `toolchains::setup` whether `bubblewrap::ensure` succeeds or fails, then returns both results together. Add a `tests/install.rs` case where bubblewrap can't be installed but `admin bubblewrap` still creates the group and folders.
