//! The languages of a project's packages, as far as its commands need
//! them: where its packages are, the toolchain they run on, the installs
//! their dependencies take, and the programs each language brings. One
//! scan of the project answers all of it, so what a cloud host installs
//! (toolchains.rs), what Prepare installs (agent_run/prepare.rs) and what
//! is made ready around the agent's commands (agent_run/environment.rs)
//! agree.
//!
//! A language is one [`Ecosystem`]: Node (node.rs), Rust (rust.rs), Python
//! (python.rs), Go (go.rs). Another one (C, say) is a module implementing
//! it and its line in [`ALL`]; nothing else changes.
//!
//! Each reads the same folders ([`Project`]): the project's own, then
//! those one level down that may be packages of its own (a monorepo's
//! `api/` and `web/`). A Node workspace's members are read through its
//! manifest, however deep they are.

mod go;
mod node;
mod python;
mod rust;

use crate::project_profile::NOT_PACKAGES;
use crate::toolchains::Want;
use std::path::{Path, PathBuf};

/// An install a package needs where what it installs isn't there: its
/// lockfile's, or its package manager's without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// Where it runs, from the project's folder: "" for the folder itself.
    pub folder: String,
    /// The command line it runs there.
    pub command: String,
}

/// One language of a project's packages.
pub trait Ecosystem: Sync {
    /// Its name, for messages: "Node", "Rust".
    fn name(&self) -> &'static str;

    /// The programs it brings, as a command line names them (`npm`,
    /// `cargo`): what's missing among them is its toolchain's to provide.
    fn programs(&self) -> &'static [&'static str];

    /// Whether `project` has packages of its.
    fn present(&self, project: &Project) -> bool;

    /// The toolchain its programs run on: the version `project`'s packages
    /// ask for, else its default. None when a cloud host doesn't install
    /// it (the host's setup script does).
    fn toolchain(&self, project: &Project) -> Option<Want>;

    /// The installs its packages in `project` need, where what they
    /// install isn't there, in the order of `project.folders`.
    fn installs(&self, project: &Project) -> Vec<Install>;
}

/// Every ecosystem, in the order a folder's installs run.
pub const ALL: &[&dyn Ecosystem] = &[&node::Node, &rust::Rust, &python::Python, &go::Go];

/// The folders of a project the ecosystems read.
pub struct Project {
    /// The project's folder.
    pub root: PathBuf,
    /// "" (the project's own folder) first, then the folders one level
    /// down that may be packages of its own, by name: never hidden ones,
    /// nor what's installed, built or vendored there (`NOT_PACKAGES`).
    pub folders: Vec<String>,
}

impl Project {
    /// The project at `root`. A folder that can't be read has only its own.
    pub fn at(root: &Path) -> Project {
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
        Project {
            root: root.to_path_buf(),
            folders,
        }
    }

    /// `folder`'s path.
    pub fn dir(&self, folder: &str) -> PathBuf {
        self.root.join(folder)
    }

    /// Whether `name` is there in `folder`.
    pub fn has(&self, folder: &str, name: &str) -> bool {
        self.dir(folder).join(name).exists()
    }

    /// The folders with `manifest` in them, the project's own first.
    pub fn with<'a>(&'a self, manifest: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.folders
            .iter()
            .map(String::as_str)
            .filter(move |folder| self.has(folder, manifest))
    }
}

/// The toolchain versions the project at `root` asks for: one for each
/// ecosystem it has packages of whose toolchain a host installs.
pub fn wanted(root: &Path) -> Vec<Want> {
    let project = Project::at(root);
    ALL.iter()
        .filter(|ecosystem| ecosystem.present(&project))
        .filter_map(|ecosystem| ecosystem.toolchain(&project))
        .collect()
}

/// The toolchain `program` comes with, at the version the project at
/// `root` runs it on: Node for `npm`, at the version the project's
/// packages ask for, else Node's default. None for a program no toolchain
/// a host installs brings.
pub fn toolchain_for(program: &str, root: &Path) -> Option<Want> {
    of_program(program)?.toolchain(&Project::at(root))
}

/// The installs the project at `root` needs, where what they install
/// isn't there: folder by folder, the project's own first, and within a
/// folder in [`ALL`]'s order.
pub fn installs(root: &Path) -> Vec<Install> {
    installs_of(ALL, root)
}

/// [`installs`] of `ecosystems` alone.
pub fn installs_of(ecosystems: &[&dyn Ecosystem], root: &Path) -> Vec<Install> {
    let project = Project::at(root);
    let mut all: Vec<Install> = ecosystems
        .iter()
        .flat_map(|ecosystem| ecosystem.installs(&project))
        .collect();
    let place = |install: &Install| project.folders.iter().position(|f| *f == install.folder);
    // Stable: a folder's installs keep the ecosystems' order.
    all.sort_by_key(place);
    all
}

/// The ecosystem `program` is of: `npm` is Node's, `cargo` Rust's.
pub fn of_program(program: &str) -> Option<&'static dyn Ecosystem> {
    ALL.iter()
        .copied()
        .find(|ecosystem| ecosystem.programs().contains(&program))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A project with `files` (a name ending in `/` is a folder), each
    /// file `{}`.
    pub(crate) fn project(files: &[&str]) -> crate::testing::TempDir {
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

    /// Writes `content` to `path` in `dir`.
    pub(crate) fn write(dir: &Path, path: &str, content: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// The installs of `root`, as (folder, command).
    pub(crate) fn commands(root: &Path) -> Vec<(String, String)> {
        installs(root)
            .into_iter()
            .map(|i| (i.folder, i.command))
            .collect()
    }

    pub(crate) fn at(folder: &str, command: &str) -> (String, String) {
        (folder.to_string(), command.to_string())
    }

    /// The folders read are the project's own and those one level down
    /// that may be packages: never hidden, installed or built ones.
    #[test]
    fn a_project_is_its_folder_and_the_packages_one_level_down() {
        let dir = project(&[
            "web/",
            "api/",
            ".git/",
            "node_modules/",
            "target/",
            "notes.txt",
        ]);
        assert_eq!(Project::at(&dir.0).folders, ["", "api", "web"]);
        assert_eq!(Project::at(&dir.0.join("missing")).folders, [""]);
    }

    /// One folder down too (a monorepo's `api/` and `web/`), the project's
    /// own folder first; never in what's installed, built or hidden.
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

    /// Each program is its ecosystem's; others are nobody's.
    #[test]
    fn a_program_belongs_to_its_ecosystem() {
        for (program, name) in [
            ("npm", "Node"),
            ("node", "Node"),
            ("yarn", "Node"),
            ("pnpm", "Node"),
            ("cargo", "Rust"),
            ("rustup", "Rust"),
            ("python3", "Python"),
            ("go", "Go"),
        ] {
            assert_eq!(
                of_program(program).map(|e| e.name()),
                Some(name),
                "{program}"
            );
        }
        for program in ["make", "cd", "vite", "bash"] {
            assert!(of_program(program).is_none(), "{program}");
        }
    }

    /// A folder with packages of several ecosystems installs them in
    /// [`ALL`]'s order, folder by folder.
    #[test]
    fn a_folders_installs_keep_the_ecosystems_order() {
        let dir = project(&[
            "package.json",
            "package-lock.json",
            "Cargo.toml",
            "Cargo.lock",
            "api/go.mod",
            "api/go.sum",
            "api/requirements.txt",
        ]);
        assert_eq!(
            commands(&dir.0),
            [
                at("", "npm ci --no-audit --no-fund"),
                at("", "cargo fetch"),
                at("api", "pip install -r requirements.txt"),
                at("api", "go mod download"),
            ]
        );
    }
}
