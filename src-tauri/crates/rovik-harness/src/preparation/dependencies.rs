//! Fetch locked inputs without executing project lifecycle hooks or build scripts.

use super::*;
use crate::{ledger::Ledger, onboarding, profile::Language, snapshot::Snapshot};
use std::collections::BTreeSet;

use crate::tools::dependencies::{bundle_files, DependencyJob as Job, DependencyKind};

fn joined(root: &str, name: &str) -> String {
    if root.is_empty() {
        name.into()
    } else {
        format!("{root}/{name}")
    }
}

fn contents(ledger: &Ledger, captured: &Snapshot, path: &str) -> Result<Vec<u8>> {
    let entry = captured.files.get(path).ok_or_else(|| Error::new(format!(
        "Automatic verification needs {path}. Add a compatible lockfile to the project and retry preparation."
    )))?;
    require(
        entry.content.byte_count <= crate::wire::MAX_JSON_BYTES as u64,
        format!("Dependency manifest or lockfile {path} exceeds its limit."),
    )?;
    ledger.read(&entry.content.digest)
}

fn jobs(
    ledger: &Ledger,
    captured: &Snapshot,
    selected_manifests: &BTreeSet<String>,
) -> Result<Vec<Job>> {
    let inventory = onboarding::inspect(ledger, captured)?;
    require(
        !inventory.units.is_empty(),
        "No supported project manifest was found for automatic preparation.",
    )?;
    require(
        !selected_manifests.is_empty(),
        "Select at least one captured build root before preparing dependencies.",
    )?;
    for manifest in selected_manifests {
        require(
            inventory
                .units
                .iter()
                .any(|unit| &unit.manifest == manifest),
            format!(
                "Selected dependency manifest {manifest} is absent from the captured inventory."
            ),
        )?;
    }
    let mut jobs = Vec::new();
    let mut cargo_locks = BTreeSet::new();
    for unit in inventory
        .units
        .into_iter()
        .filter(|unit| selected_manifests.contains(&unit.manifest))
    {
        if unit.languages.contains(&Language::Rust) {
            // Members of a Cargo workspace share their ancestor's lockfile.
            let lock = Path::new(&unit.root)
                .ancestors()
                .map(|parent| joined(parent.to_str().unwrap_or(""), "Cargo.lock"))
                .find(|path| captured.files.contains_key(path));
            let lock = lock.ok_or_else(|| Error::new(format!(
                "Automatic verification needs a committed Cargo.lock for {} (or its workspace). Generate the locked dependency graph and retry preparation.", unit.manifest
            )))?;
            if cargo_locks.insert(lock) {
                jobs.push(Job {
                    kind: DependencyKind::Cargo,
                    root: unit.root,
                });
            }
        } else if unit.languages.contains(&Language::JavaScript)
            || unit.languages.contains(&Language::TypeScript)
        {
            let manifest = wire::decode(&contents(ledger, captured, &unit.manifest)?)?;
            let lock_path = joined(&unit.root, "package-lock.json");
            let lock = wire::decode(&contents(ledger, captured, &lock_path)?)?;
            crate::tools::runtime::validate_node_lock(&manifest, &lock).map_err(|error|
                Error::new(format!("Cannot prepare {}: {error}. Use an ESM package with an npm v3 integrity lock; linked workspace packages need an explicit adapter.", unit.manifest)))?;
            jobs.push(Job {
                kind: DependencyKind::Npm,
                root: unit.root,
            });
        } else if unit.languages.contains(&Language::Python) {
            let lock_path = joined(&unit.root, "requirements.lock");
            let bytes = contents(ledger, captured, &lock_path)?;
            let lock = std::str::from_utf8(&bytes)
                .map_err(|_| Error::new(format!("{lock_path} must be UTF-8")))?;
            crate::tools::runtime::validate_python_lock(lock).map_err(|error|
                Error::new(format!("Cannot prepare {lock_path}: {error}. Use exact versions and SHA-256 hashes for every wheel.")))?;
            jobs.push(Job {
                kind: DependencyKind::Python,
                root: unit.root,
            });
        }
    }
    Ok(jobs)
}

fn source_inputs(captured: &Snapshot) -> Snapshot {
    let mut source = captured.clone();
    // Cargo discovers target files during fetch. Copy captured files read-only,
    // but never let project configuration install a credential provider, wrapper,
    // replacement registry, or a different toolchain. npm gets only two manifests.
    source.files.retain(|path, _| {
        !path.split('/').any(|part| part == ".cargo")
            && !matches!(
                path.rsplit('/').next(),
                Some(".npmrc" | "rust-toolchain" | "rust-toolchain.toml")
            )
    });
    source.deletions.clear();
    source
}

fn bundle_digest(root: &Path, cancel: &Cancellation) -> Result<Digest> {
    Digest::of_json(&bundle_files(root, cancel)?)
}

fn mount(path: &Path, destination: &str, readonly: bool) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::new("Preparation cache path must be UTF-8".into()))?;
    require(
        !path.contains([',', '\n', '\r']),
        "Preparation cache path contains an unsupported mount separator.",
    )?;
    Ok(format!(
        "type=bind,source={path},destination={destination}{}",
        if readonly { ",readonly" } else { "" }
    ))
}

impl ContainerBackend {
    /// Fetch offline dependency inputs from the captured project using fixed
    /// recipes in the measured verifier, only for explicitly selected captured
    /// manifests. Unrelated examples and fixtures do not need matching locks.
    /// No project hook or build script runs.
    /// The returned directory is private service storage, never model writable.
    pub fn prepare_dependencies(
        &self,
        cache: &Path,
        ledger: &Ledger,
        captured: &Snapshot,
        selected_manifests: &BTreeSet<String>,
        environment: &PreparedEnvironment,
        cancel: &Cancellation,
    ) -> Result<PathBuf> {
        self.prepare_dependencies_with(
            cache,
            (ledger, captured, selected_manifests),
            environment,
            cancel,
            |command, name| {
                let result = process(command, BUILD_LIMIT, OUTPUT_LIMIT, cancel, || {
                    self.remove_preparation_container(name)
                });
                // Also clean up after start failures. A successful run used --rm.
                if !matches!(result.as_ref(), Ok(output) if output.code == Some(0)) {
                    self.remove_preparation_container(name)?;
                }
                result
            },
        )
    }

    pub(super) fn remove_preparation_container(&self, name: &str) -> Result<()> {
        let mut command = self.command();
        command.args(["rm", "--force", name]);
        let result = process(
            command,
            CONTROL_LIMIT,
            METADATA_LIMIT as usize,
            &Cancellation::default(),
            || Ok(()),
        )?;
        // Docker/Podman return an error for a container already removed by --rm.
        if result.code != Some(0) {
            let message = String::from_utf8_lossy(&result.stderr).to_ascii_lowercase();
            require(
                !result.timed_out
                    && !result.exceeded
                    && (message.contains("no such container")
                        || message.contains("no container with")),
                "Could not stop the dependency preparation container.",
            )?;
        }
        Ok(())
    }

    fn prepare_dependencies_with(
        &self,
        cache: &Path,
        inputs: (&Ledger, &Snapshot, &BTreeSet<String>),
        environment: &PreparedEnvironment,
        cancel: &Cancellation,
        mut execute: impl FnMut(Command, &str) -> Result<ProcessOutput>,
    ) -> Result<PathBuf> {
        let (ledger, captured, selected_manifests) = inputs;
        cancelled(cancel)?;
        require(
            environment.backend_version == self.version(),
            "Prepared dependency engine differs from its trusted environment.",
        )?;
        let jobs = jobs(ledger, captured, selected_manifests)?;
        let toolchain = environment.facts.get("rust_toolchain").ok_or_else(|| {
            Error::new(
                "The prepared environment has no measured Rust toolchain. Prepare it again.".into(),
            )
        })?;
        let source = source_inputs(captured);
        // Cargo path dependencies can introduce new manifests/targets. The exact
        // source digest makes reuse conservative until each adapter has a complete
        // dependency-input classifier.
        let key = Digest::of_json(&(
            source.digest()?,
            &environment.image_digest,
            &environment.recipe_digest,
            self.version(),
            &jobs,
        ))?;
        let cache = private_directory(cache)?;
        let directory = private_directory(&cache.join("dependencies").join(key.hex()))?;
        let _lock = lock(&directory, cancel)?;
        let destination = directory.join("bundle");
        let receipt_path = directory.join("bundle.json");
        if receipt_path.exists() {
            let expected: Digest = wire::decode(&bounded_read(&receipt_path)?)?;
            require(expected == bundle_digest(&destination, cancel)?, "Prepared dependency cache changed. Remove the damaged preparation cache and retry.")?;
            return Ok(destination);
        }
        let temporary = tempfile::Builder::new()
            .prefix("fetch-")
            .tempdir_in(&directory)?;
        let input = temporary.path().join("inputs");
        let output = private_directory(&temporary.path().join("output"))?;
        let source_path = temporary.path().join("source");
        source.materialize(ledger, &source_path)?;
        fs::create_dir(&input)?;
        fs::write(input.join("jobs.json"), wire::canonical(&jobs)?)?;
        let certificate = certificate()?;
        if let Some(bytes) = &certificate {
            fs::write(input.join("trust.pem"), bytes)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&input, fs::Permissions::from_mode(0o755))?;
            for entry in fs::read_dir(&input)? {
                fs::set_permissions(entry?.path(), fs::Permissions::from_mode(0o444))?;
            }
        }
        if !jobs.is_empty() {
            let name = format!("rovik-prepare-{}", uuid::Uuid::new_v4().simple());
            let mut command = self.command();
            command.args([
                "run",
                "--rm",
                "--name",
                &name,
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--pids-limit=128",
                "--memory=2g",
                "--memory-swap=2g",
                "--cpus=2",
                "--tmpfs=/tmp:rw,noexec,nosuid,size=67108864,mode=1777",
                "--tmpfs=/build:rw,noexec,nosuid,size=536870912,mode=1777",
                "--tmpfs=/dependencies:rw,noexec,nosuid,size=805306368,mode=1777",
                "--env=HOME=/tmp",
                "--env=CARGO_HOME=/dependencies/cargo",
                "--env=RUSTUP_AUTO_INSTALL=0",
                "--env=CARGO_NET_GIT_FETCH_WITH_CLI=false",
                "--env=CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse",
                "--env=CARGO_HTTP_MULTIPLEXING=false",
            ]);
            command.arg(format!("--env=RUSTUP_TOOLCHAIN={toolchain}"));
            proxy_options(&mut command, "--env");
            if certificate.is_some() {
                for name in [
                    "SSL_CERT_FILE",
                    "CARGO_HTTP_CAINFO",
                    "NODE_EXTRA_CA_CERTS",
                    "PIP_CERT",
                    "REQUESTS_CA_BUNDLE",
                ] {
                    command.arg(format!("--env={name}=/inputs/trust.pem"));
                }
            }
            self.preparation_options(&mut command, &output)?;
            for (path, target, readonly) in [
                (&source_path, "/source", true),
                (&input, "/inputs", true),
                (&output, "/output", false),
            ] {
                command.arg("--mount").arg(mount(path, target, readonly)?);
            }
            command.args([
                environment.image_digest.as_str(),
                crate::tools::EXECUTABLE,
                "prepare-dependencies",
            ]);
            let result = execute(command, &name)?;
            if result.code != Some(0) && !result.cancelled && !result.timed_out && !result.exceeded
            {
                // Package managers can quote arbitrary source lines (including
                // denied-read files). Keep diagnostics only in service storage;
                // the preparation model receives a fixed, non-source message.
                publish(&directory.join("fetch-diagnostics.log"), &result.stderr)?;
                return Err(Error::new("Locked dependencies could not be prepared. Ensure the captured manifests match their lockfiles, required registries are reachable, and dependencies provide supported packages. Project setup scripts stay disabled; detailed diagnostics are in private preparation storage.".into()));
            }
            successful(
                result,
                "Fetching locked offline dependencies (project scripts are disabled)",
            )?;
        }
        cancelled(cancel)?;
        let digest = bundle_digest(&output, cancel)?;
        if destination.exists() {
            fs::remove_dir_all(&destination)?;
        }
        fs::rename(output, &destination)?;
        publish(&receipt_path, &wire::canonical(&digest)?)?;
        Ok(destination)
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::snapshot::FileEntry;

    fn captured(ledger: &Ledger, files: &[(&str, &[u8])]) -> Snapshot {
        Snapshot {
            schema_version: 1,
            base_revision: "preparation-test".into(),
            files: files
                .iter()
                .map(|(name, bytes)| {
                    (
                        name.to_string(),
                        FileEntry {
                            mode: 0o100644,
                            content: ledger.store_source(*bytes, 1).unwrap(),
                        },
                    )
                })
                .collect(),
            deletions: vec![],
            dependency_assets: vec![],
        }
    }

    fn selection(ledger: &Ledger, snapshot: &Snapshot) -> BTreeSet<String> {
        onboarding::inspect(ledger, snapshot)
            .unwrap()
            .units
            .into_iter()
            .map(|unit| unit.manifest)
            .collect()
    }

    fn environment() -> PreparedEnvironment {
        PreparedEnvironment {
            image_digest: Digest::of(b"measured image"),
            toolchain_manifest: "/private/toolchain.json".into(),
            recipe_digest: Digest::of(b"bundled recipe"),
            backend_version: BACKEND_VERSION.into(),
            facts: BTreeMap::from([(
                "rust_toolchain".into(),
                "1.96.0-x86_64-unknown-linux-gnu".into(),
            )]),
        }
    }

    fn success() -> ProcessOutput {
        ProcessOutput {
            code: Some(0),
            stdout: vec![],
            stderr: vec![],
            timed_out: false,
            cancelled: false,
            exceeded: false,
        }
    }

    fn bind(args: &[String], target: &str) -> PathBuf {
        let argument = args
            .iter()
            .find(|arg| arg.contains(&format!(",destination={target}")))
            .unwrap();
        PathBuf::from(
            argument
                .strip_prefix("type=bind,source=")
                .unwrap()
                .split(",destination=")
                .next()
                .unwrap(),
        )
    }

    #[test]
    fn missing_and_unlocked_dependencies_block_before_starting_a_container() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let cases: Vec<Vec<(&str, &[u8])>> = vec![
            vec![("Cargo.toml", b"[package]\nname='test'\nversion='0.1.0'\n")],
            vec![("package.json", br#"{"type":"module"}"#)],
            vec![("pyproject.toml", b"[project]\nname='test'\n")],
            vec![("requirements.lock", b"unlocked-package>=1")],
            vec![
                ("package.json", br#"{"type":"module"}"#),
                (
                    "package-lock.json",
                    br#"{"lockfileVersion":3,"packages":{"":{},"node_modules/unlocked":{}}}"#,
                ),
            ],
        ];
        let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
        for files in cases {
            let snapshot = captured(&ledger, &files);
            let result = backend.prepare_dependencies_with(
                &directory.path().join("cache"),
                (&ledger, &snapshot, &selection(&ledger, &snapshot)),
                &environment(),
                &Cancellation::default(),
                |_, _| panic!("must validate first"),
            );
            assert!(result.is_err());
        }
        assert!(!directory.path().join("cache").exists());
    }

    #[test]
    fn unrelated_unlocked_examples_do_not_block_selected_roots_and_unknown_selections_fail() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let snapshot = captured(
            &ledger,
            &[
                (
                    "Cargo.toml",
                    b"[package]\nname='selected'\nversion='0.1.0'\n",
                ),
                ("Cargo.lock", b"version = 4\n"),
                ("src/lib.rs", b""),
                (
                    "examples/unconfigured/package.json",
                    br#"{"name":"not-part-of-this-plan"}"#,
                ),
            ],
        );
        let selected = BTreeSet::from(["Cargo.toml".into()]);
        assert_eq!(
            jobs(&ledger, &snapshot, &selected).unwrap(),
            vec![Job {
                kind: DependencyKind::Cargo,
                root: String::new()
            }]
        );
        let selected = BTreeSet::from(["examples/unconfigured/package.json".into()]);
        assert!(jobs(&ledger, &snapshot, &selected)
            .unwrap_err()
            .to_string()
            .contains("package-lock.json"));
        assert!(jobs(
            &ledger,
            &snapshot,
            &BTreeSet::from(["missing/Cargo.toml".into()])
        )
        .unwrap_err()
        .to_string()
        .contains("absent from the captured inventory"));
        assert!(jobs(&ledger, &snapshot, &BTreeSet::new()).is_err());
    }

    #[test]
    fn trusted_fetch_uses_captured_roots_bounded_tmpfs_and_no_project_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let snapshot = captured(
            &ledger,
            &[
                ("Cargo.toml", b"[workspace]\nmembers=['backend']\n"),
                ("Cargo.lock", b"version = 4\n"),
                (
                    "backend/Cargo.toml",
                    b"[package]\nname='test'\nversion='0.1.0'\n",
                ),
                ("backend/src/lib.rs", b""),
                (
                    ".cargo/config.toml",
                    b"[registry]\nglobal-credential-providers=['malicious']\n",
                ),
                (
                    "backend/.cargo/config",
                    b"[build]\nrustc-wrapper='malicious'\n",
                ),
                ("rust-toolchain.toml", b"[toolchain]\nchannel='untrusted'\n"),
                (
                    "frontend/package.json",
                    br#"{"type":"module","scripts":{"preinstall":"touch /source/executed"}}"#,
                ),
                (
                    "frontend/package-lock.json",
                    br#"{"lockfileVersion":3,"packages":{"":{}}}"#,
                ),
                ("frontend/.npmrc", b"script-shell=malicious\n"),
                ("python/requirements.lock", b""),
            ],
        );
        let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
        let cache = directory.path().join("cache");
        let mut calls = 0;
        let result = backend
            .prepare_dependencies_with(
                &cache,
                (&ledger, &snapshot, &selection(&ledger, &snapshot)),
                &environment(),
                &Cancellation::default(),
                |command, name| {
                    calls += 1;
                    let args = command
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    assert_eq!(args[0], "--host=unix:///var/run/docker.sock");
                    assert!(args.contains(&name.into()));
                    assert!(args.contains(&environment().image_digest.to_string()));
                    assert_eq!(
                        args.last().map(String::as_str),
                        Some("prepare-dependencies")
                    );
                    assert!(args.contains(&"--read-only".into()));
                    assert!(args.contains(&"--memory=2g".into()));
                    assert!(args
                        .iter()
                        .any(|arg| arg.starts_with("--tmpfs=/dependencies:")));
                    assert!(args.contains(&"--env=RUSTUP_AUTO_INSTALL=0".into()));
                    let source = bind(&args, "/source");
                    assert!(!source.join(".cargo").exists());
                    assert!(!source.join("backend/.cargo").exists());
                    assert!(!source.join("rust-toolchain.toml").exists());
                    assert!(!source.join("frontend/.npmrc").exists());
                    assert!(source.join("backend/src/lib.rs").is_file());
                    let input = bind(&args, "/inputs");
                    let jobs: serde_json::Value =
                        wire::decode(&fs::read(input.join("jobs.json"))?)?;
                    assert_eq!(
                        jobs,
                        serde_json::json!([
                            {"kind":"cargo", "root":""},
                            {"kind":"npm", "root":"frontend"},
                            {"kind":"python", "root":"python"},
                        ])
                    );
                    let output = bind(&args, "/output");
                    fs::create_dir(output.join("wheels"))?;
                    fs::write(
                        output.join("wheels/locked.whl"),
                        b"downloaded hash-locked dependency",
                    )?;
                    Ok(success())
                },
            )
            .unwrap();
        assert_eq!(calls, 1);
        assert!(result.join("wheels/locked.whl").is_file());
        assert_eq!(
            backend
                .prepare_dependencies_with(
                    &cache,
                    (&ledger, &snapshot, &selection(&ledger, &snapshot)),
                    &environment(),
                    &Cancellation::default(),
                    |_, _| panic!("cache hit must not fetch")
                )
                .unwrap(),
            result
        );
        fs::write(result.join("wheels/locked.whl"), b"tampered").unwrap();
        assert!(backend
            .prepare_dependencies_with(
                &cache,
                (&ledger, &snapshot, &selection(&ledger, &snapshot)),
                &environment(),
                &Cancellation::default(),
                |_, _| panic!("tampered cache must fail")
            )
            .is_err());
    }

    #[test]
    fn dependency_capture_rejects_links_and_never_publishes_failed_fetches() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let snapshot = captured(&ledger, &[("requirements.lock", b"")]);
        let backend = ContainerBackend::local(Path::new("/bin/true")).unwrap();
        let result = backend.prepare_dependencies_with(
            &directory.path().join("cache"),
            (&ledger, &snapshot, &selection(&ledger, &snapshot)),
            &environment(),
            &Cancellation::default(),
            |command, _| {
                let args = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                std::os::unix::fs::symlink("/etc/passwd", bind(&args, "/output").join("escape"))?;
                Ok(success())
            },
        );
        assert!(result.unwrap_err().to_string().contains("symlink"));
        let result = backend.prepare_dependencies_with(
            &directory.path().join("cache"),
            (&ledger, &snapshot, &selection(&ledger, &snapshot)),
            &environment(),
            &Cancellation::default(),
            |_, _| {
                let mut output = success();
                output.code = Some(1);
                output.stderr = b"dependency integrity mismatch".to_vec();
                Ok(output)
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Locked dependencies could not be prepared"));
        for entry in fs::read_dir(directory.path().join("cache/dependencies")).unwrap() {
            assert!(!entry.unwrap().path().join("bundle.json").exists());
        }
    }

    #[test]
    #[ignore = "fetches public locked packages in a real container; ROVIK_REAL_PREPARATION=1"]
    fn native_preparation_fetches_locked_rust_and_npm_inputs_without_running_hooks() {
        if std::env::var("ROVIK_REAL_PREPARATION").as_deref() != Ok("1") {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let cache = std::env::var_os("ROVIK_PREPARATION_CACHE").map(PathBuf::from);
        let fallback = directory.path().join("cache");
        let cache = cache.as_deref().unwrap_or(&fallback);
        let executable = std::env::var_os("ROVIK_PREPARATION_ENGINE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/docker"));
        let backend = ContainerBackend::local(&executable).unwrap();
        let cancel = Cancellation::default();
        let environment = backend.prepare_environment(cache, &cancel).unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let snapshot = captured(&ledger, &[
            ("Cargo.toml", b"[package]\nname='prepared-rust-fixture'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nitoa='=1.0.18'\n"),
            ("Cargo.lock", b"version = 4\n[[package]]\nname = \"prepared-rust-fixture\"\nversion = \"0.1.0\"\ndependencies = [\"itoa\"]\n[[package]]\nname = \"itoa\"\nversion = \"1.0.18\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682\"\n"),
            ("src/lib.rs", b"pub fn value() -> String { itoa::Buffer::new().format(7).to_string() }\n"),
            ("build.rs", b"fn main() { panic!(\"preparation must not execute build scripts\") }\n"),
            ("web/package.json", br#"{"name":"prepared-node-fixture","version":"1.0.0","type":"module","dependencies":{"picocolors":"1.1.1"},"scripts":{"preinstall":"touch /output/lifecycle-executed"}}"#),
            ("web/package-lock.json", br#"{"name":"prepared-node-fixture","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"prepared-node-fixture","version":"1.0.0","dependencies":{"picocolors":"1.1.1"},"hasInstallScript":true},"node_modules/picocolors":{"version":"1.1.1","resolved":"https://registry.npmjs.org/picocolors/-/picocolors-1.1.1.tgz","integrity":"sha512-xceH2snhtb5M9liqDsmEw56le376mTZkEX/jEb/RxNFyegNul7eNslCXP9FDj/Lcu0X8KEyMceP2ntpaHrDEVA=="}}}"#),
            ("python/requirements.lock", b""),
        ]);
        let bundle = backend
            .prepare_dependencies(
                cache,
                &ledger,
                &snapshot,
                &selection(&ledger, &snapshot),
                &environment,
                &cancel,
            )
            .unwrap();
        assert!(!bundle.join("lifecycle-executed").exists());
        assert!(bundle.join("cargo/registry/cache").is_dir());
        assert!(bundle.join("npm/_cacache/content-v2").is_dir());
        assert_eq!(
            backend
                .prepare_dependencies(
                    cache,
                    &ledger,
                    &snapshot,
                    &selection(&ledger, &snapshot),
                    &environment,
                    &cancel
                )
                .unwrap(),
            bundle
        );
    }
}
