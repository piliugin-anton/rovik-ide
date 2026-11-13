//! Sandbox-side native adapters. These binaries cannot grant controller authority.
mod consumer;
mod evaluation;
mod imports;
mod interop;
mod native;
mod native_checks;
mod runtime;

use crate::{
    process::{process, ProcessOutput},
    require,
    runner::Cancellation,
    wire, Error, Result,
};
use std::{path::Path, process::Command, time::Duration};

/// Executable supplied by the immutable toolchain image, never by the candidate.
pub const EXECUTABLE: &str = "/usr/local/bin/rovik-harness-tool";
const LIMIT: usize = 8 * 1024 * 1024;

fn run(argv: &[String], cwd: &Path, environment: &[(&str, &str)]) -> Result<ProcessOutput> {
    require(!argv.is_empty(), "missing native command")?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .envs(environment.iter().copied());
    let output = process(
        command,
        Duration::from_secs(120),
        LIMIT,
        &Cancellation::default(),
        || Ok(()),
    )?;
    require(
        !output.timed_out && !output.cancelled && !output.exceeded,
        "native process exceeded its resource bounds",
    )?;
    use std::io::Write;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(output)
}
fn checked(argv: &[String], cwd: &Path) -> Result<String> {
    checked_as(argv, cwd, crate::FailureCategory::Infrastructure)
}
fn checked_source(argv: &[String], cwd: &Path) -> Result<String> {
    checked_as(argv, cwd, crate::FailureCategory::Implementation)
}
fn checked_as(argv: &[String], cwd: &Path, category: crate::FailureCategory) -> Result<String> {
    let output = run(argv, cwd, &[])?;
    if output.code != Some(0) {
        use std::io::Write;
        // Several compilers report diagnostics on stdout. Keep protocol stdout
        // clean while preserving those diagnostics in the runner's bounded log.
        std::io::stderr().write_all(&output.stdout)?;
    }
    if output.code != Some(0) {
        return Err(Error::classified(
            if output.code.is_none() {
                crate::FailureCategory::Infrastructure
            } else {
                category
            },
            format!("native command failed: {}", argv.join(" ")),
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| Error::new(e.to_string()))
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}
fn read<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    wire::decode(&bytes)
}
fn write_json(path: impl AsRef<Path>, value: &impl serde::Serialize) -> Result<()> {
    std::fs::write(path, wire::canonical(value)?)?;
    Ok(())
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    require(
        !std::fs::symlink_metadata(source)?.file_type().is_symlink(),
        "dependency/source symlink is unsupported",
    )?;
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            return Err(Error::new("unsupported dependency/source file mode".into()));
        }
    }
    Ok(())
}
/// Run one registered adapter invocation inside the verification sandbox.
pub fn execute(arguments: &[String]) -> Result<i32> {
    match arguments.first().map(String::as_str) {
        Some("pack") if arguments.len() == 2 => native::protocol(&arguments[1]),
        Some("quota") if arguments.len() == 1 => consumer::quota(),
        Some("interop") if arguments.len() == 1 => interop::check(),
        Some("evaluate") if arguments.len() == 1 => evaluation::check(),
        _ => Err(Error::new(
            "usage: rovik-harness-tool pack <request-id|-> | quota | interop | evaluate".into(),
        )),
    }
}
