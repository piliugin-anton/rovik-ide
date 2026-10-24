//! Python: packages with a `pyproject.toml`, a `requirements.txt` or uv's
//! lockfile, and their installs into a virtual environment. A cloud host
//! doesn't install Python: its setup script does.

use super::{Ecosystem, Install, Project};
use crate::toolchains::Want;

/// Python's projects ([`Ecosystem`]).
pub struct Python;

/// What makes a folder a Python package.
const MANIFESTS: &[&str] = &["pyproject.toml", "requirements.txt", "uv.lock", "setup.py"];

impl Ecosystem for Python {
    fn name(&self) -> &'static str {
        "Python"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["python", "python3", "pip", "pip3", "uv"]
    }

    fn present(&self, project: &Project) -> bool {
        MANIFESTS
            .iter()
            .any(|manifest| project.with(manifest).next().is_some())
    }

    fn toolchain(&self, _project: &Project) -> Option<Want> {
        None
    }

    /// uv's lockfile says how: `uv sync` where its `.venv` isn't there;
    /// else pip's requirements where no virtual environment is.
    fn installs(&self, project: &Project) -> Vec<Install> {
        project
            .folders
            .iter()
            .filter_map(|folder| {
                let has = |name: &str| project.has(folder, name);
                let command = if has("uv.lock") {
                    (!has(".venv")).then_some("uv sync")
                } else {
                    (has("requirements.txt") && !has(".venv") && !has("venv"))
                        .then_some("pip install -r requirements.txt")
                }?;
                Some(Install {
                    folder: folder.clone(),
                    command: command.into(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at, commands, project};

    #[test]
    fn uv_or_pip_installs_where_no_virtual_environment_is() {
        assert_eq!(
            commands(&project(&["pyproject.toml", "uv.lock"]).0),
            [at("", "uv sync")]
        );
        assert_eq!(
            commands(&project(&["requirements.txt"]).0),
            [at("", "pip install -r requirements.txt")]
        );
        for files in [
            &["pyproject.toml", "uv.lock", ".venv/"][..],
            &["requirements.txt", ".venv/"],
            &["requirements.txt", "venv/"],
            // uv's lockfile says how: not pip's requirements beside it.
            &["uv.lock", "requirements.txt", ".venv/"],
        ] {
            assert_eq!(commands(&project(files).0), [], "{files:?}");
        }
    }
}
