//! Shared structural graphs. Every lookup first collects the currently
//! visible files; a cache hit saves graph construction, never freshness checks.

use super::{build_graph, collect, Cache, FileTags, Graph, Options, CACHED_ROOTS, CACHE_BYTES};
use crate::restricted_files::RestrictedFiles;
use std::collections::{BTreeMap, HashMap};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::SystemTime;

/// A file version, including changes hidden by a preserved modified time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileStamp {
    modified: SystemTime,
    size: u64,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64, u32),
    #[cfg(not(unix))]
    content: [u8; 32],
}

#[cfg(unix)]
fn metadata_stamp(meta: &Metadata) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt;
    Some(FileStamp {
        modified: meta.modified().ok()?,
        size: meta.len(),
        identity: (
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec(),
            meta.mode(),
        ),
    })
}

/// Off Unix, checking content avoids trusting coarse or preserved timestamps.
/// The content is returned for parsing, so a cache miss does not read it twice.
pub(super) fn file_stamp(path: &Path, meta: &Metadata) -> Option<(FileStamp, Option<String>)> {
    #[cfg(unix)]
    {
        let _ = path;
        Some((metadata_stamp(meta)?, None))
    }
    #[cfg(not(unix))]
    {
        use sha2::{Digest, Sha256};
        let content = std::fs::read_to_string(path).ok()?;
        if !still_current(path, meta) {
            return None;
        }
        let stamp = FileStamp {
            modified: meta.modified().ok()?,
            size: meta.len(),
            content: Sha256::digest(content.as_bytes()).into(),
        };
        Some((stamp, Some(content)))
    }
}

pub(super) fn still_current(path: &Path, before: &Metadata) -> bool {
    let Ok(after) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        metadata_stamp(before).is_some_and(|stamp| Some(stamp) == metadata_stamp(&after))
    }
    #[cfg(not(unix))]
    {
        before.modified().ok().is_some_and(|modified| {
            Some(modified) == after.modified().ok() && before.len() == after.len()
        })
    }
}

// Weak entries only coordinate requests that are preparing now. They do not
// retain workspaces indefinitely; unrelated roots prepare independently.
pub(super) struct Workspace {
    gate: Mutex<()>,
    generation: AtomicU64,
}

impl Workspace {
    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

type Preparing = HashMap<PathBuf, Weak<Workspace>>;
static PREPARING: LazyLock<Mutex<Preparing>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn preparing(root: &Path) -> Arc<Workspace> {
    let mut roots = PREPARING.lock().unwrap_or_else(|e| e.into_inner());
    roots.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = roots.get(root).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Workspace {
        gate: Mutex::new(()),
        generation: AtomicU64::new(0),
    });
    roots.insert(root.to_path_buf(), Arc::downgrade(&lock));
    lock
}

struct Entry {
    root: PathBuf,
    restricted: Arc<RestrictedFiles>,
    graph: Arc<Graph>,
    bytes: usize,
}

#[derive(Default)]
struct GraphCache {
    entries: Vec<Entry>,
    bytes: usize,
    #[cfg(test)]
    builds: usize,
}

impl GraphCache {
    fn find(
        &mut self,
        root: &Path,
        options: &Options,
        files: &BTreeMap<String, Arc<FileTags>>,
    ) -> Option<Arc<Graph>> {
        let at = self.entries.iter().position(|entry| {
            entry.root == root
                && entry.restricted == options.restricted
                && entry.graph.files.len() == files.len()
                && entry
                    .graph
                    .files
                    .iter()
                    .zip(files)
                    .all(|((a, before), (b, now))| a == b && before.stamp == now.stamp)
        })?;
        let entry = self.entries.remove(at);
        let graph = entry.graph.clone();
        self.entries.push(entry);
        Some(graph)
    }

    fn remove(&mut self, root: &Path) {
        self.entries.retain(|entry| entry.root != root);
        self.bytes = self.entries.iter().map(|entry| entry.bytes).sum();
    }

    fn insert(&mut self, entry: Entry, budget: usize, roots: usize) {
        self.remove(&entry.root);
        // A single huge project still works, but its graph is not retained.
        if entry.bytes > budget || roots == 0 {
            return;
        }
        self.bytes += entry.bytes;
        self.entries.push(entry);
        while self.bytes > budget || self.entries.len() > roots {
            self.bytes -= self.entries.remove(0).bytes;
        }
    }
}

static GRAPHS: LazyLock<Mutex<GraphCache>> = LazyLock::new(|| Mutex::new(GraphCache::default()));

pub(super) fn graph(root: &Path, options: &Options) -> Arc<Graph> {
    graph_with(root, options, &GRAPHS)
}

fn graph_with(root: &Path, options: &Options, cache: &Mutex<GraphCache>) -> Arc<Graph> {
    let preparing = preparing(root);
    let _preparing = preparing.gate.lock().unwrap_or_else(|e| e.into_inner());
    let generation = preparing.generation();
    let files = collect(root, options, &preparing, generation);
    if let Some(graph) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .find(root, options, &files)
    {
        return graph;
    }
    let graph = Arc::new(build_graph(files));
    let bytes = graph_bytes(&graph);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(test)]
    {
        cache.builds += 1;
    }
    if preparing.generation() != generation {
        return graph;
    }
    cache.insert(
        Entry {
            root: root.to_path_buf(),
            restricted: options.restricted.clone(),
            graph: graph.clone(),
            bytes,
        },
        CACHE_BYTES,
        CACHED_ROOTS,
    );
    graph
}

pub(super) fn invalidate(root: &Path) {
    // A write never waits for a filesystem walk. In-flight preparations can
    // finish their snapshot, but may not publish it after this generation.
    preparing(root).generation.fetch_add(1, Ordering::AcqRel);
    GRAPHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(root);
}

fn tags_bytes(tags: &FileTags) -> usize {
    std::mem::size_of::<FileTags>()
        + tags.symbols.capacity() * std::mem::size_of::<crate::outline::Symbol>()
        + tags
            .symbols
            .iter()
            .map(|symbol| {
                symbol.name.capacity() + symbol.kind.capacity() + symbol.signature.capacity()
            })
            .sum::<usize>()
        + tags.identifiers.capacity() * (std::mem::size_of::<(String, u32)>() + 16)
        + tags.identifiers.keys().map(String::capacity).sum::<usize>()
}

pub(super) fn parsed_bytes(files: &Cache) -> usize {
    files.capacity() * (std::mem::size_of::<(PathBuf, Arc<FileTags>)>() + 16)
        + files
            .iter()
            .map(|(path, tags)| path.as_os_str().len() + tags_bytes(tags))
            .sum::<usize>()
}

fn graph_bytes(graph: &Graph) -> usize {
    std::mem::size_of::<Graph>()
        + graph
            .files
            .iter()
            .map(|(path, tags)| 96 + path.capacity() + tags_bytes(tags))
            .sum::<usize>()
        + graph.paths.capacity() * std::mem::size_of::<String>()
        + graph.paths.iter().map(String::capacity).sum::<usize>()
        + graph.scopes.capacity() * std::mem::size_of::<Vec<super::Scope>>()
        + graph
            .scopes
            .iter()
            .map(|scopes| scopes.capacity() * std::mem::size_of::<super::Scope>())
            .sum::<usize>()
        + graph.declared_by.capacity() * (std::mem::size_of::<(String, Vec<usize>)>() + 16)
        + graph
            .declared_by
            .iter()
            .map(|(name, files)| name.capacity() + files.capacity() * std::mem::size_of::<usize>())
            .sum::<usize>()
        + graph
            .links
            .keys()
            .map(|(_, _, name)| 96 + name.capacity())
            .sum::<usize>()
}

#[cfg(test)]
pub(super) fn clear() {
    *GRAPHS.lock().unwrap_or_else(|e| e.into_inner()) = GraphCache::default();
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
