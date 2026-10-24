//! Node: packages with a `package.json`, the Node version they ask for,
//! and the installs their package manager runs. A workspace (npm's or
//! Yarn's `workspaces`, a `pnpm-workspace.yaml`) installs from its root,
//! however its members are laid out, and a root whose manifest only lists
//! the members takes the Node version they name.

use super::{Ecosystem, Install, Project};
use crate::project_profile::workspaces::{detect_package_manager, is_yarn_berry, node_members};
use crate::toolchains::{Kind, Want};
use serde_json::Value;
use std::path::Path;

/// Node's packages ([`Ecosystem`]).
pub struct Node;

/// Node's lockfiles, any of which says how its packages install.
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "bun.lock",
];

/// npm's install of a package without a lockfile: at the latest versions
/// its package.json allows, writing no lockfile (it would be a file the
/// change adds).
const UNLOCKED_NPM: &str = "npm install --no-package-lock --no-audit --no-fund";

impl Ecosystem for Node {
    fn name(&self) -> &'static str {
        "Node"
    }

    fn programs(&self) -> &'static [&'static str] {
        // Yarn's and pnpm's through Corepack where Node has none of its own
        // (toolchains::COREPACK_PROGRAMS).
        &[
            "node", "npm", "npx", "corepack", "yarn", "yarnpkg", "pnpm", "pnpx",
        ]
    }

    fn present(&self, project: &Project) -> bool {
        project.with("package.json").next().is_some() || project.has("", "pnpm-workspace.yaml")
    }

    fn toolchain(&self, project: &Project) -> Option<Want> {
        Some(Want {
            kind: Kind::Node,
            version: version(project),
            components: Vec::new(),
            targets: Vec::new(),
        })
    }

    fn installs(&self, project: &Project) -> Vec<Install> {
        let (inside, root_is_one) = (
            below_a_workspace(&project.root),
            workspace_root(&project.root),
        );
        project
            .folders
            .iter()
            .filter_map(|folder| {
                let in_workspace = inside || (root_is_one && !folder.is_empty());
                install_in(project, folder, in_workspace)
            })
            .collect()
    }
}

/// The install `folder`'s package needs, where its dependencies aren't
/// there (`node_modules`, or Yarn's `.pnp.cjs`): its lockfile's, by its
/// package manager; without a lockfile, the manager's install that writes
/// none, for a package that names dependencies or a workspace. None for a
/// workspace's member (`in_workspace`), which its root installs.
fn install_in(project: &Project, folder: &str, in_workspace: bool) -> Option<Install> {
    let has = |name: &str| project.has(folder, name);
    if !has("package.json") || has("node_modules") || has(".pnp.cjs") {
        return None;
    }
    let dir = project.dir(folder);
    let pkg = package_json(&dir);
    let locked = LOCKFILES.iter().any(|lockfile| has(lockfile));
    let manager = Manager::of(&dir, pkg.as_ref(), locked);
    let command = match locked {
        true => manager.locked_install(),
        false if in_workspace || !names_dependencies(&dir, pkg.as_ref()) => return None,
        false => manager.unlocked_install()?,
    };
    Some(Install {
        folder: folder.to_string(),
        command: command.to_string(),
    })
}

/// A package's manager, as its lockfile, its `packageManager` or Yarn's
/// settings name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Manager {
    Npm,
    Pnpm,
    Yarn,
    /// Yarn 2 and later.
    YarnBerry,
    Bun,
}

impl Manager {
    /// The manager of the package in `dir`. A lockfile says which (npm's
    /// is npm's, whatever else is there); without one, Yarn's settings say
    /// Yarn 2 and later too.
    fn of(dir: &Path, pkg: Option<&Value>, locked: bool) -> Manager {
        match detect_package_manager(dir) {
            "pnpm" => Manager::Pnpm,
            "yarn" if is_yarn_berry(dir, pkg) => Manager::YarnBerry,
            "yarn" => Manager::Yarn,
            "bun" => Manager::Bun,
            _ if !locked && is_yarn_berry(dir, pkg) => Manager::YarnBerry,
            _ => Manager::Npm,
        }
    }

    /// The install of its lockfile, changing nothing in it.
    fn locked_install(self) -> &'static str {
        match self {
            // No audit: a request to the registry nobody reads here.
            Manager::Npm => "npm ci --no-audit --no-fund",
            Manager::Pnpm => "pnpm install --frozen-lockfile",
            Manager::Yarn => "yarn install --frozen-lockfile",
            Manager::YarnBerry => "yarn install --immutable",
            Manager::Bun => "bun install --frozen-lockfile",
        }
    }

    /// The install without a lockfile that writes none. None where the
    /// manager can't (Yarn 2 and later, Bun): its install would add one.
    fn unlocked_install(self) -> Option<&'static str> {
        match self {
            Manager::Npm => Some(UNLOCKED_NPM),
            Manager::Pnpm => Some("pnpm install --no-lockfile"),
            Manager::Yarn => Some("yarn install --no-lockfile"),
            Manager::YarnBerry | Manager::Bun => None,
        }
    }
}

/// Whether the package in `dir` names what an install would fetch: its
/// package.json's dependencies, or the packages of the workspace it's the
/// root of.
fn names_dependencies(dir: &Path, pkg: Option<&Value>) -> bool {
    let Some(pkg) = pkg else {
        return false;
    };
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|key| pkg[*key].as_object().is_some_and(|deps| !deps.is_empty()))
        || workspace_root(dir)
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

/// The Node version the project names: its own folder's; else the newest
/// its other packages name (a workspace's members, wherever they are, and
/// the packages one folder down), since one Node runs them all; else
/// `lts`.
fn version(project: &Project) -> String {
    if let Some(own) = named(&project.root) {
        return own;
    }
    let mut packages: Vec<String> =
        node_members(&project.root, package_json(&project.root).as_ref());
    packages.extend(
        project
            .with("package.json")
            .filter(|f| !f.is_empty())
            .map(str::to_string),
    );
    packages
        .iter()
        .filter_map(|folder| named(&project.root.join(folder)))
        .max_by(|one, other| crate::toolchains::version_order(one, other))
        .unwrap_or_else(|| "lts".into())
}

/// The Node version `dir` names: `.nvmrc`, `.node-version`, else the
/// major version in its package.json's `engines.node`. What isn't a
/// version Rovik installs (`lts/*`, `node`) names none.
fn named(dir: &Path) -> Option<String> {
    let written = [".nvmrc", ".node-version"].iter().find_map(|name| {
        let text = std::fs::read_to_string(dir.join(name)).ok()?;
        let version = text.lines().next()?.trim().trim_start_matches('v');
        (!version.is_empty()).then(|| version.to_string())
    });
    written
        .filter(|v| crate::toolchains::node_version_ok(v))
        .or_else(|| engines_major(dir))
}

/// The first major version `dir`'s package.json `engines.node` allows
/// (`>=18 <21` is `18`), where it names one.
fn engines_major(dir: &Path) -> Option<String> {
    let package = package_json(dir)?;
    let range = package["engines"]["node"].as_str()?;
    let start = range.find(|c: char| c.is_ascii_digit())?;
    let major: String = range[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    crate::toolchains::digits(&major).then_some(major)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at, commands, project, write};
    use super::*;

    /// A package.json that names a dependency.
    const DEPENDS: &str = r#"{ "name": "t", "devDependencies": { "vitest": "^3.2.0" } }"#;

    fn node_version(root: &Path) -> String {
        Node.toolchain(&Project::at(root)).unwrap().version
    }

    /// 4.3: each lockfile's install, by its package manager.
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
        ];
        for (files, command) in cases {
            let dir = project(files);
            assert_eq!(commands(&dir.0), [at("", command)], "{files:?}");
        }
    }

    /// Installed already, or nothing to install from: nothing.
    #[test]
    fn installed_dependencies_or_nothing_named_need_nothing() {
        let cases: &[&[&str]] = &[
            &["package.json", "package-lock.json", "node_modules/"],
            &["package.json", "yarn.lock", ".pnp.cjs"],
            &["package.json"],
            &["package-lock.json"],
            // `node_modules` as a file is there all the same.
            &["package.json", "package-lock.json", "node_modules"],
        ];
        for files in cases {
            let dir = project(files);
            assert_eq!(commands(&dir.0), [], "{files:?}");
        }
    }

    /// A Node package that commits no lockfile, as a server's copy of it
    /// has it: its package manager installs what its package.json names,
    /// writing no lockfile.
    #[test]
    fn a_package_without_a_lockfile_installs_what_it_names_writing_none() {
        let npm = "npm install --no-package-lock --no-audit --no-fund";
        let cases: &[(&str, &[&str], &str)] = &[
            (DEPENDS, &[], npm),
            (r#"{ "dependencies": { "left-pad": "1.3.0" } }"#, &[], npm),
            (
                r#"{ "optionalDependencies": { "fsevents": "*" } }"#,
                &[],
                npm,
            ),
            (
                r#"{ "packageManager": "npm@10.8.2", "dependencies": { "a": "1" } }"#,
                &[],
                npm,
            ),
            (
                r#"{ "private": true, "workspaces": ["packages/*"] }"#,
                &[],
                npm,
            ),
            (
                r#"{ "packageManager": "pnpm@9.1.0", "dependencies": { "a": "1" } }"#,
                &[],
                "pnpm install --no-lockfile",
            ),
            (
                DEPENDS,
                &["pnpm-workspace.yaml"],
                "pnpm install --no-lockfile",
            ),
            (
                r#"{ "packageManager": "yarn@1.22.22", "dependencies": { "a": "1" } }"#,
                &[],
                "yarn install --no-lockfile",
            ),
        ];
        for (manifest, files, command) in cases {
            let dir = project(files);
            std::fs::create_dir_all(dir.0.join(".git")).unwrap();
            write(&dir.0, "package.json", manifest);
            assert_eq!(commands(&dir.0), [at("", command)], "{manifest} {files:?}");
        }
    }

    /// Nothing for an install to fetch, or a manager whose install would
    /// write a lockfile: nothing.
    #[test]
    fn a_package_without_a_lockfile_that_names_nothing_needs_nothing() {
        let cases: &[(&str, &[&str])] = &[
            (r#"{ "scripts": { "test": "node t.js" } }"#, &[]),
            (r#"{ "dependencies": {} }"#, &[]),
            ("not json", &[]),
            (DEPENDS, &["node_modules/"]),
            (DEPENDS, &[".pnp.cjs"]),
            // Yarn 2 and later, and Bun, write a lockfile on any install.
            (
                r#"{ "packageManager": "yarn@4.5.0", "dependencies": { "a": "1" } }"#,
                &[],
            ),
            (DEPENDS, &[".yarnrc.yml"]),
            (
                r#"{ "packageManager": "bun@1.1.0", "dependencies": { "a": "1" } }"#,
                &[],
            ),
        ];
        for (manifest, files) in cases {
            let dir = project(files);
            std::fs::create_dir_all(dir.0.join(".git")).unwrap();
            write(&dir.0, "package.json", manifest);
            assert_eq!(commands(&dir.0), [], "{manifest} {files:?}");
        }
    }

    /// A workspace's packages install with it, from its root: never each
    /// on its own, one folder down or as the project's own folder.
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
        // A pnpm workspace with no lockfile: pnpm's install, at its root.
        let pnpm = project(&[".git/", "pnpm-workspace.yaml"]);
        write(&pnpm.0, "package.json", "{}");
        write(&pnpm.0, "web/package.json", DEPENDS);
        assert_eq!(commands(&pnpm.0), [at("", "pnpm install --no-lockfile")]);
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

    /// The version the project's own folder names: `.nvmrc` first, then
    /// `.node-version`, then the major version `engines.node` allows; LTS
    /// when it names none Rovik installs.
    #[test]
    fn the_projects_own_folder_names_its_node_version() {
        let dir = project(&[]);
        write(&dir.0, "package.json", "{}");
        assert_eq!(node_version(&dir.0), "lts");
        write(&dir.0, ".nvmrc", "v20.11.0\n");
        assert_eq!(node_version(&dir.0), "20.11.0");
        std::fs::remove_file(dir.0.join(".nvmrc")).unwrap();
        write(&dir.0, ".node-version", "22\n");
        assert_eq!(node_version(&dir.0), "22");
        std::fs::remove_file(dir.0.join(".node-version")).unwrap();
        write(
            &dir.0,
            "package.json",
            r#"{ "engines": { "node": ">=18 <21" } }"#,
        );
        assert_eq!(node_version(&dir.0), "18");
        // `lts/*` isn't a version Rovik installs: the engines say.
        write(&dir.0, ".nvmrc", "lts/*\n");
        assert_eq!(node_version(&dir.0), "18");
    }

    /// A workspace root whose manifest only lists its members takes the
    /// newest version they name, however deep they are; the root's own
    /// wins over them. Packages one folder down count too.
    #[test]
    fn a_workspace_only_root_takes_the_version_its_members_name() {
        let dir = project(&[".git/"]);
        write(
            &dir.0,
            "package.json",
            r#"{ "private": true, "workspaces": ["apps/*", "packages/*"] }"#,
        );
        write(
            &dir.0,
            "apps/web/package.json",
            r#"{ "engines": { "node": ">=20" } }"#,
        );
        write(&dir.0, "packages/ui/package.json", "{}");
        write(&dir.0, "packages/ui/.nvmrc", "18.19.0\n");
        assert_eq!(node_version(&dir.0), "20");
        write(&dir.0, "apps/api/package.json", "{}");
        write(&dir.0, "apps/api/.node-version", "22.3.0\n");
        assert_eq!(node_version(&dir.0), "22.3.0");
        write(&dir.0, ".nvmrc", "20\n");
        assert_eq!(node_version(&dir.0), "20");
        // A monorepo without a workspace: its packages one folder down.
        let apart = project(&[]);
        write(
            &apart.0,
            "web/package.json",
            r#"{ "engines": { "node": "^21.1.0" } }"#,
        );
        assert_eq!(node_version(&apart.0), "21");
        assert!(Node.present(&Project::at(&apart.0)));
    }

    /// Node is there for a package one folder down, or a pnpm workspace's
    /// file alone; not for a project without either.
    #[test]
    fn node_is_present_with_a_package_anywhere_the_scan_reads() {
        let present = |files: &[&str]| Node.present(&Project::at(&project(files).0));
        assert!(present(&["package.json"]));
        assert!(present(&["web/package.json"]));
        assert!(present(&["pnpm-workspace.yaml"]));
        assert!(!present(&["Cargo.toml", "node_modules/x/package.json"]));
    }
}
