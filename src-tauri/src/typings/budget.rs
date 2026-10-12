//! Limits on what a walk may load for the editor. The loaded set is sent to
//! the webview as one payload and kept there, in the main thread and in the
//! TypeScript worker, so it must have a ceiling: beyond it the editor loses
//! some definitions instead of all of its memory.

use super::{LoadedFile, LoadedFiles};
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Files beyond this many aren't loaded.
    pub max_files: usize,
    /// A bigger file is skipped on its own: usually generated or bundled.
    pub max_file_bytes: u64,
    /// Once the loaded bytes would pass this, nothing more is loaded.
    pub max_total_bytes: u64,
}

/// Whether a walk should go on after `Collector::add`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    Continue,
    Stop,
}

/// Collects files within `Limits`: what a walk feeds its candidates to.
pub struct Collector {
    limits: Limits,
    files: Vec<LoadedFile>,
    total: u64,
    truncated: bool,
}

impl Collector {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            files: Vec::new(),
            total: 0,
            truncated: false,
        }
    }

    /// Loads `path` (`len` bytes) unless a limit says otherwise: an oversize
    /// file is skipped on its own, a limit on the set stops the walk. A file
    /// that can't be read as text (binary, vanished meanwhile) is skipped.
    pub fn add(&mut self, path: &Path, len: u64) -> Walk {
        if len > self.limits.max_file_bytes {
            return Walk::Continue;
        }
        if self.files.len() >= self.limits.max_files
            || self.total + len > self.limits.max_total_bytes
        {
            self.truncated = true;
            return Walk::Stop;
        }
        if let Ok(content) = fs::read_to_string(path) {
            self.total += len;
            self.files.push(LoadedFile {
                path: path.to_string_lossy().into_owned(),
                content,
            });
        }
        Walk::Continue
    }

    pub fn finish(self) -> LoadedFiles {
        LoadedFiles {
            files: self.files,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::{relative_paths, temp_project};

    const SMALL: Limits = Limits {
        max_files: 2,
        max_file_bytes: 5,
        max_total_bytes: 8,
    };

    #[test]
    fn skips_oversize_files_and_stops_at_the_count_limit() {
        let dir = temp_project(
            "budget-count",
            &[("a", "aaa"), ("big", "toolong"), ("c", "ccc"), ("d", "d")],
        );
        let mut collector = Collector::new(SMALL);
        assert_eq!(collector.add(&dir.join("a"), 3), Walk::Continue);
        assert_eq!(collector.add(&dir.join("big"), 7), Walk::Continue);
        assert_eq!(collector.add(&dir.join("c"), 3), Walk::Continue);
        assert_eq!(collector.add(&dir.join("d"), 1), Walk::Stop);
        let loaded = collector.finish();
        assert!(loaded.truncated);
        assert_eq!(relative_paths(&dir, &loaded), ["a", "c"]);
    }

    #[test]
    fn stops_before_passing_the_byte_limit() {
        let dir = temp_project("budget-bytes", &[("a", "aaaaa"), ("b", "bbbb")]);
        let mut collector = Collector::new(SMALL);
        assert_eq!(collector.add(&dir.join("a"), 5), Walk::Continue);
        assert_eq!(collector.add(&dir.join("b"), 4), Walk::Stop);
        let loaded = collector.finish();
        assert!(loaded.truncated);
        assert_eq!(relative_paths(&dir, &loaded), ["a"]);
    }

    #[test]
    fn unreadable_files_are_skipped_without_truncating() {
        let dir = temp_project("budget-unreadable", &[("a", "aaa")]);
        let mut collector = Collector::new(SMALL);
        assert_eq!(collector.add(&dir.join("missing"), 1), Walk::Continue);
        assert_eq!(collector.add(&dir.join("a"), 3), Walk::Continue);
        let loaded = collector.finish();
        assert!(!loaded.truncated);
        assert_eq!(relative_paths(&dir, &loaded), ["a"]);
    }
}
