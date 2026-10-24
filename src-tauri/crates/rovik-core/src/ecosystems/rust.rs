//! Rust: packages with a `Cargo.toml` (a workspace's own manifest among
//! them), in the project's folder or one level down (a Tauri app's
//! `src-tauri/`), the version their `rust-toolchain` file names, and
//! `cargo fetch` where nothing was built yet.

use super::{Ecosystem, Install, Project};
use crate::toolchains::{checked_name, rust_version_ok, Kind, Want};
use std::path::Path;

/// Rust's packages ([`Ecosystem`]).
pub struct Rust;

impl Ecosystem for Rust {
    fn name(&self) -> &'static str {
        "Rust"
    }

    fn programs(&self) -> &'static [&'static str] {
        // The shared rustup's proxies.
        &[
            "cargo",
            "rustc",
            "rustup",
            "rustfmt",
            "rustdoc",
            "cargo-clippy",
            "cargo-fmt",
            "clippy-driver",
        ]
    }

    fn present(&self, project: &Project) -> bool {
        project.with("Cargo.toml").next().is_some()
    }

    /// The version the project's folder names in its toolchain file, as
    /// rustup reads it there; else the first package's one folder down,
    /// where its commands run from; else `stable`.
    fn toolchain(&self, project: &Project) -> Option<Want> {
        let (version, components, targets) = std::iter::once("")
            .chain(project.with("Cargo.toml"))
            .find_map(|folder| toolchain_file(&project.dir(folder)))
            .unwrap_or_else(|| ("stable".into(), Vec::new(), Vec::new()));
        Some(Want {
            kind: Kind::Rust,
            version,
            components,
            targets,
        })
    }

    fn installs(&self, project: &Project) -> Vec<Install> {
        project
            .with("Cargo.toml")
            .filter(|folder| project.has(folder, "Cargo.lock") && !project.has(folder, "target"))
            .map(|folder| Install {
                folder: folder.to_string(),
                command: "cargo fetch".into(),
            })
            .collect()
    }
}

/// The version `dir`'s `rust-toolchain.toml` names in its `channel`, with
/// its `components` and `targets`, or its plain `rust-toolchain`'s. What
/// isn't a version Rovik installs is left to `stable`: a file that names
/// one (a date, a custom toolchain) is the project's own. None without a
/// file.
fn toolchain_file(dir: &Path) -> Option<(String, Vec<String>, Vec<String>)> {
    let text = std::fs::read_to_string(dir.join("rust-toolchain.toml"))
        .or_else(|_| std::fs::read_to_string(dir.join("rust-toolchain")))
        .ok()?;
    let channel = setting(&text, "channel").filter(|c| rust_version_ok(c));
    let version = match channel {
        Some(channel) => channel,
        // A plain `rust-toolchain` is the channel alone.
        None => match text.lines().map(str::trim).find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.contains('=')
                && !line.starts_with('[')
        }) {
            Some(line) if rust_version_ok(line) => line.to_string(),
            _ => "stable".into(),
        },
    };
    Some((
        version,
        setting_list(&text, "components"),
        setting_list(&text, "targets"),
    ))
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

#[cfg(test)]
mod tests {
    use super::super::tests::{at, commands, project, write};
    use super::*;

    fn rust(root: &Path) -> Want {
        Rust.toolchain(&Project::at(root)).unwrap()
    }

    /// `rust-toolchain.toml`'s channel with its components and targets; a
    /// plain `rust-toolchain`'s channel; `stable` without either.
    #[test]
    fn a_projects_toolchain_file_names_its_rust_version() {
        let dir = project(&[]);
        write(&dir.0, "Cargo.toml", "[package]\nname = \"t\"\n");
        assert_eq!(rust(&dir.0).version, "stable");
        write(
            &dir.0,
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"1.82\"\ncomponents = [\"rustfmt\", \"clippy\"]\n\
             targets = [\"x86_64-unknown-linux-gnu\"]\n",
        );
        let named = rust(&dir.0);
        assert_eq!(named.version, "1.82");
        assert_eq!(named.components, ["rustfmt", "clippy"]);
        assert_eq!(named.targets, ["x86_64-unknown-linux-gnu"]);
        std::fs::remove_file(dir.0.join("rust-toolchain.toml")).unwrap();
        write(&dir.0, "rust-toolchain", "nightly-2024-09-05\n");
        assert_eq!(rust(&dir.0).version, "nightly-2024-09-05");
    }

    /// A version Rovik doesn't install is the project's own: `stable`.
    #[test]
    fn a_toolchain_file_naming_another_version_is_left_alone() {
        let dir = project(&[]);
        write(&dir.0, "Cargo.toml", "[package]\n");
        write(
            &dir.0,
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"my-custom-toolchain\"\n",
        );
        assert_eq!(rust(&dir.0).version, "stable");
    }

    /// A Cargo project one folder down (a Tauri app's `src-tauri/`) is the
    /// project's Rust, with the version its own toolchain file names.
    #[test]
    fn a_cargo_project_one_folder_down_names_the_projects_rust() {
        let dir = project(&["package.json"]);
        write(
            &dir.0,
            "src-tauri/Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n",
        );
        assert!(Rust.present(&Project::at(&dir.0)));
        assert_eq!(rust(&dir.0).version, "stable");
        write(&dir.0, "src-tauri/rust-toolchain", "1.85\n");
        assert_eq!(rust(&dir.0).version, "1.85");
        // The project's own file wins.
        write(&dir.0, "rust-toolchain", "1.80\n");
        assert_eq!(rust(&dir.0).version, "1.80");
        assert!(!Rust.present(&Project::at(&project(&["package.json"]).0)));
    }

    /// `cargo fetch` where a lockfile's crates were never built; never in
    /// a crate without its lockfile (a workspace member's is its root's).
    #[test]
    fn cargo_fetch_where_nothing_was_built() {
        assert_eq!(
            commands(&project(&["Cargo.toml", "Cargo.lock"]).0),
            [at("", "cargo fetch")]
        );
        for files in [
            &["Cargo.toml", "Cargo.lock", "target/"][..],
            &["Cargo.toml"],
        ] {
            assert_eq!(commands(&project(files).0), [], "{files:?}");
        }
        let dir = project(&[
            "Cargo.toml",
            "Cargo.lock",
            "member/Cargo.toml",
            "api/Cargo.toml",
            "api/Cargo.lock",
        ]);
        assert_eq!(
            commands(&dir.0),
            [at("", "cargo fetch"), at("api", "cargo fetch")]
        );
    }
}
