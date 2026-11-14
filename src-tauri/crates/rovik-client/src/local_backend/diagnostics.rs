//! Connection evidence survives a window or backend exit without recording RPC contents.

use super::Ready;
use std::{
    io::{self, Write},
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn record(directory: &Path, message: &str) {
    let line = format!(
        "{} pid={} {message}\n",
        rovik_core::agent_run::log::iso_now(),
        std::process::id()
    );
    log::info!(target: "rovik::backend", "{}", line.trim_end());
    // Test-only stream pairs have no discovery directory. Never write to cwd.
    if directory.as_os_str().is_empty() {
        return;
    }
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(directory.join("connection.log"))?
            .write_all(line.as_bytes())
    })();
    if let Err(error) = result {
        log::warn!("Could not write backend connection diagnostics: {error}");
    }
}

pub(super) struct Exchange<'a> {
    directory: &'a Path,
    ready: &'a Ready,
    operation: &'static str,
    verbose: bool,
    started: Instant,
    stage: &'static str,
    timeout: Option<Duration>,
    finished: bool,
}

impl<'a> Exchange<'a> {
    pub(super) fn begin(
        directory: &'a Path,
        ready: &'a Ready,
        operation: &'static str,
        verbose: bool,
    ) -> Self {
        let trace = Self {
            directory,
            ready,
            operation,
            verbose,
            started: Instant::now(),
            stage: "wait_for_connection",
            timeout: None,
            finished: false,
        };
        if verbose {
            trace.emit("started", None);
        }
        trace
    }

    pub(super) fn stage(&mut self, stage: &'static str) {
        self.stage = stage;
        self.timeout = None;
    }

    pub(super) fn wait(&mut self, stage: &'static str, timeout: Duration) {
        self.stage = stage;
        self.timeout = Some(timeout);
    }

    pub(super) fn finish<T>(&mut self, result: &io::Result<T>) {
        self.finished = true;
        if result.is_err() || self.verbose {
            self.emit(
                if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
                result.as_ref().err(),
            );
        }
    }

    fn emit(&self, outcome: &str, error: Option<&io::Error>) {
        record(self.directory, &format!(
            "rpc boot={:?} session={:?} operation={} stage={} outcome={outcome} elapsed_ms={} timeout_ms={:?} io_kind={:?} os_code={:?}",
            self.ready.boot, self.ready.session, self.operation, self.stage,
            self.started.elapsed().as_millis(), self.timeout.map(|value| value.as_millis()), error.map(io::Error::kind), error.and_then(io::Error::raw_os_error),
        ));
    }
}

impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("cancelled_completion_unknown", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failures_persist_without_arbitrary_error_contents() {
        let root = tempfile::tempdir().unwrap();
        let ready = Ready {
            protocol: super::super::PROTOCOL,
            boot: "boot".into(),
            session: "session".into(),
            version: "test".into(),
        };
        let mut trace = Exchange::begin(root.path(), &ready, "agent_start", true);
        trace.wait("await_reply", Duration::from_secs(60));
        trace.finish(&Err::<(), _>(io::Error::new(
            io::ErrorKind::TimedOut,
            "api-key=secret private prompt",
        )));
        drop(trace);
        let output = std::fs::read_to_string(root.path().join("connection.log")).unwrap();
        assert!(output.contains("operation=agent_start stage=await_reply outcome=failed"));
        assert!(output.contains("timeout_ms=Some(60000)"));
        assert!(output.contains("io_kind=Some(TimedOut)"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("private prompt"));
        assert!(!output.contains("cancelled_completion_unknown"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join("connection.log"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
