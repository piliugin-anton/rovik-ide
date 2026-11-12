use super::*;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn original_name() {}\n").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn main() { original_name(); }\n",
    )
    .unwrap();
    dir
}

#[test]
fn unchanged_source_reuses_the_graph_while_focus_changes() {
    let project = fixture();
    let options = Options::default();
    let cache = Mutex::new(GraphCache::default());
    let first = graph_with(project.path(), &options, &cache);
    let again = graph_with(project.path(), &options, &cache);
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(cache.lock().unwrap().builds, 1);
    let focused = super::super::Focus {
        files: vec!["main.rs".into()],
        words: vec!["original_name".into()],
    };
    assert!(super::super::render_map(&again, &focused, 6_000)
        .unwrap()
        .contains("original_name"));
    assert_eq!(cache.lock().unwrap().builds, 1);
}

#[test]
fn concurrent_requests_build_one_graph_for_the_same_revision() {
    let project = fixture();
    let cache = Arc::new(Mutex::new(GraphCache::default()));
    let start = Arc::new(std::sync::Barrier::new(8));
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let cache = cache.clone();
            let start = start.clone();
            let root = project.path().to_path_buf();
            std::thread::spawn(move || {
                start.wait();
                graph_with(&root, &Options::default(), &cache)
            })
        })
        .collect();
    let graphs: Vec<_> = requests
        .into_iter()
        .map(|request| request.join().unwrap())
        .collect();
    assert!(graphs.iter().all(|graph| Arc::ptr_eq(&graphs[0], graph)));
    assert_eq!(cache.lock().unwrap().builds, 1);
}

#[test]
fn changed_added_deleted_and_newly_ignored_files_change_the_graph() {
    let project = fixture();
    let root = project.path();
    let options = Options::default();
    let cache = Mutex::new(GraphCache::default());
    let first = graph_with(root, &options, &cache);
    fs::write(root.join("lib.rs"), "pub fn replacement_name() {}\n").unwrap();
    let changed = graph_with(root, &options, &cache);
    assert!(!Arc::ptr_eq(&first, &changed));
    assert!(!changed.declared_by.contains_key("original_name"));
    assert!(changed.declared_by.contains_key("replacement_name"));
    fs::write(root.join("added.rs"), "pub fn added_function() {}\n").unwrap();
    assert!(graph_with(root, &options, &cache)
        .declared_by
        .contains_key("added_function"));
    fs::remove_file(root.join("lib.rs")).unwrap();
    assert!(!graph_with(root, &options, &cache)
        .files
        .contains_key("lib.rs"));
    fs::write(root.join(".gitignore"), "added.rs\n").unwrap();
    assert!(!graph_with(root, &options, &cache)
        .files
        .contains_key("added.rs"));
    assert_eq!(cache.lock().unwrap().builds, 5);
}

#[test]
fn same_length_edits_with_preserved_mtime_refresh_cached_tags() {
    let project = fixture();
    let root = project.path();
    let file = root.join("lib.rs");
    let options = Options::default();
    let cache = Mutex::new(GraphCache::default());
    let first = graph_with(root, &options, &cache);
    let original = fs::metadata(&file).unwrap();
    fs::write(&file, "pub fn replaced_name() {}\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original.modified().unwrap()))
        .unwrap();
    assert_eq!(original.len(), fs::metadata(&file).unwrap().len());
    let changed = graph_with(root, &options, &cache);
    assert!(!Arc::ptr_eq(&first, &changed));
    assert!(changed.declared_by.contains_key("replaced_name"));
    assert!(!changed.declared_by.contains_key("original_name"));
}

#[test]
fn permission_changes_and_different_workspace_views_do_not_share_graphs() {
    let project = fixture();
    let other = fixture();
    let cache = Mutex::new(GraphCache::default());
    let options = Options::default();
    let broad = graph_with(project.path(), &options, &cache);
    let other_view = graph_with(other.path(), &options, &cache);
    assert!(!Arc::ptr_eq(&broad, &other_view));
    let restricted = Options {
        restricted: Arc::new(RestrictedFiles::from_settings(&serde_json::json!({
            "restrictedFiles": [{"path": "lib.rs", "blockRead": true}]
        }))),
        ..Options::default()
    };
    let narrow = graph_with(project.path(), &restricted, &cache);
    assert!(!narrow.files.contains_key("lib.rs"));
    assert!(!narrow.declared_by.contains_key("original_name"));
    let restored = graph_with(project.path(), &options, &cache);
    assert!(restored.declared_by.contains_key("original_name"));
    assert_eq!(cache.lock().unwrap().builds, 4);
}

#[test]
fn graph_retention_obeys_both_memory_and_root_limits() {
    let project = fixture();
    let graph = graph_with(project.path(), &Options::default(), &Mutex::default());
    let bytes = graph_bytes(&graph);
    let entry = |root: &str| Entry {
        root: root.into(),
        graph: graph.clone(),
        restricted: Arc::default(),
        bytes,
    };
    let mut cache = GraphCache::default();
    cache.insert(entry("first"), bytes * 2, 8);
    cache.insert(entry("second"), bytes * 2, 8);
    cache.insert(entry("third"), bytes * 2, 8);
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(cache.entries[0].root, Path::new("second"));
    assert_eq!(cache.bytes, bytes * 2);
    cache.insert(entry("fourth"), bytes * 4, 1);
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.entries[0].root, Path::new("fourth"));
    cache.insert(entry("oversized"), bytes - 1, 8);
    assert!(cache
        .entries
        .iter()
        .all(|entry| entry.root != Path::new("oversized")));
}

#[test]
fn invalidation_does_not_wait_for_a_graph_preparation() {
    let project = fixture();
    let root = project.path().to_path_buf();
    let preparing = preparing(&root);
    let guard = preparing.gate.lock().unwrap();
    let (finished, waiting) = std::sync::mpsc::channel();
    let invalidator = std::thread::spawn(move || {
        super::super::invalidate_path(&root, &root.join("lib.rs"));
        finished.send(()).unwrap();
    });
    let result = waiting.recv_timeout(std::time::Duration::from_secs(2));
    drop(guard);
    invalidator.join().unwrap();
    assert!(result.is_ok(), "invalidation waited for preparation");
    assert_eq!(preparing.generation(), 1);
}

#[test]
fn invalidated_preparation_does_not_repopulate_the_parse_cache() {
    let project = fixture();
    let root = project.path();
    let preparing = preparing(root);
    let generation = preparing.generation();
    super::super::invalidate_path(root, &root.join("lib.rs"));
    let files = collect(root, &Options::default(), &preparing, generation);
    assert!(!files.is_empty());
    let cache = super::super::CACHE.lock().unwrap();
    assert!(cache.iter().all(|(cached_root, _)| cached_root != root));
}
