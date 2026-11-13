//! Complete file manifests and admission of a frozen proposed tree.

use crate::{
    identity::{ArtifactRef, Digest},
    ledger::Ledger,
    policy::EffectivePolicy,
    require,
    scope::{self, validate_tree_paths},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

/// A regular source file's exact bytes and Git-compatible mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Only 100644 and 100755 are supported in this release.
    pub mode: u32,
    /// Content reference in controller storage.
    pub content: ArtifactRef,
}

/// Complete immutable candidate identity, including declared build inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Exactly 1.
    pub schema_version: u32,
    /// Authorized immutable Git base or prior snapshot identity.
    pub base_revision: String,
    /// Complete resulting tree, including added and unchanged files.
    pub files: BTreeMap<String, FileEntry>,
    /// Paths deleted relative to the approved base.
    pub deletions: Vec<String>,
    /// Pinned submodule or dependency assets supplied to the build.
    pub dependency_assets: Vec<Digest>,
}
impl Snapshot {
    /// Check portable paths, modes, references and case collisions.
    pub fn validate(&self, ledger: &Ledger, case_insensitive: bool) -> Result<()> {
        require(self.schema_version == 1, "unsupported snapshot version")?;
        crate::id(&self.base_revision)?;
        validate_tree_paths(self.files.keys().map(String::as_str), case_insensitive)?;
        let mut deleted = BTreeSet::new();
        for path in &self.deletions {
            scope::validate_path(path)?;
            require(
                deleted.insert(path) && !self.files.contains_key(path),
                "ambiguous snapshot deletion",
            )?;
        }
        for (path, file) in &self.files {
            require(
                !path.split('/').any(|p| p.eq_ignore_ascii_case(".git")),
                "Git metadata is not a source input",
            )?;
            require(
                [0o100644, 0o100755].contains(&file.mode),
                "unsupported file kind or mode; symlinks and submodules need explicit adapters",
            )?;
            require(
                file.content.kind == "source",
                "snapshot entry is not a registered source artifact",
            )?;
            ledger.verify_reference(&file.content)?;
        }
        for dependency in &self.dependency_assets {
            ledger.read(dependency)?;
        }
        Ok(())
    }
    /// Identity includes every source byte, mode, deletion and dependency input.
    pub fn digest(&self) -> Result<Digest> {
        Digest::of_json(self)
    }
    /// Materialize only into a new controller-owned directory, with no concurrent worker.
    pub fn materialize(&self, ledger: &Ledger, destination: &Path) -> Result<()> {
        self.validate(ledger, true)?;
        require(!destination.exists(), "snapshot destination must be new")?;
        fs::create_dir(destination)?;
        for (path, file) in &self.files {
            let destination = destination.join(path);
            let parent = destination
                .parent()
                .ok_or_else(|| Error::new("source has no parent".into()))?;
            fs::create_dir_all(parent)?;
            fs::write(&destination, ledger.read(&file.content.digest)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &destination,
                    fs::Permissions::from_mode(if file.mode == 0o100755 { 0o555 } else { 0o444 }),
                )?;
            }
        }
        // A private controller umask must not make read-only mounts unreadable
        // to the sandbox's separate uid. Only the controller retains write access.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut directories = BTreeSet::from([destination.to_path_buf()]);
            for path in self.files.keys() {
                let full = destination.join(path);
                for parent in full
                    .ancestors()
                    .skip(1)
                    .take_while(|p| p.starts_with(destination))
                {
                    directories.insert(parent.to_path_buf());
                }
            }
            for directory in directories {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    }
}

/// Explicit extra scope for non-text, build, dependency or executable changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRules {
    /// Authorized binary paths, still subject to all ordinary scopes.
    pub binary_paths: Vec<String>,
    /// Authorized executable additions and mode changes.
    pub executable_paths: Vec<String>,
    /// Approved build/dependency/configuration changes require replanning.
    pub build_paths: Vec<String>,
}

/// Paths whose contents can change what is compiled or which tests execute.
pub fn is_build_input(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.toml"
            | "Cargo.lock"
            | "build.rs"
            | "CMakeLists.txt"
            | "CMakePresets.json"
            | "CMakeUserPresets.json"
            | "Makefile"
            | "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | ".npmrc"
            | "pyproject.toml"
            | "requirements.lock"
            | "uv.lock"
            | "poetry.lock"
            | "Pipfile"
            | "Pipfile.lock"
            | "setup.py"
            | "setup.cfg"
            | "pytest.ini"
            | "tox.ini"
            | "conftest.py"
            | ".clang-tidy"
    ) || path
        .split('/')
        .any(|p| matches!(p, ".cargo" | ".github" | "ci"))
        || name.ends_with(".cmake")
        || (name.starts_with("tsconfig") && name.ends_with(".json"))
        || (name.starts_with("requirements") && (name.ends_with(".txt") || name.ends_with(".lock")))
}

/// Compare a complete proposed tree against its approved baseline, including removals.
pub fn admit(
    ledger: &Ledger,
    base: &Snapshot,
    proposed: &Snapshot,
    policy: &EffectivePolicy,
    rules: &AdmissionRules,
) -> Result<Vec<String>> {
    base.validate(ledger, true)?;
    proposed.validate(ledger, true)?;
    require(
        base.base_revision == proposed.base_revision,
        "candidate changed its authorized base",
    )?;
    require(
        base.dependency_assets == proposed.dependency_assets,
        "dependency assets changed; approve and replan first",
    )?;
    for pattern in rules
        .binary_paths
        .iter()
        .chain(&rules.executable_paths)
        .chain(&rules.build_paths)
    {
        scope::validate_pattern(pattern)?;
    }
    let deleted: BTreeSet<_> = base
        .files
        .keys()
        .filter(|p| !proposed.files.contains_key(*p))
        .cloned()
        .collect();
    require(
        deleted == proposed.deletions.iter().cloned().collect(),
        "candidate deletion manifest is incomplete",
    )?;
    let paths: BTreeSet<_> = base
        .files
        .keys()
        .chain(proposed.files.keys())
        .cloned()
        .collect();
    let mut changed = Vec::new();
    for path in paths {
        let before = base.files.get(&path);
        let after = proposed.files.get(&path);
        if before == after {
            continue;
        }
        policy.check_write(&path)?;
        if is_build_input(&path) {
            require(
                rules.build_paths.iter().any(|p| scope::matches(p, &path)),
                format!("build-input-change-requires-approval:{path}"),
            )?;
        }
        if before.map(|f| f.mode) != after.map(|f| f.mode)
            && (before.is_some_and(|f| f.mode == 0o100755)
                || after.is_some_and(|f| f.mode == 0o100755))
        {
            require(
                rules
                    .executable_paths
                    .iter()
                    .any(|p| scope::matches(p, &path)),
                format!("executable-change-requires-approval:{path}"),
            )?;
        }
        if let Some(file) = after {
            let bytes = ledger.read(&file.content.digest)?;
            if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                require(
                    rules.binary_paths.iter().any(|p| scope::matches(p, &path)),
                    format!("binary-change-requires-approval:{path}"),
                )?;
            }
        }
        changed.push(path);
    }
    Ok(changed)
}
