//! Optional local semantic retrieval. Shadow mode observes rankings without
//! changing the active answer; failures always preserve lexical retrieval.
mod model;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use verified_code_memory::{
    embedding::{OnnxMiniLm, MINILM_DIMENSIONS, MINILM_REVISION},
    semantic::SemanticIndex,
    RetrievalQuery, RetrievedMemory, SharedMemoryStore,
};

#[derive(Clone, Debug, PartialEq)]
struct Config {
    directory: PathBuf,
    threshold: f32,
    short_scale: f32,
    active: bool,
}

type Cached = Option<(PathBuf, Arc<OnnxMiniLm>)>;
static BACKEND: OnceLock<Mutex<Cached>> = OnceLock::new();

fn cached(directory: &Path) -> Option<Arc<OnnxMiniLm>> {
    BACKEND
        .get_or_init(Default::default)
        .try_lock()
        .ok()?
        .as_ref()
        .filter(|(path, _)| path == directory)
        .map(|(_, backend)| backend.clone())
}

fn parse_config(config_dir: &Path, repo: &str) -> Option<Config> {
    if !crate::memory::load(config_dir, repo).enabled {
        return None;
    }
    let settings = crate::project_profile::load_project_settings(config_dir, repo);
    settings_config(config_dir, settings.get("memorySemantic")?)
}

fn settings_config(config_dir: &Path, value: &serde_json::Value) -> Option<Config> {
    let mode = value.get("mode")?.as_str()?;
    if !matches!(mode, "shadow" | "hybrid") {
        return None;
    }
    if let Some(dimensions) = value.get("dimensions") {
        if dimensions.as_u64()? != MINILM_DIMENSIONS as u64 {
            return None;
        }
    }
    let threshold = value
        .get("threshold")
        .map_or(Some(0.3), |value| value.as_f64())? as f32;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return None;
    }
    let short_scale = value
        .get("shortQueryScale")
        .map_or(Some(11.0 / 12.0), |value| value.as_f64())? as f32;
    if !short_scale.is_finite() || !(0.0..=1.0).contains(&short_scale) {
        return None;
    }
    let directory = match value.get("modelPath") {
        Some(path) => PathBuf::from(path.as_str()?),
        None => config_dir.join("memory-models").join(MINILM_REVISION),
    };
    if !directory.is_absolute() {
        return None;
    }
    Some(Config {
        directory,
        threshold,
        short_scale,
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
            eprintln!("memory: invalid native semantic configuration; using lexical retrieval");
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
    let Some(embedder) = cached(&config.directory) else {
        schedule(store.clone(), query.repo.clone(), config_dir.to_path_buf());
        return Ok(baseline);
    };
    let proposed = SemanticIndex::new(store.clone(), MINILM_REVISION, MINILM_DIMENSIONS)
        .and_then(|index| index.with_short_query_scale(2, config.short_scale))
        .and_then(|index| {
            index.retrieve_filtered(query, embedder.as_ref(), config.threshold, &accept)
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

/// Prepares the native model and backfills off the response path. One job per
/// model directory runs at a time; failures retry after one minute.
pub(crate) fn schedule(store: SharedMemoryStore, repo: String, config_dir: PathBuf) {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    static JOBS: OnceLock<Mutex<HashMap<PathBuf, Option<Instant>>>> = OnceLock::new();
    let Some(config) = config(&config_dir, &repo) else {
        return;
    };
    let jobs = JOBS.get_or_init(Default::default);
    let Ok(mut state) = jobs.lock() else {
        return;
    };
    if state
        .get(&config.directory)
        .is_some_and(|retry| retry.is_none_or(|at| at > Instant::now()))
    {
        return;
    }
    state.insert(config.directory.clone(), None);
    drop(state);
    crate::rt::spawn(async move {
        struct Job<'a>(&'a Mutex<HashMap<PathBuf, Option<Instant>>>, PathBuf, bool);
        impl Drop for Job<'_> {
            fn drop(&mut self) {
                if let Ok(mut state) = self.0.lock() {
                    if self.2 {
                        state.remove(&self.1);
                    } else {
                        state.insert(
                            self.1.clone(),
                            Some(Instant::now() + Duration::from_secs(60)),
                        );
                    }
                }
            }
        }
        let mut job = Job(jobs, config.directory.clone(), false);
        let result = async {
            let embedder = if let Some(backend) = cached(&config.directory) {
                backend
            } else {
                model::install(&config.directory).await?;
                let path = config.directory.clone();
                crate::rt::spawn_blocking(move || {
                    model::check(&path)?;
                    let backend = Arc::new(OnnxMiniLm::new(&path).map_err(|e| e.to_string())?);
                    *BACKEND
                        .get_or_init(Default::default)
                        .lock()
                        .map_err(|e| e.to_string())? = Some((path, backend.clone()));
                    Ok::<_, String>(backend)
                })
                .await
                .map_err(|e| e.to_string())??
            };
            // Settings may have changed while downloading or compiling the model.
            if parse_config(&config_dir, &repo).as_ref() != Some(&config) {
                return Ok(());
            }
            crate::rt::spawn_blocking(move || {
                if !crate::memory::load(&config_dir, &repo).enabled {
                    return Ok(());
                }
                SemanticIndex::new(store, MINILM_REVISION, MINILM_DIMENSIONS)
                    .and_then(|index| index.backfill_limited(&repo, embedder.as_ref(), 3))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())?
        }
        .await;
        match result {
            Ok(()) => job.2 = true,
            Err(error) => eprintln!(
                "memory: native semantic preparation failed; using lexical retrieval: {error}"
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_settings_need_no_python_or_operator_model_path() {
        let dir = crate::testing::temp_dir();
        let config = settings_config(&dir.0, &json!({"mode":"shadow"})).unwrap();
        assert_eq!(
            config.directory,
            dir.0.join("memory-models").join(MINILM_REVISION)
        );
        assert_eq!(config.threshold, 0.3);
        assert_eq!(config.short_scale, 11.0 / 12.0);
        assert!(!config.active);
        assert!(
            settings_config(&dir.0, &json!({"mode":"hybrid"}))
                .unwrap()
                .active
        );
    }

    #[test]
    fn projects_can_keep_a_strict_cutoff_for_short_queries() {
        let dir = crate::testing::temp_dir();
        let config =
            settings_config(&dir.0, &json!({"mode":"hybrid","shortQueryScale":1.0})).unwrap();
        assert_eq!(config.short_scale, 1.0);
        assert!(settings_config(&dir.0, &json!({"mode":"hybrid","shortQueryScale":1.1})).is_none());
    }

    #[test]
    fn disabled_and_invalid_native_settings_do_not_start_a_model_job() {
        let dir = crate::testing::temp_dir();
        for value in [
            json!({"mode":"off"}),
            json!({"mode":"hybrid","dimensions":300}),
            json!({"mode":"shadow","modelPath":"relative"}),
            json!({"mode":"shadow","threshold":1.5}),
        ] {
            assert!(settings_config(&dir.0, &value).is_none());
        }
    }
}
