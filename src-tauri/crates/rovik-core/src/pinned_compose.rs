//! The Docker Compose a cloud host runs its users' project services with
//! (D21): one release on every server, each build checked against the
//! SHA-256 recorded here. Root installs it in `/opt/rovik/compose/<version>`
//! (`rovik-agentd admin containers`, which `install` and `admin upgrade`
//! run). The daemons read a project's compose file with it and start the
//! file's services under each user's own Podman (services/).
//!
//! It's Docker's own Compose, a single program, rather than a distribution's
//! package: most distributions have none at a recent version, and every
//! server then reads compose files the same way. A new version is a new
//! `VERSION` with its builds' hashes, which `scripts/pin-compose.sh VERSION`
//! prints.

use std::path::{Path, PathBuf};

/// What it's called in messages.
pub const NAME: &str = "Docker Compose";

/// The version every server has.
pub const VERSION: &str = "5.6.1";

/// Where a cloud host keeps it, each version in a folder of its own.
pub const SERVER_ROOT: &str = "/opt/rovik/compose";

/// The program's name in its version's folder.
pub const PROGRAM: &str = "docker-compose";

/// One CPU's build: the release's program for Linux on it.
#[derive(Debug, PartialEq, Eq)]
pub struct Build {
    /// The CPU, as Rust names it.
    pub cpu: &'static str,
    /// The release's file for it.
    pub asset: &'static str,
    /// The file's SHA-256, as the release's `checksums.txt` has it.
    pub sha256: &'static str,
}

/// The builds of `VERSION`: the servers run Linux on these CPUs (D16).
pub const BUILDS: &[Build] = &[
    Build {
        cpu: "x86_64",
        asset: "docker-compose-linux-x86_64",
        sha256: "47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0",
    },
    Build {
        cpu: "aarch64",
        asset: "docker-compose-linux-aarch64",
        sha256: "42cfa75f51ad0ccf290bad37899e0038a8c10a0069e85233ecc1d53e3ff1347d",
    },
];

impl Build {
    /// The build for `cpu`, if the release has one.
    pub fn for_cpu(cpu: &str) -> Option<&'static Build> {
        BUILDS.iter().find(|build| build.cpu == cpu)
    }

    /// Where Docker publishes it.
    pub fn url(&self) -> String {
        format!(
            "https://github.com/docker/compose/releases/download/v{VERSION}/{}",
            self.asset
        )
    }
}

/// The program in a folder of versions `root` (`SERVER_ROOT` on a host).
pub fn program_in(root: &Path) -> PathBuf {
    root.join(VERSION).join(PROGRAM)
}

/// The pinned program where a cloud host keeps it, when it's there.
pub fn installed() -> Option<PathBuf> {
    let program = program_in(Path::new(SERVER_ROOT));
    program.is_file().then_some(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cpu_the_daemon_is_built_for_has_a_build() {
        for cpu in ["x86_64", "aarch64"] {
            let build = Build::for_cpu(cpu).expect("a build for each server CPU");
            assert_eq!(build.sha256.len(), 64, "{cpu}: a SHA-256 in hex");
            assert!(build.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_eq!(Build::for_cpu("riscv64"), None);
    }

    #[test]
    fn a_build_is_found_where_docker_publishes_it() {
        let build = Build::for_cpu("x86_64").unwrap();
        assert_eq!(
            build.url(),
            "https://github.com/docker/compose/releases/download/v5.6.1/docker-compose-linux-x86_64"
        );
        assert_eq!(
            program_in(Path::new(SERVER_ROOT)),
            Path::new("/opt/rovik/compose/5.6.1/docker-compose")
        );
    }
}
