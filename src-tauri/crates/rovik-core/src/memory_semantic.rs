//! Optional local semantic retrieval. Shadow mode observes rankings without
//! changing the active answer; failures always preserve lexical retrieval.
mod model;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
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
type JobKey = (PathBuf, String);
type Jobs = Mutex<HashMap<JobKey, Option<Instant>>>;
static JOBS: OnceLock<Jobs> = OnceLock::new();
// Separate queued projects share one encoder without competing for its
// nonblocking inference lock or downloading/compiling duplicate models.
static WORKER: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct BackgroundEncoder<'a>(&'a OnnxMiniLm);
impl verified_code_memory::MemoryEmbedder for BackgroundEncoder<'_> {
    fn dimensions(&self) -> usize {
        MINILM_DIMENSIONS
    }
    fn input_character_limit(&self) -> Option<usize> {
        verified_code_memory::MemoryEmbedder::input_character_limit(self.0)
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        self.0.embed_blocking(text)
    }
}

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
        schedule(
            store.clone(),
            query.repo.clone(),
            config_dir.to_path_buf(),
            settings_root.to_owned(),
        );
        return Ok(baseline);
    };
    let proposed = SemanticIndex::new(store.clone(), MINILM_REVISION, MINILM_DIMENSIONS)
        .and_then(|index| index.with_short_query_scale(2, config.short_scale))
        .and_then(|index| {
            index.retrieve_filtered(query, embedder.as_ref(), config.threshold, &accept)
        });
    // Catch existing stores and claims captured during another snapshot. Queue
    // after this query's inference so new work cannot take its encoder lock.
    schedule(
        store.clone(),
        query.repo.clone(),
        config_dir.to_path_buf(),
        settings_root.to_owned(),
    );
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

/// Queued preparation includes model downloads and gaps between blocking batches.
pub(crate) fn active() -> bool {
    JOBS.get().is_some_and(|jobs| {
        jobs.lock()
            .map_or(true, |jobs| jobs.values().any(Option::is_none))
    })
}

/// Prepares the native model and completes a project's pending snapshot off the
/// response path. Projects queue independently; failures retry after one minute.
pub(crate) fn schedule(
    store: SharedMemoryStore,
    repo: String,
    config_dir: PathBuf,
    settings_root: String,
) {
    let Some(config) = config(&config_dir, &settings_root) else {
        return;
    };
    let jobs = JOBS.get_or_init(Default::default);
    let key = (config.directory.clone(), settings_root.clone());
    let Ok(mut state) = jobs.lock() else {
        return;
    };
    if state
        .get(&key)
        .is_some_and(|retry| retry.is_none_or(|at| at > Instant::now()))
    {
        return;
    }
    state.insert(key.clone(), None);
    drop(state);
    crate::rt::spawn(async move {
        struct Job<'a>(&'a Jobs, JobKey, bool);
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
        let mut job = Job(jobs, key, false);
        let result = async {
            let _worker = WORKER.get_or_init(Default::default).lock().await;
            // Queued work may have been disabled before acquiring the encoder.
            if parse_config(&config_dir, &settings_root).as_ref() != Some(&config) {
                return Ok(());
            }
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
            if parse_config(&config_dir, &settings_root).as_ref() != Some(&config) {
                return Ok(());
            }
            crate::rt::spawn_blocking(move || {
                backfill(
                    store,
                    &repo,
                    &config_dir,
                    &settings_root,
                    &config,
                    &BackgroundEncoder(embedder.as_ref()),
                )
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

fn backfill(
    store: SharedMemoryStore,
    repo: &str,
    config_dir: &Path,
    settings_root: &str,
    config: &Config,
    embedder: &dyn verified_code_memory::MemoryEmbedder,
) -> verified_code_memory::Result<usize> {
    SemanticIndex::new(store, MINILM_REVISION, MINILM_DIMENSIONS)?.backfill_while(
        repo,
        embedder,
        || parse_config(config_dir, settings_root).as_ref() == Some(config),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Disabling {
        directory: PathBuf,
        calls: std::sync::atomic::AtomicUsize,
        replacement: serde_json::Value,
    }
    impl verified_code_memory::MemoryEmbedder for Disabling {
        fn dimensions(&self) -> usize {
            MINILM_DIMENSIONS
        }
        fn embed(&self, _: &str) -> verified_code_memory::Result<Vec<f32>> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
                crate::project_profile::save_project_settings(
                    &self.directory,
                    "project".into(),
                    self.replacement.clone(),
                )
                .unwrap();
            }
            Ok(vec![1.0; MINILM_DIMENSIONS])
        }
    }

    #[test]
    fn background_completion_stops_when_memory_or_semantic_settings_change() {
        for replacement in [
            json!({"memory":false,"memorySemantic":{"mode":"hybrid"}}),
            json!({"memorySemantic":{"mode":"off"}}),
            json!({"memorySemantic":{"mode":"hybrid","threshold":0.4}}),
        ] {
            let dir = crate::testing::temp_dir();
            crate::project_profile::save_project_settings(
                &dir.0,
                "project".into(),
                json!({"memorySemantic":{"mode":"hybrid"}}),
            )
            .unwrap();
            let original = parse_config(&dir.0, "project").unwrap();
            let store = SharedMemoryStore::open_in_memory().unwrap();
            for n in 0..12 {
                store
                    .remember(verified_code_memory::NewMemory::procedure(
                        "shared-origin",
                        format!("lock-{n}"),
                        format!("lock procedure {n}"),
                    ))
                    .unwrap();
            }
            let embedder = Disabling {
                directory: dir.0.clone(),
                calls: Default::default(),
                replacement,
            };
            assert_eq!(
                backfill(
                    store.clone(),
                    "shared-origin",
                    &dir.0,
                    "project",
                    &original,
                    &embedder
                )
                .unwrap(),
                3
            );
            assert_eq!(embedder.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
            assert_eq!(
                backfill(
                    store.clone(),
                    "shared-origin",
                    &dir.0,
                    "project",
                    &original,
                    &embedder
                )
                .unwrap(),
                0
            );
            let index = SemanticIndex::new(store, MINILM_REVISION, MINILM_DIMENSIONS).unwrap();
            assert_eq!(
                index
                    .search(
                        &RetrievalQuery::new("shared-origin", "lock").with_limit(32),
                        &vec![1.0; MINILM_DIMENSIONS],
                        0.3
                    )
                    .unwrap()
                    .len(),
                3
            );
        }
    }

    async fn wait_for_indexing(model: &Path, stores: &[(&str, SharedMemoryStore)]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let complete = {
                let pending = JOBS.get().unwrap().lock().unwrap();
                assert!(pending.values().all(Option::is_none), "indexing job failed");
                stores.iter().all(|(repo, _)| {
                    !pending.contains_key(&(model.to_path_buf(), repo.to_string()))
                })
            };
            if complete {
                break;
            }
            assert!(Instant::now() < deadline, "indexing did not complete");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    #[ignore = "requires ROVIK_REAL_MEMORY_INDEX=1 and a pinned MiniLM directory"]
    async fn cloned_projects_complete_independent_jobs_using_local_settings_and_one_native_model() {
        if std::env::var("ROVIK_REAL_MEMORY_INDEX").as_deref() != Ok("1") {
            return;
        }
        let model = PathBuf::from(std::env::var("ROVIK_MEMORY_EVAL_MODEL").unwrap());
        let dir = crate::testing::temp_dir();
        let worker = WORKER.get_or_init(Default::default).lock().await;
        let mut stores = Vec::new();
        for repo in ["project-a", "project-b"] {
            crate::project_profile::save_project_settings(
                &dir.0,
                repo.into(),
                json!({"memorySemantic":{"mode":"hybrid","modelPath":model}}),
            )
            .unwrap();
            let store = SharedMemoryStore::open_in_memory().unwrap();
            for n in 0..12 {
                store
                    .remember(verified_code_memory::NewMemory::procedure(
                        "shared-origin",
                        format!("lock-{n}"),
                        format!("Serialize retries for order {n}."),
                    ))
                    .unwrap();
            }
            store
                .remember(verified_code_memory::NewMemory::procedure(
                    "shared-origin",
                    "Long investigation",
                    format!(
                        "{}\n\nStore timestamps in UTC and localize at the display boundary.",
                        "Session setup and preliminary observations. ".repeat(16)
                    ),
                ))
                .unwrap();
            schedule(
                store.clone(),
                "shared-origin".into(),
                dir.0.clone(),
                repo.into(),
            );
            stores.push((repo, store));
        }
        for (repo, _) in &stores {
            assert!(JOBS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key(&(model.clone(), repo.to_string())));
        }
        drop(worker);
        wait_for_indexing(&model, &stores).await;
        struct NoInference;
        impl verified_code_memory::MemoryEmbedder for NoInference {
            fn dimensions(&self) -> usize {
                MINILM_DIMENSIONS
            }
            fn input_character_limit(&self) -> Option<usize> {
                Some(500)
            }
            fn embed(&self, _: &str) -> verified_code_memory::Result<Vec<f32>> {
                panic!("a claim remained unindexed after its only scheduled job")
            }
        }
        for (repo, store) in &stores {
            assert_eq!(
                SemanticIndex::new(store.clone(), MINILM_REVISION, MINILM_DIMENSIONS)
                    .unwrap()
                    .backfill("shared-origin", &NoInference)
                    .unwrap(),
                0
            );
            store
                .remember(verified_code_memory::NewMemory::procedure(
                    "shared-origin",
                    "late snapshot claim",
                    "Protect shared state with a mutex.",
                ))
                .unwrap();
            // No capture hook: warm retrieval itself must schedule the new claim.
            retrieve(
                store,
                &RetrievalQuery::new("shared-origin", "Prevent simultaneous access"),
                &dir.0,
                repo,
                |_| true,
            )
            .unwrap();
        }
        wait_for_indexing(&model, &stores).await;
        for (_, store) in stores {
            assert_eq!(
                SemanticIndex::new(store, MINILM_REVISION, MINILM_DIMENSIONS)
                    .unwrap()
                    .backfill("shared-origin", &NoInference)
                    .unwrap(),
                0
            );
        }
    }

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
