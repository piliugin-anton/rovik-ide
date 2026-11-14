//! Build a reusable verifier environment from sources shipped with this IDE.
//!
//! Preparation may fetch pinned base images and locked compiler dependencies.
//! Verification still receives only a measured immutable image and offline inputs.
//! Neither the project nor the model supplies a Dockerfile or verifier executable.

use crate::{
    artifacts::private_directory,
    identity::Digest,
    process::{process, ProcessOutput},
    require,
    runner::{Cancellation, ContainerBackend, BACKEND_VERSION, PODMAN_BACKEND_VERSION},
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

mod dependencies;

include!(concat!(env!("OUT_DIR"), "/preparation-sources.rs"));

const BUILD_LIMIT: Duration = Duration::from_secs(30 * 60);
const CONTROL_LIMIT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const METADATA_LIMIT: u64 = 64 * 1024;
// Official rust:1.99.1-slim-bookworm, linux/amd64 manifest (2026-11-05).
// The legacy native fixture intentionally keeps its original Rust 1.85 profile;
// automatic preparation needs the MSRV of the helper shipped with this IDE.
const COMPILER_BASE: &str =
    "FROM rust@sha256:392f221964669402bf4ae133d2a46c3ae270f64734133f4b2703d40019235c1a";
const RECIPE_LABEL: &str = "dev.rovik.verifier-recipe";
const TOOLCHAIN_PROBE: &str = r#"set -eu
test -x /usr/local/bin/rovik-harness-tool
test -x /usr/local/bin/rovik-clang-graph
printf 'architecture=%s\n' "$(uname -m)"
printf 'rust_toolchain=%s\n' "$(rustup show active-toolchain | cut -d ' ' -f 1)"
printf 'rust_version=%s\n' "$(rustc --version | cut -d ' ' -f 2)"
printf 'cargo_version=%s\n' "$(cargo --version | cut -d ' ' -f 2)"
printf 'node_version=%s\n' "$(node --version | sed 's/^v//')"
printf 'npm_version=%s\n' "$(npm --version)"
printf 'typescript_version=%s\n' "$(tsc --version | cut -d ' ' -f 2)"
printf 'python_version=%s\n' "$(python3 --version | cut -d ' ' -f 2)"
printf 'pytest_version=%s\n' "$(python3 -c 'import pytest; print(pytest.__version__)')"
printf 'gcc_version=%s\n' "$(gcc -dumpfullversion -dumpversion)"
printf 'clang_version=%s\n' "$(clang --version | head -n 1)"
"#;

/// Host-owned execution assets. Model-authored setup fields cannot replace these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedEnvironment {
    /// Immutable local image configuration identity.
    pub image_digest: Digest,
    /// Measured tools, image, recipe and backend identities in private storage.
    pub toolchain_manifest: PathBuf,
    /// Exact bundled helper source, lockfile and fixed build recipe.
    pub recipe_digest: Digest,
    /// The same engine identity later enforced by verification.
    pub backend_version: String,
    /// Actual installed versions for language-profile preparation.
    pub facts: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    recipe_digest: Digest,
    image_digest: Digest,
    toolchain_digest: Digest,
    backend_version: String,
    facts: BTreeMap<String, String>,
}

fn recipe() -> Result<String> {
    let native = std::str::from_utf8(source("fixtures/native/Dockerfile")?)
        .map_err(|_| Error::new("bundled verifier recipe is not UTF-8".into()))?;
    let fixture_rust = native
        .lines()
        .find(|line| line.starts_with("FROM rust@sha256:"))
        .ok_or_else(|| Error::new("bundled verifier recipe has no pinned Rust base".into()))?;
    require(
        native.contains("apt-get install -y --no-install-recommends cmake"),
        "bundled verifier recipe has no native compiler installation",
    )?;
    let native = native.replacen(fixture_rust, COMPILER_BASE, 1).replace(
        "apt-get install -y --no-install-recommends cmake",
        "apt-get install -y --no-install-recommends g++ cmake",
    );
    let rust = COMPILER_BASE;
    let node = native
        .lines()
        .find(|line| line.starts_with("FROM node@sha256:") && line.ends_with(" AS node"))
        .ok_or_else(|| Error::new("bundled verifier recipe has no pinned Node base".into()))?;
    require(
        native.contains("COPY --chmod=755 rovik-harness-tool /usr/local/bin/rovik-harness-tool"),
        "bundled verifier recipe does not install its helper",
    )?;
    let replaced = native.replacen(&format!("{node}\n"), "", 1).replace(
        "COPY --chmod=755 rovik-harness-tool /usr/local/bin/rovik-harness-tool",
        "COPY --from=verifier --chmod=755 /out/rovik-harness-tool /usr/local/bin/rovik-harness-tool",
    );
    Ok(format!(
        r#"{node}
{rust} AS verifier
COPY --from=node /usr/local/bin/node /usr/local/bin/node
WORKDIR /build
ENV CARGO_HTTP_MULTIPLEXING=false
COPY helper/Cargo.toml helper/Cargo.lock helper/trusted-lock.json ./
COPY helper/src/preparation/verify-lock.mjs /verify-lock.mjs
RUN --mount=type=secret,id=proxy_ca \
    if [ -f /run/secrets/proxy_ca ]; then export CARGO_HTTP_CAINFO=/run/secrets/proxy_ca; fi && \
    mkdir -p src && touch src/lib.rs && printf 'fn main() {{}}\n' > build.rs && \
    cargo metadata --format-version=1 --filter-platform=x86_64-unknown-linux-gnu > /tmp/helper-metadata.json && \
    node /verify-lock.mjs && cp Cargo.lock /tmp/rovik-helper.lock && \
    rm build.rs src/lib.rs /tmp/helper-metadata.json
COPY helper/ ./
RUN --network=none cp /tmp/rovik-helper.lock Cargo.lock && \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_DEBUG=0 cargo build --locked --offline --release --bin rovik-harness-tool && \
    mkdir /out && cp target/release/rovik-harness-tool /out/ && \
    rm -rf target /usr/local/cargo/registry /usr/local/cargo/git /tmp/rovik-helper.lock

{replaced}"#
    ))
}

fn source(name: &str) -> Result<&'static [u8]> {
    SOURCE_FILES
        .iter()
        .find_map(|(path, bytes)| (*path == name).then_some(*bytes))
        .ok_or_else(|| Error::new(format!("bundled verifier source is missing: {name}")))
}

fn recipe_digest(recipe: &str) -> Result<Digest> {
    let files = SOURCE_FILES
        .iter()
        .map(|(path, bytes)| (*path, Digest::of(bytes)))
        .collect::<BTreeMap<_, _>>();
    Digest::of_json(&(recipe, files))
}

// Honor a host-administered trust bundle without disabling TLS verification or
// persisting certificate material in an image layer. Project/model fields cannot
// choose this path. Proxy variables pass by name so credentials never enter argv.
fn certificate() -> Result<Option<Vec<u8>>> {
    let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() <= 2 * 1024 * 1024,
        "The host TLS certificate bundle exceeds 2 MiB.",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("The host TLS certificate bundle is not PEM text.".into()))?;
    require(
        text.contains("-----BEGIN CERTIFICATE-----") && !text.contains("PRIVATE KEY"),
        "SSL_CERT_FILE must contain public PEM certificates only.",
    )?;
    Ok(Some(bytes))
}

fn proxy_options(command: &mut Command, option: &str) {
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ] {
        if std::env::var_os(name).is_some_and(|value| !value.is_empty()) {
            command.arg(option).arg(name);
        }
    }
}

fn cancelled(cancel: &Cancellation) -> Result<()> {
    require(
        !cancel.is_cancelled(),
        "Verification environment preparation was cancelled.",
    )
}

fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= METADATA_LIMIT,
        "verification environment cache metadata changed or exceeds its limit",
    )?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= METADATA_LIMIT,
        "verification environment metadata grew while reading",
    )?;
    Ok(bytes)
}

fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("runtime cache path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::from(error.error))?;
    Ok(())
}

fn lock(directory: &Path, cancel: &Cancellation) -> Result<File> {
    let path = directory.join("prepare.lock");
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        require(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "runtime preparation lock is not a regular file",
        )?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    let started = Instant::now();
    loop {
        cancelled(cancel)?;
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::Error(error)) => return Err(error.into()),
            Err(TryLockError::WouldBlock) => {
                require(started.elapsed() < BUILD_LIMIT, "Another environment preparation is still running. Try again after it finishes.")?;
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn successful(output: ProcessOutput, action: &str) -> Result<Vec<u8>> {
    require(
        !output.cancelled,
        "Verification environment preparation was cancelled.",
    )?;
    require(
        !output.timed_out,
        format!(
            "{action} exceeded its time limit. Check the container engine and network, then retry."
        ),
    )?;
    require(
        !output.exceeded,
        format!("{action} exceeded its output limit."),
    )?;
    require(
        output.code == Some(0),
        format!(
            "{action} failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .rev()
                .take(4000)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        ),
    )?;
    Ok(output.stdout)
}

fn facts(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::new("toolchain report is not UTF-8".into()))?;
    let mut facts = BTreeMap::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| Error::new("invalid toolchain report".into()))?;
        require(
            !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
            "invalid measured tool version",
        )?;
        require(
            facts.insert(name.to_string(), value.to_string()).is_none(),
            "duplicate measured tool version",
        )?;
    }
    let required = [
        "architecture",
        "rust_toolchain",
        "rust_version",
        "cargo_version",
        "node_version",
        "npm_version",
        "typescript_version",
        "python_version",
        "pytest_version",
        "gcc_version",
        "clang_version",
    ];
    require(
        facts.len() == required.len() && required.iter().all(|key| facts.contains_key(*key)),
        "the environment did not report all required tools",
    )?;
    require(
        facts["architecture"] == "x86_64",
        "automatic verification preparation currently requires a Linux x86_64 execution host",
    )?;
    Ok(facts)
}

impl ContainerBackend {
    /// Prepare a pinned verifier image once per shipped recipe and selected engine.
    /// The private cache must stay outside every model-writable workspace. This
    /// operation grants no task approval and never executes the user's project.
    pub fn prepare_environment(
        &self,
        cache: &Path,
        cancel: &Cancellation,
    ) -> Result<PreparedEnvironment> {
        self.prepare_environment_with(cache, cancel, |command, timeout, limit| {
            let args = command.get_args().collect::<Vec<_>>();
            let name = args
                .windows(2)
                .find(|pair| pair[0] == "--name")
                .map(|pair| pair[1].to_string_lossy().into_owned());
            process(command, timeout, limit, cancel, || {
                if let Some(name) = &name {
                    self.remove_preparation_container(name)?;
                }
                Ok(())
            })
        })
    }

    fn prepare_environment_with(
        &self,
        cache: &Path,
        cancel: &Cancellation,
        mut execute: impl FnMut(Command, Duration, usize) -> Result<ProcessOutput>,
    ) -> Result<PreparedEnvironment> {
        cancelled(cancel)?;
        require(
            cfg!(target_os = "linux") && cfg!(target_arch = "x86_64"),
            "automatic verification preparation currently requires a Linux x86_64 execution host",
        )?;
        require(
            matches!(self.version(), BACKEND_VERSION | PODMAN_BACKEND_VERSION),
            "Select Docker or rootless Podman before preparing verification.",
        )?;
        let recipe = recipe()?;
        let recipe_digest = recipe_digest(&recipe)?;
        let cache = private_directory(cache)?;
        let directory = private_directory(&cache.join(self.version()).join(recipe_digest.hex()))?;
        let _lock = lock(&directory, cancel)?;
        let receipt_path = directory.join("environment.json");
        let manifest_path = directory.join("toolchain.json");
        if receipt_path.exists() {
            let receipt: Receipt = wire::decode(&bounded_read(&receipt_path)?)?;
            require(
                receipt.recipe_digest == recipe_digest && receipt.backend_version == self.version(),
                "verification environment cache identity changed",
            )?;
            require(
                Digest::of(&bounded_read(&manifest_path)?) == receipt.toolchain_digest,
                "verification toolchain manifest changed",
            )?;
            let mut command = self.command();
            command.args([
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                receipt.image_digest.as_str(),
            ]);
            let output = execute(command, CONTROL_LIMIT, METADATA_LIMIT as usize)?;
            cancelled(cancel)?;
            if output.code == Some(0) {
                let inspected = successful(output, "Inspecting the prepared environment")?;
                require(
                    std::str::from_utf8(&inspected).ok().map(str::trim)
                        == Some(receipt.image_digest.as_str()),
                    "the cached verifier image identity changed",
                )?;
                return Ok(PreparedEnvironment {
                    image_digest: receipt.image_digest,
                    toolchain_manifest: manifest_path,
                    recipe_digest,
                    backend_version: self.version().into(),
                    facts: receipt.facts,
                });
            }
            require(
                !output.cancelled && !output.timed_out && !output.exceeded,
                "The container engine did not finish checking its prepared image.",
            )?;
        }
        let context = tempfile::Builder::new()
            .prefix("build-")
            .tempdir_in(&directory)?;
        let helper = context.path().join("helper");
        fs::create_dir(&helper)?;
        for (name, bytes) in SOURCE_FILES {
            crate::scope::validate_path(name)?;
            let destination = helper.join(name);
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or_else(|| Error::new("bundled helper path has no parent".into()))?,
            )?;
            fs::write(destination, bytes)?;
        }
        fs::write(context.path().join("Dockerfile"), &recipe)?;
        fs::write(
            context.path().join("clang-graph.cpp"),
            source("fixtures/native/clang-graph.cpp")?,
        )?;
        let image_id = context.path().join("image-id");
        let mut command = self.command();
        command
            .args(["build", "--force-rm", "--iidfile"])
            .arg(&image_id)
            .arg("--label")
            .arg(format!("{RECIPE_LABEL}={recipe_digest}"))
            .arg("--tag")
            .arg(format!("localhost/rovik-verifier:{}", recipe_digest.hex()));
        proxy_options(&mut command, "--build-arg");
        let certificate_file = if let Some(bytes) = certificate()? {
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            file.write_all(&bytes)?;
            let path = file
                .path()
                .to_str()
                .ok_or_else(|| Error::new("TLS bundle path is not UTF-8".into()))?;
            require(
                !path.contains(','),
                "TLS bundle path contains an unsupported separator",
            )?;
            command
                .arg("--secret")
                .arg(format!("id=proxy_ca,src={path}"));
            Some(file)
        } else {
            None
        };
        command.arg(context.path());
        successful(
            execute(command, BUILD_LIMIT, OUTPUT_LIMIT)?,
            "Preparing the verification environment",
        )?;
        drop(certificate_file);
        cancelled(cancel)?;
        let image_digest = Digest::parse(
            std::str::from_utf8(&bounded_read(&image_id)?)
                .map_err(|_| Error::new("prepared image identity is not UTF-8".into()))?
                .trim(),
        )?;
        let mut inspect = self.command();
        inspect.args([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            image_digest.as_str(),
        ]);
        let actual = successful(
            execute(inspect, CONTROL_LIMIT, METADATA_LIMIT as usize)?,
            "Inspecting the built verifier",
        )?;
        require(
            std::str::from_utf8(&actual).ok().map(str::trim) == Some(image_digest.as_str()),
            "built verifier image differs from its measured identity",
        )?;
        let mut probe = self.command();
        let probe_name = format!("rovik-measure-{}", uuid::Uuid::new_v4().simple());
        probe.args([
            "run",
            "--rm",
            "--name",
            &probe_name,
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user=65534:65534",
            "--pids-limit=64",
            "--memory=256m",
            "--cpus=1",
            "--tmpfs=/tmp:rw,noexec,nosuid,size=16777216",
            "--env=HOME=/tmp",
            image_digest.as_str(),
            "/bin/sh",
            "-ec",
            TOOLCHAIN_PROBE,
        ]);
        let facts = facts(&successful(
            execute(probe, CONTROL_LIMIT, METADATA_LIMIT as usize)?,
            "Measuring the verification toolchain",
        )?)?;
        cancelled(cancel)?;
        let manifest = wire::canonical(
            &serde_json::json!({"schemaVersion":1,"recipeDigest":recipe_digest,"imageDigest":image_digest,"backendVersion":self.version(),"tools":facts}),
        )?;
        let receipt = Receipt {
            recipe_digest: recipe_digest.clone(),
            image_digest: image_digest.clone(),
            toolchain_digest: Digest::of(&manifest),
            backend_version: self.version().into(),
            facts: facts.clone(),
        };
        publish(&manifest_path, &manifest)?;
        publish(&receipt_path, &wire::canonical(&receipt)?)?;
        Ok(PreparedEnvironment {
            image_digest,
            toolchain_manifest: manifest_path,
            recipe_digest,
            backend_version: self.version().into(),
            facts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_helper_has_a_standalone_locked_build_and_only_pinned_bases() {
        let recipe = recipe().unwrap();
        assert_eq!(recipe.matches(COMPILER_BASE).count(), 2);
        assert!(recipe.contains("apt-get install -y --no-install-recommends g++ cmake"));
        assert!(recipe.contains("cp /tmp/rovik-helper.lock Cargo.lock"));
        assert!(
            recipe.contains("cargo build --locked --offline --release --bin rovik-harness-tool")
        );
        assert!(recipe
            .lines()
            .filter(|line| line.starts_with("FROM "))
            .all(|line| line.contains("@sha256:")));
        let manifest = std::str::from_utf8(source("Cargo.toml").unwrap()).unwrap();
        assert!(!manifest.contains("workspace = true"));
        assert!(!manifest.contains("path ="));
        assert!(manifest.contains("[workspace]"));
        let lock = std::str::from_utf8(source("Cargo.lock").unwrap()).unwrap();
        assert!(lock.contains("name = \"rovik-harness\""));
        assert!(!lock.contains("name = \"rovik-core\""));
        for (path, _) in SOURCE_FILES {
            crate::scope::validate_path(path).unwrap();
            assert!(!path.ends_with("/rovik-harness-tool"));
        }
    }

    #[test]
    fn incomplete_and_duplicate_tool_reports_are_not_accepted_as_installed_versions() {
        assert!(facts(b"node_version=22.0.0\n").is_err());
        assert!(facts(b"node_version=22.0.0\nnode_version=22.0.0\n").is_err());
        assert!(facts(b"node_version=\n").is_err());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    mod linux {
        use super::*;
        const VERSIONS: &[u8] = b"architecture=x86_64\nrust_toolchain=1.96.0-x86_64-unknown-linux-gnu\nrust_version=1.96.0\ncargo_version=1.96.0\nnode_version=22.17.0\nnpm_version=10.9.2\ntypescript_version=7.0.2\npython_version=3.11.2\npytest_version=7.2.1\ngcc_version=12.2.0\nclang_version=Debian clang version 14.0.6\n";

        fn output(bytes: &[u8]) -> ProcessOutput {
            ProcessOutput {
                code: Some(0),
                stdout: bytes.to_vec(),
                stderr: vec![],
                timed_out: false,
                cancelled: false,
                exceeded: false,
            }
        }

        #[test]
        fn preparation_builds_only_bundled_sources_then_reuses_the_measured_immutable_image() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
            let image = Digest::of(b"built verifier");
            let mut calls = Vec::new();
            let mut execute =
                |command: Command, timeout: Duration, limit: usize| -> Result<ProcessOutput> {
                    let args = command
                        .get_args()
                        .map(|v| v.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    calls.push(args.clone());
                    assert_eq!(args[0], "--host=unix:///var/run/docker.sock");
                    assert!(timeout <= BUILD_LIMIT && limit <= OUTPUT_LIMIT);
                    match args[1].as_str() {
                        "build" => {
                            let context = Path::new(args.last().unwrap());
                            assert_eq!(
                                fs::read(context.join("helper/src/bin/rovik-harness-tool.rs"))?,
                                source("src/bin/rovik-harness-tool.rs")?
                            );
                            let id = args.iter().position(|arg| arg == "--iidfile").unwrap();
                            fs::write(&args[id + 1], image.as_str())?;
                            Ok(output(b"built\n"))
                        }
                        "image" => Ok(output(image.as_str().as_bytes())),
                        "run" => {
                            assert!(args.contains(&"--network=none".into()));
                            assert!(args.contains(&image.to_string()));
                            Ok(output(VERSIONS))
                        }
                        other => panic!("unexpected container operation {other}"),
                    }
                };
            let first = backend
                .prepare_environment_with(&cache, &Cancellation::default(), &mut execute)
                .unwrap();
            let second = backend
                .prepare_environment_with(&cache, &Cancellation::default(), &mut execute)
                .unwrap();
            assert_eq!(first.image_digest, image);
            assert_eq!(first.image_digest, second.image_digest);
            assert_eq!(first.facts["node_version"], "22.17.0");
            assert!(first.toolchain_manifest.is_file());
            assert_eq!(
                calls
                    .iter()
                    .map(|args| args[1].as_str())
                    .collect::<Vec<_>>(),
                ["build", "image", "run", "image"]
            );
            fs::write(&first.toolchain_manifest, b"changed").unwrap();
            assert!(backend
                .prepare_environment_with(&cache, &Cancellation::default(), |_, _, _| panic!(
                    "changed metadata must not execute"
                ))
                .is_err());
        }

        #[test]
        fn cancelled_preparation_never_starts_a_container_or_publishes_a_receipt() {
            let root = tempfile::tempdir().unwrap();
            let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
            let cancel = Cancellation::default();
            cancel.cancel();
            assert!(backend
                .prepare_environment_with(root.path(), &cancel, |_, _, _| panic!(
                    "already cancelled"
                ))
                .is_err());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }

        #[test]
        fn a_failed_or_incomplete_build_cannot_be_cached_as_a_prepared_environment() {
            let root = tempfile::tempdir().unwrap();
            let cache = root.path().join("cache");
            let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
            let result =
                backend.prepare_environment_with(&cache, &Cancellation::default(), |_, _, _| {
                    let mut failed = output(b"");
                    failed.code = Some(1);
                    failed.stderr = b"registry is unavailable".to_vec();
                    Ok(failed)
                });
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("registry is unavailable"));
            let digest = recipe_digest(&recipe().unwrap()).unwrap();
            assert!(!cache
                .join(BACKEND_VERSION)
                .join(digest.hex())
                .join("environment.json")
                .exists());
        }

        #[test]
        #[ignore = "builds the bundled verifier in a real container engine; ROVIK_REAL_PREPARATION=1"]
        fn native_first_use_builds_and_measures_the_shipped_verifier_without_a_source_checkout() {
            if std::env::var("ROVIK_REAL_PREPARATION").as_deref() != Ok("1") {
                return;
            }
            let executable = std::env::var_os("ROVIK_PREPARATION_ENGINE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/bin/docker"));
            let cache = std::env::var_os("ROVIK_PREPARATION_CACHE").map(PathBuf::from);
            let temporary = tempfile::tempdir().unwrap();
            let fallback = temporary.path().join("cache");
            let backend = ContainerBackend::local(&executable).unwrap();
            let prepared = backend
                .prepare_environment(
                    cache.as_deref().unwrap_or(&fallback),
                    &Cancellation::default(),
                )
                .unwrap();
            assert!(prepared.toolchain_manifest.is_file());
            assert!(!prepared.facts["rust_version"].is_empty());
            assert!(!prepared.facts["node_version"].is_empty());
            assert!(!prepared.facts["python_version"].is_empty());
        }
    }
}
