#![cfg(target_os = "linux")]

use crate::{
    approvals::Principal,
    controller::Controller,
    fixtures,
    identity::Digest,
    ledger::Ledger,
    runner::{self, Cancellation, ContainerBackend, PODMAN_BACKEND_VERSION},
    state::State,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn cancelled_host_qualification_does_not_start_an_engine_process() {
    let root = tempfile::tempdir().unwrap();
    let backend = engine(root.path());
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(backend
        .validate_host(&cancel)
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    assert!(!root.path().join("commands").exists());
}

fn engine(root: &Path) -> ContainerBackend {
    let binary = root.join("podman");
    fs::write(&binary, r#"#!/bin/sh
set -eu
here=$(dirname "$0")
printf '%s\n' "$@" >> "$here/commands"
while test "$#" -gt 0; do
  case "$1" in
    info) printf '{"host":{"security":{"rootless":true},"cgroupVersion":"v2"}}'; exit ;;
    image) cat "$here/image"; exit ;;
    create) shift; while test "$1" != --name; do shift; done; printf '%s\n' "$2" > "$here/job"; exit ;;
    start) exit ;;
    exec) case "$*" in *memory.max*) cat "$here/limits" ;; *) printf ran > "$here/workload"; printf output ;; esac; exit ;;
    inspect) printf false; exit ;;
    ps) if test -f "$here/job"; then cat "$here/job"; fi; exit ;;
    rm) rm "$here/job"; printf removed > "$here/removed"; exit ;;
  esac
  shift
done
exit 71
"#).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    ContainerBackend::podman_fixture(&binary, root)
}

#[test]
fn podman_checks_actual_limits_before_work_and_removes_rejected_containers() {
    for valid in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let backend = engine(root.path());
        let ledger = Ledger::open(&root.path().join("ledger")).unwrap();
        let image = Digest::of(b"image");
        // Podman prints its image configuration ID without the sha256 prefix.
        fs::write(
            root.path().join("image"),
            image.as_str().trim_start_matches("sha256:"),
        )
        .unwrap();
        let mut setup = fixtures::quota_rust(&ledger, "podman", image, "base").unwrap();
        setup.environment.backend_version = backend.version().into();
        let check = &setup.plan.checks[0];
        let limits = &setup.registry.get(&check.command_id).unwrap().limits;
        let report = if valid {
            format!(
                "{}\n0\n{}\n{} 100000\n",
                limits.memory_bytes,
                limits.processes,
                limits.cpu_millis * 100
            )
        } else {
            "max\nmax\nmax\nmax 100000\n".into()
        };
        fs::write(root.path().join("limits"), report).unwrap();
        let result = backend.execute(
            &ledger,
            &setup.base,
            &setup.oracle,
            &setup.dependencies,
            &setup.environment,
            &setup.registry,
            check,
            &Cancellation::default(),
        );
        assert_eq!(result.is_ok(), valid, "{result:?}");
        assert_eq!(root.path().join("workload").exists(), valid);
        assert!(root.path().join("removed").exists());
        assert!(!root.path().join("job").exists());
        let commands = fs::read_to_string(root.path().join("commands")).unwrap();
        for required in [
            "--network=none",
            "--read-only",
            "--pull=never",
            "--cap-drop=ALL",
            "--cgroups=enabled",
            "--cgroup-parent",
            "/service",
            "org.rovik.harness.backend=rovik-podman-v1",
        ] {
            assert!(commands.lines().any(|line| line == required), "{required}");
        }
        assert!(!commands.lines().any(|line| line == "--cgroups=split"));
        assert_eq!(
            fs::read_to_string(root.path().join("membership")).unwrap(),
            "0::/service/rovik-controller\n"
        );
    }
}

#[test]
fn changing_engines_cannot_create_or_recover_an_approved_environment() {
    let root = tempfile::tempdir().unwrap();
    let backend = engine(root.path());
    let mut controller = Controller {
        ledger: Ledger::open(&root.path().join("ledger")).unwrap(),
        backend,
    };
    let mut setup =
        fixtures::quota_rust(&controller.ledger, "podman", Digest::of(b"image"), "base").unwrap();
    let docker_digest = Digest::of_json(&setup.environment).unwrap();
    assert!(controller
        .create(&setup, runner::now_ms().unwrap())
        .is_err());
    setup.environment.backend_version = PODMAN_BACKEND_VERSION.into();
    assert_ne!(docker_digest, Digest::of_json(&setup.environment).unwrap());
    controller
        .create(&setup, runner::now_ms().unwrap())
        .unwrap();
    controller.backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
    assert!(controller
        .recover(&Principal::current().unwrap(), "podman")
        .is_err());
    assert_eq!(
        controller.ledger.run("podman").unwrap().state,
        State::Intake
    );
    assert!(!root.path().join("commands").exists());
}
