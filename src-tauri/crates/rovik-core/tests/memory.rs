//! The project's long-term memory: one store per project, and the project's own switch.
//!
//! These use only what `rovik_core::memory` makes public: `load`, `store_path`,
//! `MemoryState` and `stats`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use rovik_core::core::Paths;
use rovik_core::memory::{self, MemorySettings, MemoryState};
use serde_json::json;
use verified_code_memory::{NewMemory, RetrievalQuery};

/// A folder of its own, removed when the test ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "rovik-memory-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

fn paths_of(dir: &Path) -> (Paths, PathBuf) {
    let data = dir.join("data");
    (Paths::new(data.clone(), dir.join("config")), data)
}

fn project(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn the_memory_of_two_projects_is_in_two_files() {
    let dir = temp_dir();
    let (paths, data) = paths_of(&dir.0);
    let first = project(&dir.0, "first");
    let second = project(&dir.0, "second");

    let first_path = memory::store_path(&paths, &first);
    let second_path = memory::store_path(&paths, &second);
    assert_eq!(
        first_path,
        memory::store_path(&paths, &first),
        "one project, one path"
    );
    assert_ne!(first_path, second_path, "two projects don't share a file");
    assert_eq!(
        first_path.parent(),
        Some(data.join("memory").as_path()),
        "the file lives in the app's data folder, not in the project"
    );
    assert_eq!(second_path.parent(), Some(data.join("memory").as_path()));
    assert_eq!(first_path.extension().and_then(|e| e.to_str()), Some("db"));
    assert!(
        !first_path.starts_with(&first),
        "nothing is written into the user's repository"
    );

    let state = MemoryState::default();
    let one = state.open(&paths, &first).expect("the first project opens");
    let memory = one
        .remember(
            NewMemory::episode(
                "first-project",
                "Alpha panics on an empty list",
                "Guard the empty list before indexing.",
            )
            .with_files(["src/a.rs"]),
        )
        .unwrap();

    let same = state.open(&paths, &first).expect("the cached store returns");
    let found = same
        .retrieve(&RetrievalQuery::new(
            "first-project",
            "Alpha panics on an empty list",
        ))
        .unwrap();
    assert!(
        found.iter().any(|m| m.record.id == memory.id),
        "the store opened twice for one project is the same file"
    );

    let other = state
        .open(&paths, &second)
        .expect("the second project opens");
    let elsewhere = other
        .retrieve(&RetrievalQuery::new(
            "first-project",
            "Alpha panics on an empty list",
        ))
        .unwrap();
    assert!(
        elsewhere.is_empty(),
        "one project's memory is not in another project's file"
    );
    assert_eq!(
        memory::stats(&other, "first-project").unwrap().total,
        0,
        "the second project's store counts none of the first's memories"
    );
    assert_eq!(
        memory::stats(&one, "first-project").unwrap().total,
        1,
        "the first project's store counts what was remembered in it"
    );
}

#[test]
fn a_project_that_never_chose_keeps_memory_on() {
    let dir = temp_dir();
    let config = dir.0.join("config");
    let root = project(&dir.0, "project");
    let settings_root = root.to_string_lossy().into_owned();

    assert!(
        MemorySettings::load(&config, &settings_root).enabled,
        "a project with no saved settings has memory on"
    );

    rovik_core::project_profile::save_project_settings(
        &config,
        settings_root.clone(),
        json!({ "memory": true }),
    )
    .unwrap();
    assert!(
        MemorySettings::load(&config, &settings_root).enabled,
        "a project that chose memory on keeps it on"
    );

    rovik_core::project_profile::save_project_settings(
        &config,
        settings_root.clone(),
        json!({ "memory": false }),
    )
    .unwrap();
    assert!(
        !MemorySettings::load(&config, &settings_root).enabled,
        "a project that switched memory off is off"
    );

    rovik_core::project_profile::save_project_settings(&config, settings_root, json!({})).unwrap();
    assert!(
        MemorySettings::load(&config, &settings_root).enabled,
        "settings saved without the key fall back to on"
    );
}

#[test]
fn a_store_that_cannot_be_opened_yields_nothing() {
    let dir = temp_dir();
    let data = dir.0.join("data");
    std::fs::write(&data, b"a file where the data folder should be").unwrap();
    let paths = Paths::new(data, dir.0.join("config"));
    let project = project(&dir.0, "project");

    let state = MemoryState::default();
    assert!(
        state.open(&paths, &project).is_none(),
        "a store that can't be opened yields nothing instead of stopping the run"
    );
}
