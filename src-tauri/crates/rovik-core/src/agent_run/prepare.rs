//! Prepare (autopilot-plan 4.3, D19): the installs a working copy's
//! lockfiles imply, or a Node package's manifest names without one, where
//! the dependencies they install aren't there, run as heavy work with the
//! checks' timeout.
//!
//! S03: an Autopilot task's Prepare stage runs them before the tests and
//! the code (`prepare_stage`), in a run of its own with no model turn; a
//! record shows each install, its output saved as `@checks/prepare-…`. A
//! failed install never ends the task: the checks say what's missing.
//!
//! V03 (D12) runs them again when a check of the task fails on its
//! environment (a program or a package that isn't installed:
//! `command_output::environment_failure`), once a run: that round isn't
//! the change's failure, so it isn't counted, and the checks run again
//! once the installs are done.
//!
//! A program an install runs that isn't on the PATH (`npm`, `cargo`) may
//! have been installed since the app started, or a server's daemon: the
//! login shell's PATH is read again before the installs (`find_programs`).

use super::check_output;
use super::command_output::{clean_output, focus_failures};
use super::log;
use super::run::Run;
use super::verify::{self, CheckResult, FAILURE_OUTPUT_CHARS};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// An install a package needs: its lockfile's, or npm's of its package.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// Where it runs, from the project's folder: "" for the folder itself.
    pub folder: String,
    pub command: String,
}

use crate::project_profile::NOT_PACKAGES;
use crate::toolchains::Want;

/// Node's lockfiles, any of which says how its packages install.
const NODE_LOCKFILES: &[&str] = &[
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "bun.lock",
];

/// A Node package's install without a lockfile: npm's, at the latest
/// versions its package.json allows, writing no lockfile (it would be a
/// file the change adds).
const UNLOCKED_NODE_INSTALL: &str = "npm install --no-package-lock --no-audit --no-fund";

/// The installs the packages in `root` and one folder down need, where
/// what they install isn't there: `node_modules` (or Yarn's `.pnp.cjs`)
/// for Node, the `.venv` uv makes, a virtual environment for pip's
/// requirements, `target` for Cargo (never built there), `vendor` for Go.
/// A lockfile says how; a Node package without one, whose dependencies
/// npm installs, gets npm's install (a server's copy of a project that
/// commits no lockfile has nothing installed). A Node workspace's packages
/// install with it, from its root, never each on its own. The folder
/// itself first, then the others by name.
pub fn needed(root: &Path) -> Vec<Install> {
    let mut folders = vec![String::new()];
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut below: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|name| !name.starts_with('.') && !NOT_PACKAGES.contains(&name.as_str()))
            .collect();
        below.sort();
        folders.extend(below);
    }
    let (inside, root_is_one) = (below_a_workspace(root), workspace_root(root));
    folders
        .into_iter()
        .flat_map(|folder| {
            let in_workspace = inside || (root_is_one && !folder.is_empty());
            installs_in(root, folder, in_workspace)
        })
        .collect()
}

/// The installs `folder`'s own lockfiles imply, or its package.json names
/// without one, unless it's `in_workspace`: a package of a Node workspace,
/// which its root installs.
fn installs_in(root: &Path, folder: String, in_workspace: bool) -> Vec<Install> {
    let dir = root.join(&folder);
    let has = |name: &str| dir.join(name).exists();
    let mut commands: Vec<&'static str> = Vec::new();
    if has("package.json") && !has("node_modules") && !has(".pnp.cjs") {
        if NODE_LOCKFILES.iter().any(|l| has(l)) {
            commands.push(node_install(&dir));
        } else if !in_workspace && npm_installs(&dir) {
            commands.push(UNLOCKED_NODE_INSTALL);
        }
    }
    if has("Cargo.toml") && has("Cargo.lock") && !has("target") {
        commands.push("cargo fetch");
    }
    if has("uv.lock") {
        if !has(".venv") {
            commands.push("uv sync");
        }
    } else if has("requirements.txt") && !has(".venv") && !has("venv") {
        commands.push("pip install -r requirements.txt");
    }
    if has("go.mod") && has("go.sum") && !has("vendor") {
        commands.push("go mod download");
    }
    commands
        .into_iter()
        .map(|command| Install {
            folder: folder.clone(),
            command: command.to_string(),
        })
        .collect()
}

/// The install of a Node package's lockfile, by its package manager.
fn node_install(dir: &Path) -> &'static str {
    use crate::project_profile::workspaces::{detect_package_manager, is_yarn_berry};
    let pkg = package_json(dir);
    match detect_package_manager(dir) {
        "pnpm" => "pnpm install --frozen-lockfile",
        "yarn" if is_yarn_berry(dir, pkg.as_ref()) => "yarn install --immutable",
        "yarn" => "yarn install --frozen-lockfile",
        "bun" => "bun install --frozen-lockfile",
        // No audit: a request to the registry nobody reads here.
        _ => "npm ci --no-audit --no-fund",
    }
}

/// Whether npm installs what `dir`'s package.json, which has no lockfile
/// beside it, names: dependencies, or the packages of the workspace it's
/// the root of, and no other package manager's (its `packageManager`,
/// Yarn's settings, a pnpm workspace).
fn npm_installs(dir: &Path) -> bool {
    use crate::project_profile::workspaces::{detect_package_manager, is_yarn_berry};
    let Some(pkg) = package_json(dir) else {
        return false;
    };
    let names = ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|key| pkg[*key].as_object().is_some_and(|deps| !deps.is_empty()))
        || !pkg["workspaces"].is_null();
    let npms = pkg["packageManager"]
        .as_str()
        .is_none_or(|manager| manager.starts_with("npm@"));
    names && npms && detect_package_manager(dir) == "npm" && !is_yarn_berry(dir, Some(&pkg))
}

/// `dir`'s package.json, when it reads.
fn package_json(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether `dir` is a Node workspace's root: its package.json names
/// workspaces, or a pnpm-workspace.yaml does.
fn workspace_root(dir: &Path) -> bool {
    dir.join("pnpm-workspace.yaml").exists()
        || package_json(dir).is_some_and(|pkg| !pkg["workspaces"].is_null())
}

/// Whether a folder above `root`, up to its repository's top, is a Node
/// workspace's root: `root`'s packages are that workspace's.
fn below_a_workspace(root: &Path) -> bool {
    let mut dir = root;
    while !dir.join(".git").exists() {
        let Some(parent) = dir.parent() else {
            return false;
        };
        if workspace_root(parent) {
            return true;
        }
        dir = parent;
    }
    false
}

/// The programs `installs` run (`npm` of `npm ci`) that aren't on the PATH
/// the chat's commands get may have been installed since the app started,
/// or since a server's daemon did: a host's setup script installs the
/// toolchains after its daemon started, and the daemon read its login
/// shell's PATH as it started. That PATH is read again then
/// (`process::refresh_login_path`), for these installs and the commands
/// after them: the checks, the agent's. Logs what it found.
///
/// Before that, a cloud host's shared Rust and Node versions the packages
/// ask for and the host lacks are installed, through the root command
/// (toolchains): what the installs below run. Each with its outcome, for
/// the line that says how the installs went.
async fn find_programs(root: &str, installs: &[Install]) -> Vec<(Want, Result<(), String>)> {
    let toolchains = crate::toolchains::ensure(Path::new(root)).await;
    let mut missing: Vec<String> = Vec::new();
    for install in installs {
        let Some(program) = install.command.split_whitespace().next() else {
            continue;
        };
        if !missing.iter().any(|m| m == program) && !crate::process::on_path(program) {
            missing.push(program.to_string());
        }
    }
    if missing.is_empty() {
        return toolchains;
    }
    let read = crate::rt::spawn_blocking(crate::process::refresh_login_path)
        .await
        .unwrap_or(false);
    let found: Vec<&String> = missing
        .iter()
        .filter(|p| crate::process::on_path(p))
        .collect();
    let path = match read {
        true => "read again",
        false => "couldn't be read",
    };
    ::log::info!(
        "prepare: {missing:?} not on the PATH; the login shell's PATH {path}; found now: {found:?}"
    );
    toolchains
}

/// How installs went, in a line: what was installed, or which failed, and
/// the programs they need that weren't found where the chat runs.
/// `on_server`: a daemon's run, where a program that isn't there is the
/// server's to install, which its host's setup script does.
pub(super) fn installs_line(
    done: &[InstallResult],
    on_server: bool,
    toolchains: &[(Want, Result<(), String>)],
) -> String {
    let named = |d: &InstallResult| match d.folder.as_str() {
        "" => format!("`{}`", d.command),
        folder => format!("`{}` in {folder}", d.command),
    };
    let toolchains = toolchains_line(toolchains);
    let failed: Vec<&InstallResult> = done.iter().filter(|d| d.status == "failed").collect();
    if failed.is_empty() {
        return format!(
            "{toolchains}Installed: {}",
            done.iter().map(named).collect::<Vec<_>>().join(", ")
        );
    }
    let mut missing: Vec<&str> = Vec::new();
    for program in failed.iter().filter_map(|d| not_found(d)) {
        if !missing.contains(&program) {
            missing.push(program);
        }
    }
    let (place, it, them) = match on_server {
        true => (
            "on the server",
            " (its setup script, in Settings → Cloud hosts, can install it)",
            " (its setup script, in Settings → Cloud hosts, can install them)",
        ),
        false => ("where the chat runs", "", ""),
    };
    let why = match missing.as_slice() {
        [] => "the checks will say what's missing".to_string(),
        [program] => format!("{program} wasn't found {place}{it}"),
        [first @ .., last] => format!(
            "{} and {last} weren't found {place}{them}",
            first.join(", ")
        ),
    };
    format!(
        "{toolchains}{} of {} installs failed ({}): {why}",
        failed.len(),
        done.len(),
        failed
            .iter()
            .map(|d| named(d))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The toolchains this round installed, or couldn't, as the start of
/// `installs_line`'s sentence: empty when the project asked for none.
/// The toolchain outcomes for a record's metadata: each version, with
/// the root command's error when it wasn't installed.
fn toolchains_meta(toolchains: &[(Want, Result<(), String>)]) -> Vec<Value> {
    toolchains
        .iter()
        .map(|(want, outcome)| json!({ "toolchain": want.describe(), "error": outcome.as_ref().err() }))
        .collect()
}

/// An environment round's outcome: true ("repaired") when an install ran
/// or a toolchain was installed, so the checks run again at once.
fn environment_outcome(done: &[InstallResult], toolchains: &[(Want, Result<(), String>)]) -> bool {
    !done.is_empty() || toolchains.iter().any(|(_, outcome)| outcome.is_ok())
}

/// What the agent is told when an environment round repaired nothing.
/// A version the host couldn't install is the server's to add, never the
/// agent's.
fn environment_note(failure: &str, toolchains: &[(Want, Result<(), String>)]) -> String {
    let refused: Vec<String> = toolchains
        .iter()
        .filter(|(_, outcome)| outcome.is_err())
        .map(|(want, _)| want.describe())
        .collect();
    if refused.is_empty() {
        return format!(
            "This attempt isn't counted: the checks failed on the environment, not on your change (`{failure}`), \
             and the IDE found nothing to install for it. If the project says how to install what's missing (its \
             manifest, lockfile or README), install it; otherwise work around it. Then finish: the checks run again."
        );
    }
    format!(
        "{}This attempt isn't counted: the checks failed on the environment, not on your change (`{failure}`). \
         The version the project needs ({}) couldn't be installed on this server, and an administrator can add it. \
         Don't install a toolchain yourself: tell the user, then finish: the checks run again.",
        toolchains_line(toolchains),
        refused.join(", ")
    )
}

fn toolchains_line(toolchains: &[(Want, Result<(), String>)]) -> String {
    if toolchains.is_empty() {
        return String::new();
    }
    let said: Vec<String> = toolchains
        .iter()
        .map(|(want, outcome)| match outcome {
            Ok(()) => format!("installed {}", want.describe()),
            // The root command's message: one line, in a sentence.
            Err(why) => format!(
                "{} couldn't be installed ({})",
                want.describe(),
                why.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        })
        .collect();
    format!("Toolchains: {}. ", said.join(", "))
}

/// The program a failed install runs that its shell didn't find: `npm`
/// for `npm ci` ending "sh: 1: npm: not found", with exit code 127 (cmd's
/// 9009 on Windows). Not one the install itself runs (a package's script).
fn not_found(d: &InstallResult) -> Option<&str> {
    let program = d.command.split_whitespace().next()?;
    let code @ Some(127 | 9009) = d.exit_code.flatten() else {
        return None;
    };
    let line = super::command_output::environment_failure(code, d.output.as_deref()?)?;
    line.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .any(|word| word == program)
        .then_some(program)
}

/// An install as its record shows it: like a check's result.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub command: String,
    pub folder: String,
    /// pending, running, passed, failed, skipped.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// failed: the end of its output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The whole of a longer output (`@checks/prepare-…`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_output: Option<String>,
}

impl InstallResult {
    fn pending(install: &Install) -> Self {
        InstallResult {
            command: install.command.clone(),
            folder: install.folder.clone(),
            status: "pending",
            exit_code: None,
            timed_out: None,
            duration_ms: None,
            output: None,
            saved_output: None,
        }
    }
}

impl Run {
    /// S03 (4.3): the Prepare stage's run. The installs the packages need
    /// (`needed`) run where what they install isn't there, a record
    /// showing each; the stage's record says how it went, and counts the
    /// installs that failed (`failures`). "stopped" when the user stopped
    /// it, else "done": a failed install doesn't end the task.
    pub(super) async fn prepare_stage(&mut self) -> Result<String, String> {
        use super::autopilot::Status;
        if self.stage.is_none() {
            return Ok("done".into());
        }
        let Some(root) = self.root.clone() else {
            self.end_own_stage(Status::Skipped, "No project folder", None)
                .await?;
            return Ok("done".into());
        };
        let installs = needed(Path::new(&root));
        if installs.is_empty() {
            let why = "Nothing to install: the dependencies are there";
            self.end_own_stage(Status::Skipped, why, None).await?;
            return Ok("done".into());
        }
        // The task's first saved output: an earlier task's goes (P4.7);
        // the task's later runs keep this stage's (`clean_earlier_output`).
        self.clean_earlier_output();
        let mut done: Vec<InstallResult> = installs.iter().map(InstallResult::pending).collect();
        let meta = |status: &str, done: &[InstallResult]| json!({ "prepare": { "status": status, "installs": done } });
        let mut row = super::run::new_message("assistant", "");
        row.meta = Some(meta("running", &done));
        let record_id = row.id.clone();
        self.append(vec![row]).await?;
        let toolchains = find_programs(&root, &installs).await;
        for (i, install) in installs.iter().enumerate() {
            if self.stopped() {
                done[i].status = "skipped";
                continue;
            }
            done[i].status = "running";
            self.update_meta(&record_id, meta("running", &done)).await?;
            done[i] = self.install(&root, install, i, "prepare").await;
            self.update_meta(&record_id, meta("running", &done)).await?;
        }
        let failed: Vec<&InstallResult> = done.iter().filter(|d| d.status == "failed").collect();
        let status = if self.stopped() {
            "stopped"
        } else if failed.is_empty() {
            "passed"
        } else {
            "failed"
        };
        self.update_meta(&record_id, meta(status, &done)).await?;
        log::write(
            &self.core,
            json!({
                "event": "prepare",
                "dialogId": self.dialog_id,
                "reason": "stage",
                "installs": done.iter().map(|d| json!({
                    "command": d.command,
                    "folder": d.folder,
                    "status": d.status,
                    "exitCode": d.exit_code,
                    "ms": d.duration_ms,
                })).collect::<Vec<_>>(),
            }),
        );
        if self.stopped() {
            return Ok("stopped".into());
        }
        // Done all the same (the checks say what's missing), but not a
        // success to show: the failed installs are counted in the record.
        let failures = failed.len() as u32;
        let summary = installs_line(&done, self.core.agents.on_server(), &toolchains);
        self.end_own_stage_with(|r| {
            r.status = Status::Done;
            r.summary = Some(summary);
            r.reference = Some(record_id);
            r.failures = failures;
        })
        .await?;
        Ok("done".into())
    }

    /// D19 in a Thorough task's candidate's copy (T05), before its
    /// implementation starts: the installs its packages need, where what
    /// they install isn't there. A server's worktree has none of the chat's
    /// installs, which git ignores; a copy on this computer sees the
    /// project's through its overlay, and needs none. Run as the Prepare
    /// stage's are, by this run (its Stop stops them), in `chat`'s name:
    /// its environment, its saved output. The line says how they went;
    /// None: nothing to install, or Prepare is off.
    pub(super) async fn prepare_copy(&mut self, chat: &str, root: &str) -> Option<String> {
        if !self.config().autopilot.prepare {
            return None;
        }
        let installs = needed(Path::new(root));
        if installs.is_empty() {
            return None;
        }
        let toolchains = find_programs(root, &installs).await;
        let mut done: Vec<InstallResult> = installs.iter().map(InstallResult::pending).collect();
        for (i, install) in installs.iter().enumerate() {
            if self.stopped() {
                done[i].status = "skipped";
                continue;
            }
            done[i] = self.install_in(chat, root, install, i, "prepare").await;
        }
        log::write(
            &self.core,
            json!({
                "event": "prepare",
                "dialogId": chat,
                "reason": "candidate",
                "installs": done.iter().map(|d| json!({
                    "command": d.command,
                    "folder": d.folder,
                    "status": d.status,
                    "exitCode": d.exit_code,
                    "ms": d.duration_ms,
                })).collect::<Vec<_>>(),
            }),
        );
        Some(installs_line(
            &done,
            self.core.agents.on_server(),
            &toolchains,
        ))
    }

    /// V03 (D12): the round's checks failed on their environment
    /// (`failure`, from `command`'s output). The round isn't counted: the
    /// installs the packages need run (when the task's settings let
    /// Prepare run), and the checks run again at once ("repaired"). With
    /// nothing to install, the agent hears what's missing instead
    /// ("continue"). The record says which, and that it isn't counted.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn environment_round(
        &mut self,
        root: &str,
        record_id: &str,
        round: u32,
        results: &[CheckResult],
        changed: &[String],
        failure: String,
        command: String,
    ) -> Result<String, String> {
        let prepare = self.config().autopilot.prepare;
        let installs = if prepare {
            needed(Path::new(root))
        } else {
            Vec::new()
        };
        let mut done: Vec<InstallResult> = installs.iter().map(InstallResult::pending).collect();
        let meta = |done: &[InstallResult], toolchains: &[(Want, Result<(), String>)]| {
            verify::with(
                verify::record("failed", round, results, changed, None),
                json!({
                    "environment": {
                        "failure": failure,
                        "command": command,
                        "installs": done,
                        "toolchains": toolchains_meta(toolchains),
                    },
                    "counted": false,
                }),
            )
        };
        self.update_meta(record_id, meta(&done, &[])).await?;
        // A version the project needs that the host lacks installs first;
        // the round's record says how the installs went, and this one is
        // counted again if they don't fix it. Prepare off: nothing is
        // installed, the host's toolchains included.
        let toolchains = match prepare {
            true => find_programs(root, &installs).await,
            false => Vec::new(),
        };
        self.update_meta(record_id, meta(&done, &toolchains)).await?;
        for (i, install) in installs.iter().enumerate() {
            if self.stopped() {
                done[i].status = "skipped";
                continue;
            }
            done[i].status = "running";
            self.update_meta(record_id, meta(&done, &toolchains)).await?;
            // Its own names: the Prepare stage's output stays as it was.
            done[i] = self.install(root, install, i, "prepare-again").await;
            self.update_meta(record_id, meta(&done, &toolchains)).await?;
        }
        log::write(
            &self.core,
            json!({
                "event": "prepare",
                "dialogId": self.dialog_id,
                "reason": "environment",
                "failure": failure,
                "installs": done.iter().map(|d| json!({
                    "command": d.command,
                    "folder": d.folder,
                    "status": d.status,
                    "exitCode": d.exit_code,
                    "ms": d.duration_ms,
                })).collect::<Vec<_>>(),
            }),
        );
        // Not counted: the next round is this one again.
        self.verify_rounds = self.verify_rounds.saturating_sub(1);
        if self.stopped() {
            return Ok("verify-stopped".into());
        }
        if environment_outcome(&done, &toolchains) {
            // The same code: the checks run again on it now.
            self.writes_since_verify = true;
            return Ok("repaired".into());
        }
        let note = environment_note(&failure, &toolchains);
        let report = verify::failure_report(results, changed, round, None, &[note]);
        self.add_harness(report, "verification").await?;
        self.last_verify_failed = true;
        self.take_checkpoint();
        Ok("continue".into())
    }

    /// Runs one install as heavy work, as the checks run; its output is
    /// saved as `@checks/<stem>-…`.
    async fn install(
        &self,
        root: &str,
        install: &Install,
        index: usize,
        stem: &str,
    ) -> InstallResult {
        let chat = self.dialog_id.clone();
        self.install_in(&chat, root, install, index, stem).await
    }

    /// `install`, in `chat`'s name: its copy's environment and confinement,
    /// its saved output; this run's Stop stops it. A cloud host's shared
    /// caches are used where they're safe: npm's for `npm ci` with a
    /// lockfile, and the crates' for `cargo fetch`, seeded from the store
    /// before it and added to it after it succeeds (`toolchains`).
    async fn install_in(
        &self,
        chat: &str,
        root: &str,
        install: &Install,
        index: usize,
        stem: &str,
    ) -> InstallResult {
        let mut r = InstallResult::pending(install);
        let dir = Path::new(root).join(&install.folder);
        let stop = self.stop.clone();
        let overlay = crate::overlay::chats::get(chat);
        let mut queued = crate::command_tool::Queued::check(chat, overlay.as_deref());
        let (command, env) = install_command(install, &dir);
        queued.env.extend(env);
        // `cargo fetch`: the crates the last chat on this host left in the
        // shared store, each checked against this project's lockfile.
        let cargo_fetch = install.command.starts_with("cargo fetch");
        let shared_crates = cargo_fetch && crate::toolchains::crate_cache().is_some();
        if shared_crates {
            let seeded = dir.clone();
            let copied = crate::rt::spawn_blocking(move || crate::toolchains::seed_crates(&seeded))
                .await
                .unwrap_or_default();
            if copied > 0 {
                ::log::info!("prepare: {copied} crate(s) from the shared store in {dir:?}");
            }
        }
        let outcome = crate::command_tool::run_command(
            dir.to_string_lossy().into_owned(),
            command,
            Some(verify::check_timeout_ms(&self.dialog_id)),
            move || stop.is_set(),
            Some(queued),
        )
        .await;
        match outcome {
            Ok(out) if out.stopped => r.status = "skipped",
            Ok(out) => {
                r.duration_ms = Some(out.duration_ms);
                r.exit_code = Some(out.exit_code);
                r.timed_out = Some(out.timed_out);
                let combined = clean_output(
                    &[out.stderr.trim(), out.stdout.trim()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                let passed = out.exit_code == Some(0) && !out.timed_out;
                r.status = if passed { "passed" } else { "failed" };
                if passed && shared_crates {
                    // What this chat fetched, for the next user's chat here.
                    let published = dir.clone();
                    let _ = crate::rt::spawn_blocking(move || {
                        crate::toolchains::publish_crates(&published)
                    })
                    .await;
                }
                if !passed {
                    let focused = focus_failures(&combined, FAILURE_OUTPUT_CHARS);
                    r.output = Some(if focused.is_empty() {
                        "(no output)".into()
                    } else {
                        focused
                    });
                }
                // Kept whole, as a check's output is (4.3): `@checks/prepare-1.log`.
                if !combined.is_empty() {
                    let name = match install.folder.as_str() {
                        "" => format!("{}", index + 1),
                        folder => format!("{folder}-{}", index + 1),
                    };
                    r.saved_output = check_output::save(chat, stem, &name, &combined);
                }
            }
            Err(e) => {
                r.status = "failed";
                r.output = Some(format!("Could not run the command: {e}"));
            }
        }
        r
    }
}

/// The command an install runs in `dir`, and the environment it gets.
/// `npm ci` with a lockfile uses the shared npm cache, where this daemon
/// has one and neither the project's nor the user's `.npmrc` carries
/// registry credentials: npm checks every package against the lockfile's
/// `integrity`, so a package another user put there is still the one this
/// project asked for, and a private package never lands in it. The folder
/// keeps `umask 0007`, so what's written is the `rovik` group's to read.
fn install_command(install: &Install, dir: &Path) -> (String, Vec<(String, String)>) {
    let Some(cache) = crate::toolchains::npm_cache() else {
        return (install.command.clone(), Vec::new());
    };
    let locked = install.command.starts_with("npm ci") && dir.join("package-lock.json").is_file();
    if !locked || npm_credentials(dir) {
        return (install.command.clone(), Vec::new());
    }
    (
        format!("umask 0007 && {}", install.command),
        vec![(
            "npm_config_cache".to_string(),
            cache.to_string_lossy().into_owned(),
        )],
    )
}

/// Whether `dir`'s or this user's `.npmrc` names a registry token: then the
/// install keeps its own cache, since what it downloads is private.
fn npm_credentials(dir: &Path) -> bool {
    let user = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".npmrc"));
    [Some(dir.join(".npmrc")), user]
        .into_iter()
        .flatten()
        .any(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| {
                text.lines().any(|line| {
                    let line = line.split(['#', ';']).next().unwrap_or_default().trim();
                    line.contains("_authToken") || line.starts_with("_auth")
                })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[&str]) -> crate::testing::TempDir {
        let dir = crate::testing::temp_dir();
        for f in files {
            let path = dir.0.join(f);
            if f.ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "{}").unwrap();
            }
        }
        dir
    }

    fn commands(root: &Path) -> Vec<(String, String)> {
        needed(root)
            .into_iter()
            .map(|i| (i.folder, i.command))
            .collect()
    }

    fn at(folder: &str, command: &str) -> (String, String) {
        (folder.to_string(), command.to_string())
    }

    /// Writes `content` to `path` in `dir`.
    fn write(dir: &Path, path: &str, content: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A package.json that names a dependency.
    const DEPENDS: &str = r#"{ "name": "t", "devDependencies": { "vitest": "^3.2.0" } }"#;

    /// 4.3: each lockfile's install, where what it installs isn't there.
    #[test]
    fn a_lockfile_without_its_dependencies_needs_its_install() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["package.json", "package-lock.json"],
                "npm ci --no-audit --no-fund",
            ),
            (
                &["package.json", "pnpm-lock.yaml"],
                "pnpm install --frozen-lockfile",
            ),
            (
                &["package.json", "yarn.lock"],
                "yarn install --frozen-lockfile",
            ),
            (
                &["package.json", "yarn.lock", ".yarnrc.yml"],
                "yarn install --immutable",
            ),
            (
                &["package.json", "bun.lockb"],
                "bun install --frozen-lockfile",
            ),
            (&["Cargo.toml", "Cargo.lock"], "cargo fetch"),
            (&["pyproject.toml", "uv.lock"], "uv sync"),
            (&["requirements.txt"], "pip install -r requirements.txt"),
            (&["go.mod", "go.sum"], "go mod download"),
        ];
        for (files, command) in cases {
            let dir = project(files);
            assert_eq!(commands(&dir.0), [at("", command)], "{files:?}");
        }
    }

    /// Installed already, or nothing to install from: nothing.
    #[test]
    fn installed_dependencies_or_no_lockfile_need_nothing() {
        let cases: &[&[&str]] = &[
            &["package.json", "package-lock.json", "node_modules/"],
            &["package.json", "yarn.lock", ".pnp.cjs"],
            &["package.json"],
            &["package-lock.json"],
            &["Cargo.toml", "Cargo.lock", "target/"],
            &["Cargo.toml"],
            &["pyproject.toml", "uv.lock", ".venv/"],
            &["requirements.txt", ".venv/"],
            &["requirements.txt", "venv/"],
            &["go.mod", "go.sum", "vendor/"],
            &["go.mod"],
            &[],
        ];
        for files in cases {
            let dir = project(files);
            assert_eq!(commands(&dir.0), [], "{files:?}");
        }
        // uv's lockfile says how: not pip's requirements beside it.
        let dir = project(&["uv.lock", "requirements.txt", ".venv/"]);
        assert_eq!(commands(&dir.0), []);
    }

    /// One folder down too (a monorepo's `api/` and `web/`), the folder
    /// itself first; never in what's installed, built or hidden.
    #[test]
    fn a_package_one_folder_down_is_prepared_too() {
        let dir = project(&[
            "package.json",
            "package-lock.json",
            "web/package.json",
            "web/pnpm-lock.yaml",
            "api/Cargo.toml",
            "api/Cargo.lock",
            "api/deep/requirements.txt",
            ".cache/package.json",
            ".cache/package-lock.json",
            "dist/requirements.txt",
        ]);
        assert_eq!(
            commands(&dir.0),
            [
                at("", "npm ci --no-audit --no-fund"),
                at("api", "cargo fetch"),
                at("web", "pnpm install --frozen-lockfile"),
            ]
        );
        // An installed package's own lockfile isn't the project's.
        let installed = project(&[
            "package.json",
            "package-lock.json",
            "node_modules/x/package.json",
            "node_modules/x/package-lock.json",
        ]);
        assert_eq!(commands(&installed.0), []);
    }

    /// A file where a folder would be, or an unreadable folder: no install,
    /// no panic.
    #[test]
    fn odd_folders_are_left_alone() {
        let dir = project(&["package.json", "package-lock.json", "node_modules"]);
        // `node_modules` is a file here: it's there, so nothing installs.
        assert_eq!(commands(&dir.0), []);
        assert_eq!(commands(&dir.0.join("missing")), []);
    }

    /// A Node package that commits no lockfile, as a server's copy of it
    /// has it, with nothing installed: npm installs what its package.json
    /// names, writing no lockfile.
    #[test]
    fn a_package_without_a_lockfile_installs_what_it_names() {
        let cases = [
            DEPENDS,
            r#"{ "dependencies": { "left-pad": "1.3.0" } }"#,
            r#"{ "optionalDependencies": { "fsevents": "*" } }"#,
            r#"{ "packageManager": "npm@10.8.2", "dependencies": { "a": "1" } }"#,
            // A workspace's root, whose packages npm installs.
            r#"{ "private": true, "workspaces": ["packages/*"] }"#,
        ];
        for manifest in cases {
            let dir = project(&[".git/"]);
            write(&dir.0, "package.json", manifest);
            assert_eq!(
                commands(&dir.0),
                [at("", "npm install --no-package-lock --no-audit --no-fund")],
                "{manifest}"
            );
        }
    }

    /// Nothing for npm to install, or another package manager's package:
    /// nothing.
    #[test]
    fn a_package_without_a_lockfile_npm_wouldnt_install_needs_nothing() {
        let cases: &[(&str, &[&str])] = &[
            (r#"{ "scripts": { "test": "node t.js" } }"#, &[]),
            (r#"{ "dependencies": {} }"#, &[]),
            ("not json", &[]),
            (DEPENDS, &["node_modules/"]),
            (DEPENDS, &[".pnp.cjs"]),
            (
                r#"{ "packageManager": "pnpm@9.1.0", "dependencies": { "a": "1" } }"#,
                &[],
            ),
            (
                r#"{ "packageManager": "yarn@1.22.22", "dependencies": { "a": "1" } }"#,
                &[],
            ),
            (DEPENDS, &[".yarnrc.yml"]),
            (DEPENDS, &["pnpm-workspace.yaml"]),
        ];
        for (manifest, files) in cases {
            let dir = project(files);
            std::fs::create_dir_all(dir.0.join(".git")).unwrap();
            write(&dir.0, "package.json", manifest);
            assert_eq!(commands(&dir.0), [], "{manifest} {files:?}");
        }
    }

    /// A Node workspace's packages install with it, from its root: never
    /// each on its own, one folder down or as the project's own folder.
    #[test]
    fn a_workspaces_packages_install_from_its_root() {
        let unlocked = "npm install --no-package-lock --no-audit --no-fund";
        let dir = project(&[".git/"]);
        write(&dir.0, "package.json", r#"{ "workspaces": ["web"] }"#);
        write(&dir.0, "web/package.json", DEPENDS);
        assert_eq!(commands(&dir.0), [at("", unlocked)]);
        // Its lockfile says how.
        write(&dir.0, "package-lock.json", "{}");
        assert_eq!(commands(&dir.0), [at("", "npm ci --no-audit --no-fund")]);
        // pnpm's, with no lockfile: not npm's to install.
        let pnpm = project(&[".git/", "pnpm-workspace.yaml"]);
        write(&pnpm.0, "package.json", "{}");
        write(&pnpm.0, "web/package.json", DEPENDS);
        assert_eq!(commands(&pnpm.0), []);
        // A package of its own beside a root that isn't a workspace's.
        let apart = project(&[".git/"]);
        write(&apart.0, "package.json", r#"{ "name": "tools" }"#);
        write(&apart.0, "web/package.json", DEPENDS);
        assert_eq!(commands(&apart.0), [at("web", unlocked)]);
        // The project is a package of a workspace above it in its
        // repository, or a folder of its packages.
        let repo = project(&[".git/"]);
        write(&repo.0, "package.json", r#"{ "workspaces": ["apps/*"] }"#);
        write(&repo.0, "apps/web/package.json", DEPENDS);
        assert_eq!(commands(&repo.0.join("apps/web")), []);
        assert_eq!(commands(&repo.0.join("apps")), []);
        // A workspace above its repository's top isn't its.
        std::fs::create_dir_all(repo.0.join("apps/web/.git")).unwrap();
        assert_eq!(commands(&repo.0.join("apps/web")), [at("", unlocked)]);
    }

    /// A failed install's line names the program its shell didn't find:
    /// not one a package's script runs, nor a failure of another kind.
    #[test]
    fn an_install_whose_program_isnt_there_says_so() {
        let ran = |command: &str, folder: &str, code: i32, output: &str| InstallResult {
            status: if code == 0 { "passed" } else { "failed" },
            exit_code: Some(Some(code)),
            output: (code != 0).then(|| output.to_string()),
            ..InstallResult::pending(&Install {
                folder: folder.into(),
                command: command.into(),
            })
        };
        let npm = ran(
            "npm ci --no-audit --no-fund",
            "",
            127,
            "sh: 1: npm: not found",
        );
        assert_eq!(
            installs_line(std::slice::from_ref(&npm), false, &[]),
            "1 of 1 installs failed (`npm ci --no-audit --no-fund`): npm wasn't found where the chat runs"
        );
        let cargo = ran("cargo fetch", "api", 127, "bash: cargo: command not found");
        let web = ran(
            "npm install --no-package-lock --no-audit --no-fund",
            "web",
            9009,
            "'npm' is not recognized as an internal or external command,",
        );
        let uv = ran("uv sync", "py", 0, "");
        assert_eq!(
            installs_line(&[npm.clone(), cargo.clone(), web, uv.clone()], false, &[]),
            "3 of 4 installs failed (`npm ci --no-audit --no-fund`, `cargo fetch` in api, \
             `npm install --no-package-lock --no-audit --no-fund` in web): \
             npm and cargo weren't found where the chat runs"
        );
        let script = ran(
            "npm ci --no-audit --no-fund",
            "",
            127,
            "sh: 1: node-gyp: not found",
        );
        let other = ran("npm ci --no-audit --no-fund", "", 1, "npm ERR! code E404");
        for failed in [script, other] {
            assert_eq!(
                installs_line(&[failed], false, &[]),
                "1 of 1 installs failed (`npm ci --no-audit --no-fund`): the checks will say what's missing"
            );
        }
        assert_eq!(installs_line(&[uv], false, &[]), "Installed: `uv sync` in py");
        // On a server, what isn't there is its setup script's to install.
        assert_eq!(
            installs_line(std::slice::from_ref(&npm), true, &[]),
            "1 of 1 installs failed (`npm ci --no-audit --no-fund`): npm wasn't found on the server \
             (its setup script, in Settings → Cloud hosts, can install it)"
        );
        assert_eq!(
            installs_line(&[npm, cargo], true, &[]),
            "2 of 2 installs failed (`npm ci --no-audit --no-fund`, `cargo fetch` in api): npm and \
             cargo weren't found on the server (its setup script, in Settings → Cloud hosts, can \
             install them)"
        );
    }

    /// What a cloud host installed for the project comes first in the line:
    /// the versions it was missing, or the root command's refusal.
    #[test]
    fn the_toolchains_a_project_needed_come_first() {
        use crate::toolchains::{Kind, Want};
        let rust = Want {
            kind: Kind::Rust,
            version: "1.82".into(),
            components: Vec::new(),
            targets: Vec::new(),
        };
        let node = Want {
            kind: Kind::Node,
            version: "20".into(),
            components: Vec::new(),
            targets: Vec::new(),
        };
        let npm = InstallResult::pending(&Install {
            folder: String::new(),
            command: "npm ci --no-audit --no-fund".into(),
        });
        let line = installs_line(
            std::slice::from_ref(&npm),
            false,
            &[
                (rust, Ok(())),
                (
                    node,
                    Err("sudo: a password is required\nIt isn't installed".into()),
                ),
            ],
        );
        assert_eq!(
            line,
            "Toolchains: installed Rust 1.82, Node 20 couldn't be installed (sudo: a password is \
             required; It isn't installed). Installed: `npm ci --no-audit --no-fund`"
        );
        // Nothing asked for: the line is the installs' alone.
        assert_eq!(
            installs_line(&[InstallResult::pending(&Install {
                folder: String::new(),
                command: "uv sync".into(),
            })], false, &[]),
            "Installed: `uv sync`"
        );
    }

    /// An environment round repairs when an install ran or a toolchain was
    /// installed; a refusal alone doesn't, and the agent is told it's the
    /// server's to add.
    #[test]
    fn an_environment_round_counts_a_new_toolchain_and_names_a_refused_one() {
        use crate::toolchains::{Kind, Want};
        let rust = Want {
            kind: Kind::Rust,
            version: "1.82".into(),
            components: Vec::new(),
            targets: Vec::new(),
        };
        let installed = [(rust.clone(), Ok(()))];
        let refused = [(rust, Err("not allowed".to_string()))];
        let npm = InstallResult::pending(&Install {
            folder: String::new(),
            command: "npm ci".into(),
        });
        assert!(environment_outcome(&[], &installed));
        assert!(!environment_outcome(&[], &refused));
        assert!(!environment_outcome(&[], &[]));
        assert!(environment_outcome(std::slice::from_ref(&npm), &refused));

        let note = environment_note("cargo: not found", &refused);
        assert!(note.starts_with("Toolchains: Rust 1.82 couldn't be installed (not allowed). "), "{note}");
        assert!(note.contains("The version the project needs (Rust 1.82)"), "{note}");
        let plain = environment_note("cargo: not found", &[]);
        assert!(plain.contains("the IDE found nothing to install for it"), "{plain}");
        assert_eq!(plain, environment_note("cargo: not found", &installed));

        assert_eq!(
            toolchains_meta(&refused),
            [json!({ "toolchain": "Rust 1.82", "error": "not allowed" })]
        );
        assert_eq!(
            toolchains_meta(&installed),
            [json!({ "toolchain": "Rust 1.82", "error": null })]
        );
    }

    /// `set`, for a test that uses one; other tests that set one wait.
    struct Host(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Host {
        fn new() -> Host {
            let lock = crate::toolchains::test_lock();
            crate::toolchains::unset();
            let dir = crate::testing::temp_dir();
            let caches = dir.0.join("caches");
            for folder in ["npm", "crates"] {
                std::fs::create_dir_all(caches.join(folder)).unwrap();
            }
            crate::toolchains::set(crate::toolchains::Shared {
                root: dir.0.join("toolchains"),
                caches: Some(caches),
                installer: None,
            });
            // The folder outlives the guard, which only forgets the host.
            std::mem::forget(dir);
            Host(lock)
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            crate::toolchains::unset();
        }
    }

    /// `npm ci` with a lockfile writes its packages into the shared cache;
    /// an install whose registry credentials are in an `.npmrc`, and one
    /// that isn't `npm ci`, keep the user's own.
    #[test]
    fn npm_ci_with_a_lockfile_uses_the_shared_cache() {
        let _host = Host::new();
        let dir = crate::testing::temp_dir();
        let install = |command: &str| Install {
            folder: String::new(),
            command: command.into(),
        };
        let ci = install("npm ci --no-audit --no-fund");
        // No lockfile: npm's cache of its own.
        let (command, env) = install_command(&ci, &dir.0);
        assert_eq!(command, ci.command);
        assert!(env.is_empty());

        write(&dir.0, "package-lock.json", "{}");
        let (command, env) = install_command(&ci, &dir.0);
        assert_eq!(command, "umask 0007 && npm ci --no-audit --no-fund");
        let cache = crate::toolchains::npm_cache().unwrap();
        assert_eq!(env, [("npm_config_cache".to_string(), cache.to_string_lossy().into_owned())]);

        // With a registry token in the project's or the user's `.npmrc`:
        // what it downloads may be private, so never the shared cache.
        write(&dir.0, ".npmrc", "//registry.npmjs.org/:_authToken=secret\n");
        let (command, env) = install_command(&ci, &dir.0);
        assert_eq!(command, ci.command);
        assert!(env.is_empty());
        write(&dir.0, ".npmrc", "legacy-peer-deps=true\n");
        assert!(!install_command(&ci, &dir.0).1.is_empty());

        // Not `npm ci`: its own cache.
        let unlocked = install("npm install --no-package-lock --no-audit --no-fund");
        assert_eq!(install_command(&unlocked, &dir.0).0, unlocked.command);
    }
}
