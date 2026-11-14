//! Content-free progress retained by the execution host's normal logger.

use rovik_harness::{FailureCategory, Result};
use std::time::Instant;

pub(super) struct PreparationLog {
    trace: uuid::Uuid,
    operation: &'static str,
    stage: &'static str,
    started: Instant,
    finished: bool,
}

impl PreparationLog {
    pub(super) fn start(operation: &'static str) -> Self {
        let trace = Self {
            trace: uuid::Uuid::new_v4(),
            operation,
            stage: "begin",
            started: Instant::now(),
            finished: false,
        };
        trace.emit("started", None);
        trace
    }

    pub(super) fn stage(&mut self, stage: &'static str) {
        self.stage = stage;
        self.emit("progress", None);
    }

    pub(super) fn bind(&self, id: &str) {
        // Only service-issued identities read from a validated record belong here.
        log::info!(target: "rovik::preparation", "{} pid={} preparation trace={} id={id:?}",
            crate::agent_run::log::iso_now(), std::process::id(), self.trace);
    }

    pub(super) fn finish<T>(&mut self, result: &Result<T>) {
        self.finished = true;
        self.emit(
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
            result.as_ref().err().map(|error| error.category),
        );
    }

    fn line(&self, outcome: &str, category: Option<FailureCategory>) -> String {
        format!(
            "{} pid={} preparation trace={} operation={} stage={} outcome={outcome} elapsed_ms={} category={category:?}",
            crate::agent_run::log::iso_now(), std::process::id(), self.trace,
            self.operation, self.stage, self.started.elapsed().as_millis(),
        )
    }

    fn emit(&self, outcome: &str, category: Option<FailureCategory>) {
        let line = self.line(outcome, category);
        if matches!(outcome, "failed" | "interrupted" | "panicked") {
            log::warn!(target: "rovik::preparation", "{line}");
        } else {
            log::info!(target: "rovik::preparation", "{line}");
        }
    }
}

impl Drop for PreparationLog {
    fn drop(&mut self) {
        if !self.finished {
            self.emit(
                if std::thread::panicking() {
                    "panicked"
                } else {
                    "interrupted"
                },
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_diagnostics_identify_the_stage_without_native_error_content() {
        let mut trace = PreparationLog::start("submit");
        trace.stage("dependencies");
        let failure: Result<()> = Err(rovik_harness::Error::classified(
            FailureCategory::Infrastructure,
            "provider-key=secret; private source and command output",
        ));
        trace.finish(&failure);
        let line = trace.line("failed", failure.err().map(|error| error.category));
        assert!(line.contains("operation=submit stage=dependencies outcome=failed"));
        assert!(line.contains("category=Some(Infrastructure)"));
        assert!(line.contains(&format!("trace={}", trace.trace)));
        for private in ["provider-key", "secret", "private source", "command output"] {
            assert!(!line.contains(private));
        }
    }
}
