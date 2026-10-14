//! What commands cost when they ran before, per project.
//!
//! The best guess of whether a command will be heavy is what the same
//! command cost last time. Each finished command is recorded under a
//! fingerprint (its program, subcommand and flags, without paths, test
//! names or file names), with the CPU time, peak memory and wall time the
//! tracker measured. From three runs on, the 75th percentile overrides the
//! rules in classify.rs.
//!
//! Kept in one small JSON file in the app's data folder (not the chat
//! database: it's about the machine and the project, not a chat).

use super::classify::Class;
use crate::agent_run::permissions::parse_command;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Runs kept per fingerprint, newest last.
const SAMPLES: usize = 20;
/// Fingerprints kept per project; the least recently used go first.
const PER_PROJECT: usize = 500;
/// Runs before history overrides the rules.
const MIN_SAMPLES: usize = 3;

/// Thresholds for "heavy", on the 75th percentile of past runs.
pub const HEAVY_CPU_MS: u64 = 15_000;
pub const HEAVY_CORES: f64 = 1.5;
pub const HEAVY_MEMORY: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub cpu_ms: u64,
    pub peak_memory: u64,
    pub wall_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Entry {
    samples: VecDeque<Sample>,
    /// A counter, not a clock: which entries were used last.
    last_used: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    tick: u64,
    projects: HashMap<String, HashMap<String, Entry>>,
}

struct History {
    file: Option<PathBuf>,
    store: Option<Store>,
    /// When it was last written: at most every SAVE_EVERY.
    saved_at: Option<std::time::Instant>,
}

static HISTORY: LazyLock<Mutex<History>> = LazyLock::new(|| {
    Mutex::new(History {
        file: None,
        store: None,
        saved_at: None,
    })
});

/// History is written at most this often (and at exit).
const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);
/// Projects remembered; the least recently used go first.
const PROJECTS: usize = 50;

/// Where history is saved. Without it (tests), it's kept in memory only.
pub fn init(data_dir: &Path) {
    let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    h.file = Some(data_dir.join("workload-history.json"));
    h.store = None;
}

impl History {
    fn store(&mut self) -> &mut Store {
        if self.store.is_none() {
            let loaded = self
                .file
                .as_ref()
                .and_then(|f| std::fs::read(f).ok())
                .and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok())
                .filter(|s| s.version == 1);
            self.store = Some(loaded.unwrap_or(Store {
                version: 1,
                ..Default::default()
            }));
        }
        self.store.as_mut().unwrap()
    }

    fn save(&mut self) {
        if let (Some(file), Some(store)) = (&self.file, &self.store) {
            if let Ok(bytes) = serde_json::to_vec(store) {
                let _ = crate::atomic_file::write_atomic(file, &bytes);
            }
        }
        self.saved_at = Some(std::time::Instant::now());
    }
}

/// Writes what isn't saved yet (at exit).
pub fn flush() {
    let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if h.store.is_some() {
        h.save();
    }
}

fn looks_like_a_path(word: &str) -> bool {
    word.contains(['/', '\\'])
        || word.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// A command without what changes between runs of "the same" command:
/// paths, test names, file names. `cargo test -p foo some_test` →
/// `cargo test -p foo`; `npm run build -- --mode x` → `npm run build --mode`.
pub fn fingerprint(command: &str) -> String {
    let parsed = parse_command(command);
    let mut parts = Vec::new();
    for segment in &parsed.segments {
        let mut kept: Vec<&str> = Vec::new();
        let mut positional = 0;
        let keep_positional = match segment.first().map(String::as_str) {
            Some("npm" | "pnpm" | "yarn" | "bun" | "npx" | "just" | "task" | "cd") => 2,
            _ => 1,
        };
        let mut after_flag = false;
        for (i, word) in segment.iter().enumerate() {
            if i == 0 {
                kept.push(word);
                continue;
            }
            if word.starts_with('-') {
                kept.push(word.split('=').next().unwrap_or(word));
                // `-p foo`: a short flag's value names what runs.
                after_flag = word.len() == 2;
                continue;
            }
            if after_flag && !looks_like_a_path(word) {
                kept.push(word);
            } else if positional < keep_positional
                && (segment[0] == "cd" || !looks_like_a_path(word))
            {
                kept.push(word);
                positional += 1;
            }
            after_flag = false;
        }
        parts.push(kept.join(" "));
    }
    parts.join(" && ")
}

pub fn record(project: &str, fingerprint: &str, sample: Sample) {
    if fingerprint.is_empty() {
        return;
    }
    let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    let store = h.store();
    store.tick += 1;
    let tick = store.tick;
    let entries = store.projects.entry(project.to_string()).or_default();
    let entry = entries.entry(fingerprint.to_string()).or_default();
    entry.samples.push_back(sample);
    while entry.samples.len() > SAMPLES {
        entry.samples.pop_front();
    }
    entry.last_used = tick;
    if entries.len() > PER_PROJECT {
        if let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
    }
    if store.projects.len() > PROJECTS {
        let last_used = |entries: &HashMap<String, Entry>| {
            entries.values().map(|e| e.last_used).max().unwrap_or(0)
        };
        if let Some(oldest) = store
            .projects
            .iter()
            .min_by_key(|(_, e)| last_used(e))
            .map(|(k, _)| k.clone())
        {
            store.projects.remove(&oldest);
        }
    }
    if h.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) {
        h.save();
    }
}

fn p75(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    let i = ((values.len() as f64 - 1.0) * 0.75).round() as usize;
    values[i.min(values.len() - 1)]
}

/// What past runs say about the command, from MIN_SAMPLES runs on.
pub struct Prediction {
    pub class: Class,
    /// The usual (75th percentile) peak memory.
    pub peak_memory: u64,
}

pub fn predict(project: &str, fingerprint: &str) -> Option<Prediction> {
    let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    let samples = h
        .store()
        .projects
        .get(project)?
        .get(fingerprint)?
        .samples
        .iter()
        .copied()
        .collect::<Vec<_>>();
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let cpu = p75(samples.iter().map(|s| s.cpu_ms as f64).collect());
    let cores = p75(samples
        .iter()
        .map(|s| s.cpu_ms as f64 / s.wall_ms.max(1) as f64)
        .collect());
    let memory = p75(samples.iter().map(|s| s.peak_memory as f64).collect());
    let heavy = cpu > HEAVY_CPU_MS as f64 || cores > HEAVY_CORES || memory > HEAVY_MEMORY as f64;
    Some(Prediction {
        class: if heavy { Class::Heavy } else { Class::Light },
        peak_memory: memory as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_drop_what_changes_between_runs() {
        assert_eq!(
            fingerprint("cargo test -p foo some_test"),
            "cargo test -p foo"
        );
        assert_eq!(
            fingerprint("cargo test -p foo other_test"),
            "cargo test -p foo"
        );
        assert_eq!(
            fingerprint("cargo build --release"),
            "cargo build --release"
        );
        assert_eq!(
            fingerprint("pytest tests/test_api.py -k login"),
            "pytest -k login"
        );
        assert_eq!(
            fingerprint("npm run build -- --mode=staging"),
            "npm run build -- --mode"
        );
        assert_eq!(
            fingerprint("cd web && npx tsc --noEmit"),
            "cd web && npx tsc --noEmit"
        );
        assert_eq!(fingerprint("RUST_LOG=debug cargo check"), "cargo check");
        assert_eq!(fingerprint("rustfmt src/main.rs"), "rustfmt");
    }

    #[test]
    fn history_overrides_after_three_runs() {
        let project = format!("/test/history-{}", std::process::id());
        let light = Sample {
            cpu_ms: 300,
            peak_memory: 50 << 20,
            wall_ms: 1_000,
        };
        let heavy = Sample {
            cpu_ms: 90_000,
            peak_memory: 2 << 30,
            wall_ms: 30_000,
        };
        record(&project, "cargo build", heavy);
        record(&project, "cargo build", heavy);
        assert!(predict(&project, "cargo build").is_none());
        record(&project, "cargo build", light);
        let p = predict(&project, "cargo build").unwrap();
        assert_eq!(p.class, Class::Heavy);
        assert_eq!(p.peak_memory, 2 << 30);
        // Mostly no-op builds now: the 75th percentile says light.
        for _ in 0..8 {
            record(&project, "cargo build", light);
        }
        assert_eq!(
            predict(&project, "cargo build").unwrap().class,
            Class::Light
        );
    }

    #[test]
    fn many_cores_for_a_short_time_is_heavy() {
        let project = format!("/test/cores-{}", std::process::id());
        let burst = Sample {
            cpu_ms: 8_000,
            peak_memory: 1 << 20,
            wall_ms: 2_000,
        };
        for _ in 0..3 {
            record(&project, "make -j", burst);
        }
        assert_eq!(predict(&project, "make -j").unwrap().class, Class::Heavy);
    }
}
