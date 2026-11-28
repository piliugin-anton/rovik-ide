//! Fixed dependency preparation commands inside the service-owned environment.
use super::{args, read, run_with_timeout};
use crate::{identity::Digest, require, runner::Cancellation, Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DependencyKind {
    Cargo,
    Npm,
    Python,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DependencyJob {
    pub(crate) kind: DependencyKind,
    pub(crate) root: String,
}

pub(crate) fn bundle_files(
    root: &Path,
    cancel: &Cancellation,
) -> Result<BTreeMap<String, (Digest, u64)>> {
    let metadata = fs::symlink_metadata(root)?;
    require(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Prepared dependency root must be a regular directory.",
    )?;
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    let mut bytes = 0u64;
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        require(
            !cancel.is_cancelled(),
            "Dependency preparation was cancelled.",
        )?;
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            entries += 1;
            require(
                entries <= 32768 && !metadata.file_type().is_symlink(),
                "Prepared dependencies contain a symlink or exceed the entry limit.",
            )?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            require(
                metadata.is_file(),
                "Prepared dependencies contain a non-regular file.",
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                require(
                    metadata.nlink() == 1,
                    "Prepared dependency files cannot be hard-linked.",
                )?;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| Error::new("Dependency path escaped its cache".into()))?
                .to_str()
                .ok_or_else(|| Error::new("Dependency path is not UTF-8".into()))?
                .replace('\\', "/");
            crate::scope::validate_path(&relative)?;
            require(
                !relative
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git")),
                "Git metadata cannot enter an offline dependency bundle.",
            )?;
            let (digest, length) =
                Digest::copy(fs::File::open(&path)?, std::io::sink(), 64 * 1024 * 1024)?;
            bytes = bytes
                .checked_add(length)
                .ok_or_else(|| Error::new("Dependency size overflow".into()))?;
            require(
                bytes <= 512 * 1024 * 1024,
                "Prepared dependencies exceed the 512 MiB bundle limit.",
            )?;
            files.insert(relative, (digest, length));
        }
    }
    Ok(files)
}

fn run(argv: &[String], cwd: &Path) -> Result<()> {
    let output = run_with_timeout(argv, cwd, &[], Duration::from_secs(30 * 60))?;
    use std::io::Write;
    // The controller keeps these diagnostics outside the model's workspace.
    std::io::stderr().write_all(&output.stdout)?;
    require(output.code == Some(0), "Locked dependency fetch failed.")
}

pub(super) fn prepare() -> Result<i32> {
    let jobs: Vec<DependencyJob> = read("/inputs/jobs.json")?;
    require(
        jobs.len() <= 128,
        "Dependency preparation has too many build roots.",
    )?;
    for job in &jobs {
        if !job.root.is_empty() {
            crate::scope::validate_path(&job.root)?;
        }
    }
    let dependencies = Path::new("/dependencies");
    for name in ["cargo", "npm", "wheels"] {
        fs::create_dir(dependencies.join(name))?;
    }
    fs::write("/build/npm-user.rc", b"")?;
    fs::write("/build/npm-global.rc", b"")?;
    for (index, job) in jobs.iter().enumerate() {
        let source = Path::new("/source").join(&job.root);
        let source_text = source
            .to_str()
            .ok_or_else(|| Error::new("Dependency root is not UTF-8".into()))?;
        match job.kind {
            DependencyKind::Cargo => run(
                &args(&[
                    "/usr/local/cargo/bin/cargo",
                    "fetch",
                    "--locked",
                    "--target=x86_64-unknown-linux-gnu",
                    "--manifest-path",
                    &format!("{source_text}/Cargo.toml"),
                ]),
                &source,
            )?,
            DependencyKind::Npm => {
                let working = Path::new("/build").join(format!("node-{index}"));
                fs::create_dir(&working)?;
                for name in ["package.json", "package-lock.json"] {
                    fs::copy(source.join(name), working.join(name))?;
                }
                run(
                    &args(&[
                        "/usr/local/bin/node",
                        "/usr/local/lib/node_modules/npm/bin/npm-cli.js",
                        "ci",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                        "--cache=/dependencies/npm",
                        "--userconfig=/build/npm-user.rc",
                        "--globalconfig=/build/npm-global.rc",
                    ]),
                    &working,
                )?;
                fs::remove_dir_all(working)?;
            }
            DependencyKind::Python => {
                let mut command = args(&[
                    "/usr/bin/python3",
                    "-I",
                    "-m",
                    "pip",
                    "download",
                    "--isolated",
                    "--require-hashes",
                    "--only-binary=:all:",
                    "--no-cache-dir",
                    "--disable-pip-version-check",
                    "--dest=/dependencies/wheels",
                    "--requirement",
                    &format!("{source_text}/requirements.lock"),
                ]);
                if Path::new("/inputs/trust.pem").is_file() {
                    // pip --isolated intentionally ignores PIP_CERT and other
                    // environment configuration; pass only the service's CA.
                    command.extend(args(&["--cert", "/inputs/trust.pem"]));
                }
                run(&command, Path::new("/build"))?;
            }
        }
    }
    for name in ["cargo/registry/src", "cargo/git/checkouts", "npm/_logs"] {
        let path = dependencies.join(name);
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
    }
    // Validate the whole tmpfs before writing any bytes onto the host. No
    // package-manager process survives its command or races this publication.
    let files = bundle_files(dependencies, &Cancellation::default())?;
    for (relative, expected) in files {
        let destination = Path::new("/output").join(&relative);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| Error::new("Dependency has no parent".into()))?,
        )?;
        let actual = Digest::copy(
            fs::File::open(dependencies.join(relative))?,
            fs::File::create(destination)?,
            expected.1,
        )?;
        require(
            actual == expected,
            "Dependency changed while publishing its prepared bundle.",
        )?;
    }
    Ok(0)
}
