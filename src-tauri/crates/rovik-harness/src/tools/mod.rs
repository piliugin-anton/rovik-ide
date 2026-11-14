//! Sandbox-side native adapters. These binaries cannot grant controller authority.
mod consumer;
pub(crate) mod dependencies;
mod evaluation;
mod imports;
mod interop;
mod native;
mod native_checks;
pub(crate) mod runtime;

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
const FAILURE_SNIPPET_BYTES: usize = 2048;

fn diagnostic_snippet(bytes: &[u8], maximum: usize) -> String {
    if bytes.is_empty() {
        return "<empty>".into();
    }
    // Bound both the captured bytes inspected and the rendered UTF-8 text:
    // replacement characters can otherwise triple malformed binary output.
    let inspected = bytes.len().min(maximum);
    let text = String::from_utf8_lossy(&bytes[..inspected]);
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut snippet = text[..end].to_string();
    if inspected < bytes.len() || end < text.len() {
        snippet.push_str(" [truncated]");
    }
    snippet
}

fn require_complete_process(executable: &str, output: &ProcessOutput) -> Result<()> {
    let reasons = [
        (output.timed_out, "timeout"),
        (output.cancelled, "cancellation"),
        (output.exceeded, "output limit"),
    ]
    .into_iter()
    .filter_map(|(present, reason)| present.then_some(reason))
    .collect::<Vec<_>>();
    if reasons.is_empty() {
        return Ok(());
    }
    // Arguments can contain credentials or source text. The executable and
    // bounded captured output are sufficient to identify a flooding child.
    Err(Error::new(format!(
        "native process exceeded its resource bounds: {}; executable={}; stdout ({} captured bytes): {}; stderr ({} captured bytes): {}",
        reasons.join(", "),
        diagnostic_snippet(executable.as_bytes(), 256),
        output.stdout.len(),
        diagnostic_snippet(&output.stdout, FAILURE_SNIPPET_BYTES),
        output.stderr.len(),
        diagnostic_snippet(&output.stderr, FAILURE_SNIPPET_BYTES),
    )))
}

fn run(argv: &[String], cwd: &Path, environment: &[(&str, &str)]) -> Result<ProcessOutput> {
    run_with_timeout(argv, cwd, environment, Duration::from_secs(120))
}
fn run_with_timeout(
    argv: &[String],
    cwd: &Path,
    environment: &[(&str, &str)],
    timeout: Duration,
) -> Result<ProcessOutput> {
    require(!argv.is_empty(), "missing native command")?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .envs(environment.iter().copied());
    let output = process(command, timeout, LIMIT, &Cancellation::default(), || Ok(()))?;
    require_complete_process(&argv[0], &output)?;
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
        Some("prepare-dependencies") if arguments.len() == 1 => dependencies::prepare(),
        _ => Err(Error::new(
            "usage: rovik-harness-tool pack <request-id|-> | quota | interop | evaluate | prepare-dependencies".into(),
        )),
    }
}

#[cfg(test)]
mod resource_diagnostic_tests {
    use super::*;

    fn output() -> ProcessOutput {
        ProcessOutput {
            code: Some(42),
            stdout: vec![],
            stderr: vec![],
            timed_out: false,
            cancelled: false,
            exceeded: false,
        }
    }

    #[test]
    fn resource_refusals_identify_only_the_executable_and_actual_failure_reasons() {
        let argv = ["/usr/bin/compiler", "--token=private-argument"];
        for (timed_out, cancelled, exceeded, reason) in [
            (true, false, false, "timeout"),
            (false, true, false, "cancellation"),
            (false, false, true, "output limit"),
            (true, true, true, "timeout, cancellation, output limit"),
        ] {
            let mut observed = output();
            observed.timed_out = timed_out;
            observed.cancelled = cancelled;
            observed.exceeded = exceeded;
            let error = require_complete_process(argv[0], &observed).unwrap_err();
            assert!(error.message.starts_with(&format!(
                "native process exceeded its resource bounds: {reason};"
            )));
            assert!(error.message.contains("executable=/usr/bin/compiler;"));
            assert!(!error.message.contains(argv[1]));
            assert!(error.message.contains("stdout (0 captured bytes): <empty>"));
            assert!(error.message.contains("stderr (0 captured bytes): <empty>"));
            assert_eq!(error.category, crate::FailureCategory::InvalidInput);
        }
    }

    #[test]
    fn resource_diagnostics_bound_rendered_binary_output_and_omit_the_uncaptured_tail() {
        let mut observed = output();
        observed.exceeded = true;
        observed.stdout = vec![b'x'; LIMIT];
        observed.stdout.extend_from_slice(b"hidden-output-tail");
        observed.stderr = vec![0xff; 4096];
        let executable = format!("/tool/{}hidden-executable-tail", "a".repeat(512));
        let error = require_complete_process(&executable, &observed).unwrap_err();
        assert!(error.message.contains(" [truncated]"));
        assert!(!error.message.contains("hidden-output-tail"));
        assert!(!error.message.contains("hidden-executable-tail"));
        assert!(error.message.contains('\u{fffd}'));
        assert!(error.message.len() < 4600, "{}", error.message.len());
        let multibyte = "é".repeat(1025);
        let snippet = diagnostic_snippet(multibyte.as_bytes(), FAILURE_SNIPPET_BYTES);
        assert_eq!(snippet, format!("{} [truncated]", "é".repeat(1024)));
    }

    #[test]
    fn completed_nonzero_processes_keep_their_existing_output_and_failure_handling() {
        let mut observed = output();
        observed.stdout = b"native result".to_vec();
        observed.stderr = b"native diagnostic".to_vec();
        require_complete_process("/usr/bin/compiler", &observed).unwrap();
        assert_eq!(observed.code, Some(42));
        assert_eq!(observed.stdout, b"native result");
        assert_eq!(observed.stderr, b"native diagnostic");
    }
}
