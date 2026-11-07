//! Optional local semantic retrieval. Shadow mode observes rankings without
//! changing the active answer; failures always preserve lexical retrieval.
use std::path::{Path, PathBuf};
use verified_code_memory::{
    embedding::LocalSentenceEmbedder, semantic::SemanticIndex, RetrievalQuery, RetrievedMemory,
    SharedMemoryStore,
};

struct Config {
    embedder: std::sync::Arc<LocalSentenceEmbedder>,
    revision: String,
    dimensions: usize,
    threshold: f32,
    active: bool,
}

fn parse_config(config_dir: &Path, repo: &str) -> Option<Config> {
    let settings = crate::project_profile::load_project_settings(config_dir, repo);
    if !crate::memory::load(config_dir, repo).enabled {
        return None;
    }
    let value = settings.get("memorySemantic")?;
    let mode = value.get("mode")?.as_str()?;
    if !matches!(mode, "shadow" | "hybrid") {
        return None;
    }
    let dimensions = usize::try_from(value.get("dimensions")?.as_u64()?).ok()?;
    let revision = value.get("revision")?.as_str()?.to_owned();
    let threshold = value.get("threshold")?.as_f64()? as f32;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) || revision.is_empty() {
        return None;
    }
    use std::sync::{Arc, Mutex, OnceLock};
    type Cached = Option<(String, Arc<LocalSentenceEmbedder>)>;
    static BACKEND: OnceLock<Mutex<Cached>> = OnceLock::new();
    let key = value.to_string();
    let mut cache = BACKEND.get_or_init(Default::default).lock().ok()?;
    let embedder = if let Some((_, backend)) = cache.as_ref().filter(|(stored, _)| stored == &key) {
        backend.clone()
    } else {
        let backend = Arc::new(
            LocalSentenceEmbedder::new(
                PathBuf::from(value.get("python")?.as_str()?),
                PathBuf::from(value.get("modelPath")?.as_str()?),
                dimensions,
            )
            .ok()?,
        );
        *cache = Some((key, backend.clone()));
        backend
    };
    Some(Config {
        embedder,
        revision,
        dimensions,
        threshold,
        active: mode == "hybrid",
    })
}

fn config(config_dir: &Path, repo: &str) -> Option<Config> {
    let result = parse_config(config_dir, repo);
    if result.is_none() && crate::memory::load(config_dir, repo).enabled {
        let settings = crate::project_profile::load_project_settings(config_dir, repo);
        if settings
            .get("memorySemantic")
            .and_then(|s| s.get("mode"))
            .and_then(|m| m.as_str())
            .is_some_and(|mode| matches!(mode, "hybrid" | "shadow"))
        {
            eprintln!("memory: incomplete local semantic configuration; using lexical retrieval");
        }
    }
    result
}

/// Retrieves the active policy with lexical fallback; caller enforces visibility.
pub(crate) fn retrieve(
    store: &SharedMemoryStore,
    query: &RetrievalQuery,
    config_dir: &Path,
    settings_root: &str,
    accept: impl Fn(&verified_code_memory::MemoryRecord) -> bool,
) -> verified_code_memory::Result<Vec<RetrievedMemory>> {
    if !crate::memory::load(config_dir, settings_root).enabled {
        return Ok(Vec::new());
    }
    let baseline = store.retrieve_filtered(query, &accept)?;
    let Some(config) = config(config_dir, settings_root) else {
        return Ok(baseline);
    };
    let proposed =
        SemanticIndex::new(store.clone(), &config.revision, config.dimensions).and_then(|index| {
            index.retrieve_filtered(query, config.embedder.as_ref(), config.threshold, &accept)
        });
    match proposed {
        Ok(hits) => {
            let overlap = hits
                .iter()
                .filter(|hit| baseline.iter().any(|base| base.record.id == hit.record.id))
                .count();
            eprintln!(
                "memory: semantic mode={} lexical={} proposed={} overlap={overlap}",
                if config.active { "hybrid" } else { "shadow" },
                baseline.len(),
                hits.len()
            );
            if config.active {
                Ok(hits)
            } else {
                Ok(baseline)
            }
        }
        Err(error) => {
            eprintln!("memory: semantic retrieval unavailable; using lexical retrieval: {error}");
            Ok(baseline)
        }
    }
}

/// Backfills a small batch off the response path. Only one job per project may
/// run at once. Generation checks in the index reject deleted/cleared claims.
pub(crate) fn schedule(store: SharedMemoryStore, repo: String, config_dir: PathBuf) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Some(config) = config(&config_dir, &repo) else {
        return;
    };
    let running = RUNNING.get_or_init(Default::default);
    let Ok(mut jobs) = running.lock() else {
        return;
    };
    if !jobs.insert(repo.clone()) {
        return;
    }
    drop(jobs);
    crate::rt::spawn_blocking(move || {
        // Always release admission, including a backend panic.
        struct Job<'a>(&'a Mutex<HashSet<String>>, String);
        impl Drop for Job<'_> {
            fn drop(&mut self) {
                if let Ok(mut jobs) = self.0.lock() {
                    jobs.remove(&self.1);
                }
            }
        }
        let _job = Job(running, repo.clone());
        if !crate::memory::load(&config_dir, &repo).enabled {
            return;
        }
        let result = SemanticIndex::new(store, &config.revision, config.dimensions)
            .and_then(|index| index.backfill_limited(&repo, config.embedder.as_ref(), 3));
        if let Err(error) = result {
            eprintln!("memory: semantic backfill failed: {error}");
        }
    });
}
