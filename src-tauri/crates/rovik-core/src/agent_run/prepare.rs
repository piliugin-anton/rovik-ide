//! Prepare (autopilot-plan 4.3, D19): the installs a working copy's
//! lockfiles imply, where the dependencies they install aren't there, run
//! as heavy work with the checks' timeout.
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

use super::check_output;
use super::command_output::{clean_output, focus_failures};
use super::log;
use super::run::Run;
use super::verify::{self, CheckResult, FAILURE_OUTPUT_CHARS};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

/// An install a lockfile implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// Where it runs, from the project's folder: "" for the folder itself.
    pub folder: String,
    pub command: String,
}

use crate::project_profile::NOT_PACKAGES;

/// Node's lockfiles, any of which says how its packages install.
const NODE_LOCKFILES: &[&str] = &[
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "bun.lock",
];

/// The installs the lockfiles in `root` and one folder down imply, where
/// what they install isn't there: `node_modules` (or Yarn's `.pnp.cjs`)
/// for Node, the `.venv` uv makes, a virtual environment for pip's
/// requirements, `target` for Cargo (never built there), `vendor` for Go.
/// The folder itself first, then the others by name.
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
    folders
        .into_iter()
        .flat_map(|folder| installs_in(root, folder))
        .collect()
}

/// The installs `folder`'s own lockfiles imply.
fn installs_in(root: &Path, folder: String) -> Vec<Install> {
    let dir = root.join(&folder);
    let has = |name: &str| dir.join(name).exists();
    let mut commands: Vec<&'static str> = Vec::new();
    if has("package.json")
        && NODE_LOCKFILES.iter().any(|l| has(l))
        && !has("node_modules")
        && !has(".pnp.cjs")
    {
        commands.push(node_install(&dir));
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
    let pkg = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    match detect_package_manager(dir) {
        "pnpm" => "pnpm install --frozen-lockfile",
        "yarn" if is_yarn_berry(dir, pkg.as_ref()) => "yarn install --immutable",
        "yarn" => "yarn install --frozen-lockfile",
        "bun" => "bun install --frozen-lockfile",
        // No audit: a request to the registry nobody reads here.
        _ => "npm ci --no-audit --no-fund",
    }
}

/// How installs went, in a line: what was installed, or which failed.
pub(super) fn installs_line(done: &[InstallResult]) -> String {
    let named = |d: &InstallResult| match d.folder.as_str() {
        "" => format!("`{}`", d.command),
        folder => format!("`{}` in {folder}", d.command),
    };
    let failed: Vec<&InstallResult> = done.iter().filter(|d| d.status == "failed").collect();
    if failed.is_empty() {
        return format!(
            "Installed: {}",
            done.iter().map(named).collect::<Vec<_>>().join(", ")
        );
    }
    format!(
        "{} of {} installs failed ({}): the checks will say what's missing",
        failed.len(),
        done.len(),
        failed
            .iter()
            .map(|d| named(d))
            .collect::<Vec<_>>()
            .join(", ")
    )
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
    /// S03 (4.3): the Prepare stage's run. The installs the lockfiles imply
    /// run where what they install isn't there, a record showing each; the
    /// stage's record says how it went. "stopped" when the user stopped it,
    /// else "done": a failed install doesn't end the task.
    pub(super) async fn prepare_stage(&mut self) -> Result<String, String> {
        let Some(stage) = self.stage.clone() else {
            return Ok("done".into());
        };
        let (core, dialog_id) = (self.core.clone(), self.dialog_id.clone());
        let end = |summary: String, status, reference: Option<String>| {
            let (core, dialog_id, record) = (core.clone(), dialog_id.clone(), stage.record.clone());
            async move {
                super::autopilot::end_stage(&core, &dialog_id, &record, |r| {
                    r.status = status;
                    r.summary = Some(summary);
                    r.reference = reference;
                })
                .await
                .map(|_| ())
            }
        };
        use super::autopilot::Status;
        let Some(root) = self.root.clone() else {
            end("No project folder".into(), Status::Skipped, None).await?;
            return Ok("done".into());
        };
        let installs = needed(Path::new(&root));
        if installs.is_empty() {
            end(
                "Nothing to install: the dependencies are there".into(),
                Status::Skipped,
                None,
            )
            .await?;
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
        end(installs_line(&done), Status::Done, Some(record_id)).await?;
        Ok("done".into())
    }

    /// D19 in a Thorough task's candidate's copy (T05), before its
    /// implementation starts: the installs its lockfiles imply, where what
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
        Some(installs_line(&done))
    }

    /// V03 (D12): the round's checks failed on their environment
    /// (`failure`, from `command`'s output). The round isn't counted: the
    /// installs the lockfiles imply run (when the task's settings let
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
        let installs = if self.config().autopilot.prepare {
            needed(Path::new(root))
        } else {
            Vec::new()
        };
        let mut done: Vec<InstallResult> = installs.iter().map(InstallResult::pending).collect();
        let meta = |done: &[InstallResult]| {
            verify::with(
                verify::record("failed", round, results, changed, None),
                json!({
                    "environment": { "failure": failure, "command": command, "installs": done },
                    "counted": false,
                }),
            )
        };
        self.update_meta(record_id, meta(&done)).await?;
        for (i, install) in installs.iter().enumerate() {
            if self.stopped() {
                done[i].status = "skipped";
                continue;
            }
            done[i].status = "running";
            self.update_meta(record_id, meta(&done)).await?;
            // Its own names: the Prepare stage's output stays as it was.
            done[i] = self.install(root, install, i, "prepare-again").await;
            self.update_meta(record_id, meta(&done)).await?;
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
        if !done.is_empty() {
            // The same code: the checks run again on it now.
            self.writes_since_verify = true;
            return Ok("repaired".into());
        }
        let note = format!(
            "This attempt isn't counted: the checks failed on the environment, not on your change (`{failure}`), \
             and the IDE found nothing to install for it. If the project says how to install what's missing (its \
             manifest, lockfile or README), install it; otherwise work around it. Then finish: the checks run again."
        );
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
    /// its saved output; this run's Stop stops it.
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
        let queued = crate::command_tool::Queued {
            kind: crate::workload::JobKind::Check {
                owner: chat.to_string(),
            },
            note: Box::new(|_| {}),
            env: crate::overlay::chats::get(chat)
                .map(|o| o.command_env())
                .unwrap_or_default(),
            confine: crate::overlay::chats::get(chat).and_then(|o| o.confine()),
            whole_output: false,
        };
        let outcome = crate::command_tool::run_command(
            dir.to_string_lossy().into_owned(),
            install.command.clone(),
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
}
