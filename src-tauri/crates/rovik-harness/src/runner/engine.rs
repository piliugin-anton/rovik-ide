//! Engine-specific host capabilities. The runner owns all workload policy.

use super::{BACKEND_VERSION, PODMAN_BACKEND_VERSION};
use crate::{require, Result};
use std::{fmt::Debug, path::Path, process::Command};

pub(super) trait Engine: Debug + Send + Sync {
    fn version(&self) -> &'static str;
    fn command(&self) -> Command;
    fn create_options(&self, command: &mut Command);
    fn needs_cgroup_probe(&self) -> bool;
}

#[derive(Debug)]
pub(super) struct Management;
impl Engine for Management {
    fn version(&self) -> &'static str {
        "no-execution-capability"
    }
    fn command(&self) -> Command {
        Command::new("/__rovik_no_execution_capability__/unavailable")
    }
    fn create_options(&self, _: &mut Command) {}
    fn needs_cgroup_probe(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub(super) struct Docker(pub std::path::PathBuf);

impl Engine for Docker {
    fn version(&self) -> &'static str {
        BACKEND_VERSION
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.0);
        for key in [
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
        ] {
            command.env_remove(key);
        }
        command.arg("--host=unix:///var/run/docker.sock");
        command
    }
    fn create_options(&self, _: &mut Command) {}
    fn needs_cgroup_probe(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub(super) struct Podman {
    executable: std::path::PathBuf,
    state: std::path::PathBuf,
}

impl Podman {
    pub(super) fn new(executable: &Path, state: &Path) -> Result<Self> {
        require(
            state.is_absolute(),
            "Podman state must be an absolute private directory",
        )?;
        let state = crate::artifacts::private_directory(state)?.canonicalize()?;
        // A dedicated store prevents project services' mutable containers.conf
        // (which deliberately disables cgroups) from weakening verification.
        for name in ["storage", "run", "tmp", "config"] {
            crate::artifacts::private_directory(&state.join(name))?;
        }
        let storage = state.join("storage.conf");
        let config = b"[storage]\ndriver = \"vfs\"\n";
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&storage) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(config)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                require(
                    std::fs::symlink_metadata(&storage)?.is_file()
                        && std::fs::read(&storage)? == config,
                    "Podman verification storage configuration changed",
                )?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            executable: executable.into(),
            state,
        })
    }
}

impl Engine for Podman {
    fn version(&self) -> &'static str {
        PODMAN_BACKEND_VERSION
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        for key in [
            "CONTAINER_HOST",
            "CONTAINER_CONNECTION",
            "CONTAINER_SSHKEY",
            "CONTAINERS_CONF",
            "CONTAINERS_CONF_OVERRIDE",
            "CONTAINERS_CONF_MODULES",
            "CONTAINERS_STORAGE_CONF",
            "CONTAINERS_REGISTRIES_CONF",
            "CONTAINERS_REGISTRIES_CONF_DIR",
            "CONTAINERS_POLICY",
            "STORAGE_DRIVER",
            "STORAGE_OPTS",
        ] {
            command.env_remove(key);
        }
        command
            .env("CONTAINERS_CONF", "/dev/null")
            .env("CONTAINERS_STORAGE_CONF", self.state.join("storage.conf"))
            .env("XDG_CONFIG_HOME", self.state.join("config"))
            .env("XDG_RUNTIME_DIR", self.state.join("run"))
            .env("TMPDIR", self.state.join("tmp"))
            .args([
                "--remote=false",
                "--cgroup-manager=cgroupfs",
                "--events-backend=file",
                "--storage-driver=vfs",
                "--root",
            ])
            .arg(self.state.join("storage"))
            .arg("--runroot")
            .arg(self.state.join("run"))
            .arg("--tmpdir")
            .arg(self.state.join("tmp"));
        command
    }
    fn create_options(&self, command: &mut Command) {
        // Keep the input owner readable without chowning any immutable source.
        command.args([
            "--userns=keep-id:uid=65534,gid=65534",
            "--cgroups=enabled",
            "--cgroupns=private",
            "--http-proxy=false",
        ]);
    }
    fn needs_cgroup_probe(&self) -> bool {
        true
    }
}

pub(super) fn validate_podman_host(bytes: &[u8]) -> Result<()> {
    let info: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| crate::Error::new(format!("invalid Podman host report: {e}")))?;
    require(
        info.pointer("/host/security/rootless")
            .and_then(|v| v.as_bool())
            == Some(true),
        "the Podman verification backend requires rootless Podman",
    )?;
    require(
        info.pointer("/host/cgroupVersion").and_then(|v| v.as_str()) == Some("v2"),
        "Podman verification requires cgroup v2 with delegated memory, CPU and pids controllers",
    )
}

pub(super) const CGROUP_PROBE: &str =
    "set -eu; for f in memory.max memory.swap.max pids.max cpu.max; do cat /sys/fs/cgroup/$f; done";

pub(super) fn validate_cgroup_limits(
    bytes: &[u8],
    expected: &crate::registry::Limits,
) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| crate::Error::new("invalid container resource report".into()))?;
    let numbers = text.split_whitespace().map(str::parse::<u64>).collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| crate::Error::new("Podman did not enforce finite resource limits; delegate cgroup v2 memory, CPU and pids controllers to the controller service".into()))?;
    require(numbers.len() == 5
        && numbers[0] == expected.memory_bytes
        && numbers[1] == 0
        && numbers[2] == u64::from(expected.processes)
        && numbers[3] > 0 && numbers[4] > 0
        && u128::from(numbers[3]) * 1000 == u128::from(numbers[4]) * u128::from(expected.cpu_millis),
        "Podman resource limits differ from the approved check; delegate cgroup v2 memory, CPU and pids controllers to the controller service")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podman_is_local_and_cannot_inherit_disabled_cgroups_or_another_store() {
        let directory = tempfile::tempdir().unwrap();
        let engine = Podman::new(
            Path::new("/usr/bin/podman"),
            &directory.path().join("podman"),
        )
        .unwrap();
        let mut command = engine.command();
        command.arg("create");
        engine.create_options(&mut command);
        let args: Vec<_> = command.get_args().map(|s| s.to_string_lossy()).collect();
        assert!(args.contains(&"--remote=false".into()));
        assert!(args.contains(&"--cgroups=enabled".into()));
        assert!(args.contains(&"--cgroupns=private".into()));
        assert!(args.contains(&"--userns=keep-id:uid=65534,gid=65534".into()));
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(environment[std::ffi::OsStr::new("CONTAINER_HOST")], None);
        assert_eq!(
            environment[std::ffi::OsStr::new("CONTAINERS_CONF")],
            Some(std::ffi::OsStr::new("/dev/null"))
        );
        assert!(engine.needs_cgroup_probe());
        assert_ne!(engine.version(), Docker("/usr/bin/docker".into()).version());
    }

    #[test]
    fn rootful_hosts_old_cgroups_and_missing_host_facts_are_rejected() {
        validate_podman_host(br#"{"host":{"security":{"rootless":true},"cgroupVersion":"v2"}}"#)
            .unwrap();
        for invalid in [
            br#"{"host":{"security":{"rootless":false},"cgroupVersion":"v2"}}"#.as_slice(),
            br#"{"host":{"security":{"rootless":true},"cgroupVersion":"v1"}}"#,
            b"{}",
        ] {
            assert!(validate_podman_host(invalid).is_err());
        }
    }

    #[test]
    fn ignored_or_partial_resource_limits_never_authorize_work() {
        let limits = crate::registry::Limits {
            timeout_ms: 1000,
            output_bytes: 4096,
            memory_bytes: 67108864,
            processes: 16,
            cpu_millis: 1500,
            build_bytes: 16777216,
        };
        validate_cgroup_limits(b"67108864\n0\n16\n150000 100000\n", &limits).unwrap();
        for report in [
            "max\n0\n16\n150000 100000",
            "67108864\nmax\n16\n150000 100000",
            "67108864\n1\n16\n150000 100000",
            "67108864\n0\n32\n150000 100000",
            "67108864\n0\n16\nmax 100000",
            "67108864\n0\n16\n200000 100000",
            "67108864\n0\n16\n0 0",
            "67108864\n0\n16",
            "",
        ] {
            assert!(
                validate_cgroup_limits(report.as_bytes(), &limits).is_err(),
                "{report}"
            );
        }
    }
}
